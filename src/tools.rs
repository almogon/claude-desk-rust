//! Built-in tool list passed to the CLI with `--tools`.
//!
//! The task-list tools (`TaskCreate`, ...) are not enabled in headless mode by
//! default; they only show up when listed in `--tools`. But `--tools` replaces
//! the whole built-in set (`default,TaskCreate` does not combine), so the full
//! list has to be explicit. To follow CLI updates, the list is learned from the
//! `system/init.tools` of a session spawned WITHOUT `--tools` and cached on
//! disk; it is relearned whenever the CLI version changes.
//!
//! Every function here is best effort: cache errors never prevent a spawn.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Built-in tools of a plain run (`system/init.tools`, CLI 2.1.328). Used until
/// a list has been learned.
pub const FALLBACK_DEFAULT_TOOLS: &[&str] = &[
    "Task",
    "Artifact",
    "ArtifactComments",
    "ArtifactData",
    "AskUserQuestion",
    "Bash",
    "CronCreate",
    "CronDelete",
    "CronList",
    "DesignSync",
    "Edit",
    "EnterPlanMode",
    "EnterWorktree",
    "ExitPlanMode",
    "ExitWorktree",
    "Glob",
    "Grep",
    "ListAgents",
    "Monitor",
    "NotebookEdit",
    "PowerShell",
    "PushNotification",
    "Read",
    "RemoteTrigger",
    "ReportFindings",
    "ScheduleWakeup",
    "SendMessage",
    "SendToProject",
    "SendToProjectContext",
    "Skill",
    "TaskStop",
    "ToolSearch",
    "WebSearch",
    "Write",
];

/// Task-list tools that are only available when requested explicitly.
pub const TASK_TOOLS: &[&str] = &["TaskCreate", "TaskUpdate", "TaskList", "TaskGet"];

/// Tools the app depends on (dedicated UI); always passed, even if a cache was
/// learned badly.
pub const MUST_KEEP: &[&str] = &["AskUserQuestion", "ExitPlanMode", "EnterPlanMode"];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Cache {
    /// CLI version the `tools` list was learned from.
    cli_version: String,
    /// Built-in tools (without `TASK_TOOLS` and `mcp__*`).
    tools: Vec<String>,
    /// Version of the last session seen, learning or not.
    last_seen_version: Option<String>,
}

/// `<config dir>/claude-deck/tools_cache.json`.
pub fn cache_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("claude-deck").join("tools_cache.json"))
}

