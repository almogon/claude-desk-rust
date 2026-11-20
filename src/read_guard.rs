//! Guard against dumping whole large files into the model context.
//!
//! Pure logic: given a tool call, decide whether it should be refused because it
//! would read an entire file with more than `max_lines` lines. It prefers false
//! negatives over false positives: anything odd is allowed.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Line count above which reading a whole file is refused.
pub const MAX_LINES: usize = 300;

/// Extensions that are never counted (binary or structured formats the Read tool handles specially).
const SKIP_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "pdf", "ipynb",
];

/// Words/flags that indicate the command already limits its output.
const LIMITERS: &[&str] = &[
    "-totalcount",
    "-head",
    "-tail",
    "-first",
    "-last",
    "select-object",
    "select",
    "head",
    "tail",
    "measure-object",
    "wc",
];

/// Commands that print whole files.
const DUMP_COMMANDS: &[&str] = &["get-content", "gc", "cat", "type", "more", "less"];

/// Flags (lowercase) whose next argument is a value, not a path.
const VALUE_FLAGS: &[&str] = &["-encoding", "-delimiter", "-stream", "-filter"];

/// Flags (lowercase) whose next argument is a path.
const PATH_FLAGS: &[&str] = &["-path", "-literalpath"];

/// Returns Some(reason) if the tool call would dump a whole file larger than `max_lines`
/// lines into the context; None to allow it.
pub fn check(
    tool_name: &str,
    input: &serde_json::Value,
    cwd: &Path,
    max_lines: usize,
) -> Option<String> {
    match tool_name {
        "Read" => check_read(input, cwd, max_lines),
        "PowerShell" | "Bash" => check_shell(input, cwd, max_lines),
        _ => None,
    }
}

fn check_read(input: &serde_json::Value, cwd: &Path, max_lines: usize) -> Option<String> {
    if input.get("limit").is_some() {
        return None;
    }
    let given = input.get("file_path")?.as_str()?;
    check_path(given, cwd, max_lines)
}

fn check_shell(input: &serde_json::Value, cwd: &Path, max_lines: usize) -> Option<String> {
    let command = input.get("command")?.as_str()?;

    let has_limiter = command
        .split(|c: char| c.is_whitespace() || "|;&()\"'`,".contains(c))
        .any(|w| LIMITERS.contains(&w.to_lowercase().as_str()));
    if has_limiter {
        return None;
    }

    let normalized = command.replace("&&", ";").replace("||", ";");
    for segment in normalized.split(['|', ';', '\n']) {
        let mut words = segment.split_whitespace();
        let Some(first) = words.next() else { continue };
        if !DUMP_COMMANDS.contains(&first.to_lowercase().as_str()) {
            continue;
        }
        let mut args = words;
        while let Some(arg) = args.next() {
            let candidate = if let Some(flag) = arg.strip_prefix('-').map(|_| arg.to_lowercase()) {
                if PATH_FLAGS.contains(&flag.as_str()) {
                    args.next()
                } else {
                    if VALUE_FLAGS.contains(&flag.as_str()) {
                        args.next();
                    }
                    None
                }
            } else {
                Some(arg)
            };
            if let Some(c) = candidate {
                let given = c.trim_matches(|ch| ch == '"' || ch == '\'');
                if given.is_empty() {
                    continue;
                }
                if let Some(reason) = check_path(given, cwd, max_lines) {
                    return Some(reason);
                }
            }
        }
    }
    None
}

/// Checks one path (as given by the model) and builds the refusal reason if too long.
fn check_path(given: &str, cwd: &Path, max_lines: usize) -> Option<String> {
    let path = resolve(given, cwd);
    let ext_skipped = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| SKIP_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false);
    if ext_skipped {
        return None;
    }
    let lines = count_lines(&path)?;
    if lines > max_lines {
        Some(format!(
            "Bloqueado por claude-deck: {given} tiene {lines} líneas (máx. {max_lines} para leerlo entero). Localiza con Grep y lee solo el rango necesario con Read (offset/limit)."
        ))
    } else {
        None
    }
}

