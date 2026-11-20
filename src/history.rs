//! Reading of the CLI's own session history (`~/.claude/projects/*/*.jsonl`).
//!
//! [`scan_history`] builds a cheap, sorted list of past sessions;
//! [`load_session`] turns one file into transcript [`Item`]s so it can be shown
//! and then resumed with `--resume <session_id>`. Parsing is tolerant: lines
//! that are not valid JSON or have unexpected shapes are skipped.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local};
use serde_json::Value;

use crate::protocol::{AttachmentSummary, ContentBlock, parse_content};
use crate::state::{Item, NoteKind, truncate_title};

/// Max length of a history title.
const HISTORY_TITLE_MAX: usize = 80;

/// One past session found on disk.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// Session id (file name without extension).
    pub session_id: String,
    /// Real working directory read from the file (the folder name is lossy).
    pub cwd: Option<String>,
    /// `ai-title`, else first real user prompt, else last prompt.
    pub title: String,
    /// Last modification of the file.
    pub modified: DateTime<Local>,
    /// Approximate number of user + assistant lines (includes tool results).
    pub approx_messages: usize,
    /// Path of the `.jsonl` file.
    pub path: PathBuf,
}

/// A fully loaded session.
#[derive(Debug, Clone, Default)]
pub struct LoadedSession {
    pub session_id: String,
    pub cwd: Option<String>,
    pub title: String,
    /// Model of the last assistant message.
    pub model: Option<String>,
    pub items: Vec<Item>,
}

/// `~/.claude/projects`, if the home directory is known.
pub fn default_projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// True if `cwd` looks like a throwaway folder used for tests: under the
/// system temp dir, under any `target/tmp`, or with a `sandbox` path segment.
pub fn is_test_cwd(cwd: &str) -> bool {
    let norm = |s: &str| s.to_lowercase().replace('/', "\\");
    let c = norm(cwd);
    let temp = norm(&std::env::temp_dir().display().to_string());
    let temp = temp.trim_end_matches('\\');
    if !temp.is_empty() && (c == temp || c.starts_with(&format!("{temp}\\"))) {
        return true;
    }
    let segs: Vec<&str> = c.split('\\').collect();
    segs.contains(&"sandbox") || segs.windows(2).any(|w| w[0] == "target" && w[1] == "tmp")
}