fn read_cache(path: &Path) -> Option<Cache> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let tools: Vec<String> = v
        .get("tools")?
        .as_array()?
        .iter()
        .filter_map(|t| t.as_str().map(str::to_owned))
        .collect();
    Some(Cache {
        cli_version: v.get("cli_version")?.as_str()?.to_owned(),
        tools,
        last_seen_version: v
            .get("last_seen_version")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn write_cache(path: &Path, c: &Cache) {
    let v = json!({
        "cli_version": c.cli_version,
        "tools": c.tools,
        "last_seen_version": c.last_seen_version,
    });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(path, v.to_string()) {
        tracing::warn!(error = %e, "could not write the tools cache");
    }
}

/// Union of `base`, [`MUST_KEEP`] and [`TASK_TOOLS`], without duplicates.
fn build_list(base: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in base
        .iter()
        .map(String::as_str)
        .chain(MUST_KEEP.iter().copied())
        .chain(TASK_TOOLS.iter().copied())
    {
        if !t.is_empty() && !out.iter().any(|o| o == t) {
            out.push(t.to_owned());
        }
    }
    out
}

/// Value of `--tools` using the cache at `path`, or `None` when the session
/// must be spawned without `--tools` to (re)learn the list: no usable cache, or
/// the last seen CLI version differs from the cached one.
pub fn tools_arg_at(path: Option<&Path>) -> Option<String> {
    let cache = path.and_then(read_cache);
    let base: Vec<String> = match &cache {
        Some(c) if !c.tools.is_empty() => {
            let stale = c
                .last_seen_version
                .as_deref()
                .is_some_and(|v| v != c.cli_version);
            if stale {
                return None;
            }
            c.tools.clone()
        }
        // No cache: learn. (With nowhere to store a cache, use the fallback.)
        _ if path.is_some() => return None,
        _ => FALLBACK_DEFAULT_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let list = build_list(&base);
    if list.is_empty() {
        return None;
    }
    Some(list.join(","))
}

/// [`tools_arg_at`] with the default cache location.
pub fn tools_arg() -> Option<String> {
    tools_arg_at(cache_path().as_deref())
}

/// Stores the tools of a session spawned without `--tools` (minus
/// [`TASK_TOOLS`] and `mcp__*`).
pub fn remember_at(path: &Path, version: &str, tools: &[String]) {
    let tools: Vec<String> = tools
        .iter()
        .filter(|t| !t.starts_with("mcp__") && !TASK_TOOLS.contains(&t.as_str()))
        .cloned()
        .collect();
    if version.is_empty() || tools.is_empty() {
        return;
    }
    write_cache(
        path,
        &Cache {
            cli_version: version.to_owned(),
            tools,
            last_seen_version: Some(version.to_owned()),
        },
    );
}

/// [`remember_at`] with the default cache location.
pub fn remember(version: &str, tools: &[String]) {
    if let Some(p) = cache_path() {
        remember_at(&p, version, tools);
    }
}

/// Records the CLI version of a session spawned WITH `--tools`, so that a
/// version change triggers a relearn on the next spawn.
pub fn note_version_at(path: &Path, version: &str) {
    if let Some(mut c) = read_cache(path)
        && c.last_seen_version.as_deref() != Some(version)
    {
        c.last_seen_version = Some(version.to_owned());
        write_cache(path, &c);
    }
}

/// [`note_version_at`] with the default cache location.
pub fn note_version(version: &str) {
    if let Some(p) = cache_path() {
        note_version_at(&p, version);
    }
}

/// Called with every `system/init` of a session. `learning` is true when the
/// session was spawned without `--tools`.
pub fn on_init(learning: bool, version: Option<&str>, tools: &[String]) {
    let Some(v) = version else { return };
    if learning {
        remember(v, tools);
    } else {
        note_version(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn no_cache_means_learn() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(tools_arg_at(Some(&dir.path().join("none.json"))), None);
    }

    #[test]
    fn no_cache_location_uses_fallback() {
        let a = tools_arg_at(None).unwrap();
        for t in MUST_KEEP.iter().chain(TASK_TOOLS) {
            assert!(a.split(',').any(|x| x == *t), "{t}");
        }
        assert!(a.split(',').any(|x| x == "Bash"));
    }

    #[test]
    fn remember_filters_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("c.json");
        remember_at(
            &p,
            "9.9.9",
            &s(&["Bash", "Read", "TaskCreate", "mcp__x__y", "Bash"]),
        );
        let a = tools_arg_at(Some(&p)).unwrap();
        let parts: Vec<&str> = a.split(',').collect();
        assert_eq!(parts[..2], ["Bash", "Read"]);
        // Must-keep and task tools are appended, no duplicates.
        for t in MUST_KEEP.iter().chain(TASK_TOOLS) {
            assert_eq!(parts.iter().filter(|x| *x == t).count(), 1, "{t}");
        }
        assert!(!a.contains("mcp__"));
    }

    #[test]
    fn version_change_triggers_relearn() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        remember_at(&p, "1.0.0", &s(&["Bash"]));
        assert!(tools_arg_at(Some(&p)).is_some());
        note_version_at(&p, "1.0.0");
        assert!(tools_arg_at(Some(&p)).is_some());
        note_version_at(&p, "1.1.0");
        assert_eq!(tools_arg_at(Some(&p)), None);
        remember_at(&p, "1.1.0", &s(&["Bash", "Grep"]));
        assert!(tools_arg_at(Some(&p)).unwrap().starts_with("Bash,Grep"));
    }

    #[test]
    fn empty_or_garbage_cache_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(tools_arg_at(Some(&p)), None);
        std::fs::write(&p, r#"{"cli_version":"1","tools":[]}"#).unwrap();
        assert_eq!(tools_arg_at(Some(&p)), None);
        // Nothing learnable: the cache is not written.
        remember_at(&p, "2", &s(&["TaskCreate", "mcp__a__b"]));
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            r#"{"cli_version":"1","tools":[]}"#
        );
    }
}