fn resolve(given: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(given);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Counts lines of a regular file; None if missing, unreadable, or binary (NUL in first 8 KB).
fn count_lines(path: &Path) -> Option<usize> {
    let mut file = File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = vec![0u8; 64 * 1024];
    let mut newlines = 0usize;
    let mut total = 0usize;
    let mut last = b'\n';
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        if total < 8192 {
            let end = (8192 - total).min(n);
            if chunk[..end].contains(&0) {
                return None;
            }
        }
        newlines += chunk.iter().filter(|&&b| b == b'\n').count();
        total += n;
        last = chunk[n - 1];
    }
    if total > 0 && last != b'\n' {
        newlines += 1;
    }
    Some(newlines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(n: usize) -> String {
        "x\n".repeat(n)
    }

    fn setup() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.rs"), lines(500)).unwrap();
        std::fs::write(dir.path().join("small.rs"), lines(10)).unwrap();
        std::fs::write(dir.path().join("pic.png"), lines(500)).unwrap();
        let mut bin = vec![0u8; 100];
        bin.extend(lines(500).into_bytes());
        std::fs::write(dir.path().join("data.bin"), bin).unwrap();
        dir
    }

    fn read(dir: &tempfile::TempDir, input: serde_json::Value) -> Option<String> {
        check("Read", &input, dir.path(), MAX_LINES)
    }

    fn shell(dir: &tempfile::TempDir, tool: &str, cmd: &str) -> Option<String> {
        check(tool, &json!({ "command": cmd }), dir.path(), MAX_LINES)
    }

    #[test]
    fn read_small_allowed() {
        let d = setup();
        let p = d.path().join("small.rs");
        assert!(read(&d, json!({ "file_path": p })).is_none());
    }

    #[test]
    fn read_big_denied_with_reason() {
        let d = setup();
        let p = d.path().join("big.rs");
        let given = p.to_str().unwrap();
        let reason = read(&d, json!({ "file_path": given })).unwrap();
        assert!(reason.starts_with("Bloqueado por claude-deck: "));
        assert!(reason.contains(given));
        assert!(reason.contains("500 líneas (máx. 300"));
    }

    #[test]
    fn read_with_limit_allowed() {
        let d = setup();
        let p = d.path().join("big.rs");
        assert!(read(&d, json!({ "file_path": p, "limit": 50 })).is_none());
    }

    #[test]
    fn read_relative_resolved() {
        let d = setup();
        assert!(read(&d, json!({ "file_path": "big.rs" })).is_some());
    }

    #[test]
    fn read_missing_allowed() {
        let d = setup();
        assert!(read(&d, json!({ "file_path": "nope.rs" })).is_none());
    }

    #[test]
    fn read_png_and_binary_allowed() {
        let d = setup();
        assert!(read(&d, json!({ "file_path": "pic.png" })).is_none());
        assert!(read(&d, json!({ "file_path": "data.bin" })).is_none());
    }

    #[test]
    fn powershell_get_content_denied() {
        let d = setup();
        assert!(shell(&d, "PowerShell", "Get-Content big.rs").is_some());
        assert!(shell(&d, "PowerShell", "Get-Content -Path big.rs").is_some());
        assert!(shell(&d, "PowerShell", "Get-Content small.rs").is_none());
    }

    #[test]
    fn limited_output_allowed() {
        let d = setup();
        assert!(shell(&d, "PowerShell", "Get-Content big.rs -TotalCount 50").is_none());
        assert!(
            shell(
                &d,
                "PowerShell",
                "Get-Content big.rs | Select-Object -First 20"
            )
            .is_none()
        );
    }

    #[test]
    fn bash_cat_quoted_denied() {
        let d = setup();
        assert!(shell(&d, "Bash", "cat \"big.rs\"").is_some());
    }

    #[test]
    fn other_tool_allowed() {
        let d = setup();
        let input = json!({ "file_path": "big.rs", "command": "cat big.rs" });
        assert!(check("Grep", &input, d.path(), MAX_LINES).is_none());
    }
}