/// Scans `projects_dir/*/*.jsonl` (not the `subagents/` subfolders) and
/// returns up to `limit` entries ordered by modification time, newest first.
///
/// Only the newest `limit` files are opened; each is streamed line by line
/// without keeping it in memory.
pub fn scan_history(projects_dir: &Path, limit: usize) -> Vec<HistoryEntry> {
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    let Ok(dirs) = std::fs::read_dir(projects_dir) else {
        return Vec::new();
    };
    for d in dirs.flatten() {
        let p = d.path();
        if !p.is_dir() {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for f in rd.flatten() {
            let fp = f.path();
            if fp.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = f.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((mtime, fp));
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.truncate(limit);
    files
        .into_iter()
        .filter_map(|(mtime, path)| scan_file(&path, mtime))
        .collect()
}

/// Permanently deletes a session file and, if present, the CLI's sidecar
/// folder next to it (`<session_id>/`: subagents, tool results).
pub fn delete_session(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)?;
    let sidecar = path.with_extension("");
    if sidecar.is_dir() {
        std::fs::remove_dir_all(&sidecar)?;
    }
    Ok(())
}

fn scan_file(path: &Path, mtime: SystemTime) -> Option<HistoryEntry> {
    let session_id = path.file_stem()?.to_str()?.to_owned();
    let file = File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);

    let mut cwd: Option<String> = None;
    let mut ai_title: Option<String> = None;
    let mut first_prompt: Option<String> = None;
    let mut queued_prompt: Option<String> = None;
    let mut last_prompt: Option<String> = None;
    let mut count = 0usize;

    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let is_user = contains(&buf, br#""type":"user""#);
        let is_assistant = contains(&buf, br#""type":"assistant""#);
        if is_user || is_assistant {
            count += 1;
        }
        let need_cwd = cwd.is_none() && contains(&buf, br#""cwd":"#);
        let is_title = contains(&buf, br#""type":"ai-title""#);
        let is_last = contains(&buf, br#""type":"last-prompt""#);
        let is_queue = queued_prompt.is_none() && contains(&buf, br#""type":"queue-operation""#);
        let need_prompt = is_user && first_prompt.is_none();
        if !(need_cwd || is_title || is_last || is_queue || need_prompt) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&buf) else {
            continue;
        };
        if need_cwd {
            cwd = v.get("cwd").and_then(Value::as_str).map(str::to_owned);
        }
        match v.get("type").and_then(Value::as_str) {
            Some("ai-title") => {
                if let Some(t) = v.get("aiTitle").and_then(Value::as_str)
                    && !t.trim().is_empty()
                {
                    ai_title = Some(t.to_owned());
                }
            }
            Some("last-prompt") => {
                last_prompt = v
                    .get("lastPrompt")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Some("queue-operation") => {
                if v.get("operation").and_then(Value::as_str) == Some("enqueue") {
                    queued_prompt = v.get("content").and_then(Value::as_str).map(str::to_owned);
                }
            }
            Some("user") if first_prompt.is_none() => {
                first_prompt = real_user_prompt(&v);
            }
            _ => {}
        }
    }

    let title = ai_title
        .or(first_prompt)
        .or(queued_prompt)
        .or(last_prompt)
        .map(|t| truncate_title(&t, HISTORY_TITLE_MAX))
        .unwrap_or_else(|| "(untitled)".to_owned());

    Some(HistoryEntry {
        session_id,
        cwd,
        title,
        modified: DateTime::<Local>::from(mtime),
        approx_messages: count,
        path: path.to_owned(),
    })
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Text of a "real" user prompt: not meta, not sidechain, plain text that is
/// not a command wrapper (`<command-name>`, `<local-command-...>`) nor an
/// interrupt marker. `None` otherwise (e.g. tool results).
fn real_user_prompt(v: &Value) -> Option<String> {
    if v.get("isMeta").and_then(Value::as_bool) == Some(true)
        || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let blocks = parse_content(v.get("message").and_then(|m| m.get("content")));
    let text: String = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let t = text.trim();
    if t.is_empty() || t.starts_with('<') || t.starts_with("[Request interrupted") {
        None
    } else {
        Some(t.to_owned())
    }
}

fn parse_ts(v: &Value) -> Option<DateTime<Local>> {
    let s = v.get("timestamp")?.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Local))
}

/// Loads a whole session file into transcript items (sidechain/subagent lines
/// and bookkeeping lines are skipped).
pub fn load_session(path: &Path) -> std::io::Result<LoadedSession> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = LoadedSession {
        session_id: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned(),
        ..Default::default()
    };
    let mut ai_title: Option<String> = None;
    let mut first_prompt: Option<String> = None;
    let fallback_ts = Local::now();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if out.cwd.is_none() {
            out.cwd = v.get("cwd").and_then(Value::as_str).map(str::to_owned);
        }
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "ai-title" {
            if let Some(t) = v.get("aiTitle").and_then(Value::as_str) {
                ai_title = Some(t.to_owned());
            }
            continue;
        }
        if ty != "user" && ty != "assistant" {
            continue;
        }
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let at = parse_ts(&v).unwrap_or(fallback_ts);
        let msg = v.get("message");
        let blocks = parse_content(msg.and_then(|m| m.get("content")));

        if ty == "assistant" {
            if let Some(m) = msg
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .filter(|m| *m != "<synthetic>")
            {
                out.model = Some(m.to_owned());
            }
            for b in blocks {
                match b {
                    ContentBlock::Text(t) if !t.is_empty() => out.items.push(Item::AssistantText {
                        text: t,
                        finalized: true,
                    }),
                    ContentBlock::ToolUse { id, name, input } => out.items.push(Item::ToolUse {
                        id,
                        name,
                        input,
                        result: None,
                    }),
                    _ => {}
                }
            }
        } else {
            // Images / pasted texts of this message, shown as chips on its text.
            let mut chips: Vec<AttachmentSummary> = Vec::new();
            for b in blocks {
                match b {
                    ContentBlock::Unknown(v)
                        if v.get("type").and_then(Value::as_str) == Some("image") =>
                    {
                        chips.push(AttachmentSummary {
                            label: "Imagen".into(),
                        });
                    }
                    ContentBlock::Text(t) => {
                        let tt = t.trim();
                        if tt.starts_with("<pasted_text>") {
                            let lines = tt.lines().count().saturating_sub(2);
                            chips.push(AttachmentSummary {
                                label: format!("Pasted text · {lines} lines"),
                            });
                            continue;
                        }
                        if tt.is_empty() || tt.starts_with('<') {
                            continue;
                        }
                        if tt.starts_with("[Request interrupted") {
                            out.items.push(Item::Note {
                                kind: NoteKind::Info,
                                text: "Interrupted by the user.".into(),
                                at,
                            });
                            continue;
                        }
                        if first_prompt.is_none() {
                            first_prompt = Some(tt.to_owned());
                        }
                        out.items.push(Item::UserText {
                            text: t.clone(),
                            at,
                            attachments: std::mem::take(&mut chips),
                        });
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        for it in out.items.iter_mut().rev() {
                            if let Item::ToolUse { id, result, .. } = it
                                && *id == tool_use_id
                            {
                                *result = Some((content.clone(), is_error));
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Attachments with no text of their own still get a bubble.
            if !chips.is_empty() {
                out.items.push(Item::UserText {
                    text: String::new(),
                    at,
                    attachments: chips,
                });
            }
        }
    }

    out.title = ai_title
        .or(first_prompt)
        .map(|t| truncate_title(&t, HISTORY_TITLE_MAX))
        .unwrap_or_else(|| "(untitled)".to_owned());
Ok(out)
}
