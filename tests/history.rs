use std::fs;

use claude_deck::history::{delete_session, is_test_cwd, load_session, scan_history};
use claude_deck::state::Item;

#[test]
fn test_cwd_detection() {
    let temp = std::env::temp_dir().join("demo");
    assert!(is_test_cwd(&temp.display().to_string()));
    assert!(is_test_cwd("F:\\dev\\claude-deck\\target\\tmp\\x"));
    assert!(is_test_cwd("C:\\a\\Sandbox\\b"));
    assert!(!is_test_cwd("C:\\work\\claude-deck"));
    assert!(!is_test_cwd("C:\\work\\sandboxed-app"));
}

const SAMPLE: &str = include_str!("fixtures/history_sample.jsonl");
const ID: &str = "efbc96b7-411a-44f5-a7e9-ec25f1c784ab";

#[test]
fn scan_and_load() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("C--some-project");
    fs::create_dir_all(proj.join(ID).join("subagents")).unwrap();
    fs::write(proj.join(format!("{ID}.jsonl")), SAMPLE).unwrap();
    // must be ignored: subagent history
    fs::write(
        proj.join(ID).join("subagents").join("agent-x.jsonl"),
        SAMPLE,
    )
    .unwrap();

    // second, newer session: no ai-title, meta/command/garbage lines before the real prompt
    let other = tmp.path().join("D--other");
    fs::create_dir_all(&other).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let second = [
        "garbage not json",
        r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"meta caveat"},"cwd":"D:\\other"}"#,
        r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"},"cwd":"D:\\other"}"#,
        r#"{"type":"user","message":{"role":"user","content":"real   first\nprompt"},"cwd":"D:\\other","timestamp":"2026-11-17T10:00:00.000Z"}"#,
        r#"{"type":"assistant","message":{"model":"claude-x","role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
    ]
    .join("\n");
    fs::write(other.join("second-session.jsonl"), second).unwrap();

    let all = scan_history(tmp.path(), 10);
    assert_eq!(all.len(), 2, "subagent file ignored");
    assert_eq!(all[0].session_id, "second-session", "newest first");
    assert_eq!(all[0].title, "real first prompt");
    assert_eq!(all[0].cwd.as_deref(), Some("D:\\other"));
    assert_eq!(all[0].approx_messages, 4);
    assert_eq!(all[1].session_id, ID);
    assert!(all[1].cwd.is_some());
    assert_eq!(all[1].title, "<short generated title>", "ai-title wins");

    assert_eq!(scan_history(tmp.path(), 1).len(), 1);
    assert!(scan_history(&tmp.path().join("missing"), 5).is_empty());

    let loaded = load_session(&all[0].path).unwrap();
    assert_eq!(loaded.model.as_deref(), Some("claude-x"));
    assert_eq!(loaded.items.len(), 2);
    assert!(matches!(&loaded.items[0], Item::UserText { text, .. } if text.starts_with("real")));
    assert!(matches!(&loaded.items[1], Item::AssistantText { text, .. } if text == "ok"));

    let sample = load_session(&all[1].path).unwrap();
    assert_eq!(sample.session_id, ID);
    assert!(
        sample
            .items
            .iter()
            .any(|i| matches!(i, Item::UserText { .. }))
    );
}

#[test]
fn delete_removes_file_and_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("C--some-project");
    fs::create_dir_all(proj.join(ID).join("subagents")).unwrap();
    let file = proj.join(format!("{ID}.jsonl"));
    fs::write(&file, SAMPLE).unwrap();
    fs::write(proj.join("other.jsonl"), SAMPLE).unwrap();

    delete_session(&file).unwrap();
    assert!(!file.exists());
    assert!(!proj.join(ID).exists());
    assert!(proj.join("other.jsonl").exists());
    assert_eq!(scan_history(tmp.path(), 10).len(), 1);
}
