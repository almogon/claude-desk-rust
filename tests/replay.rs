//! Replays captured CLI output (tests/fixtures) through the state model.

use std::path::PathBuf;

use claude_deck::protocol::{ControlRequestBody, Incoming};
use claude_deck::session::{SessionEvent, SessionEventKind, SessionKey};
use claude_deck::state::{
    AppState, Item, NewSession, NoteKind, PendingReason, SessionStatus, SubagentStatus,
};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn fixture_lines(name: &str) -> Vec<String> {
    std::fs::read_to_string(fixture_dir().join(format!("{name}.jsonl")))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn ev(key: SessionKey, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        session_key: key,
        kind,
    }
}

fn incoming(key: SessionKey, line: &str) -> SessionEvent {
    ev(
        key,
        SessionEventKind::Incoming(Box::new(Incoming::parse_line(line).unwrap())),
    )
}

/// Replays a fixture. A `UserSent` is injected before the first line and before
/// every later `system/init` (the CLI emits init once per turn, after the
/// message arrives). `decide` answers permission requests (`Some(allowed)`);
/// `check` is called after a request arrives and after it is resolved.
fn replay(
    name: &str,
    decide: impl Fn(&str) -> Option<bool>,
    check: impl Fn(&AppState, SessionKey, &str),
) -> (AppState, SessionKey) {
    let key = SessionKey::new();
    let mut st = AppState::new();
    st.add_session(NewSession::new(key, "C:\\work"));
    st.apply(&ev(key, SessionEventKind::Spawned { pid: Some(1) }));
    let mut inits = 0;
    let user_sent = |st: &mut AppState| {
        st.apply(&ev(
            key,
            SessionEventKind::UserSent {
                text: "hola".into(),
                attachments: Vec::new(),
            },
        ));
    };
    user_sent(&mut st);
    for line in fixture_lines(name) {
        let inc = Incoming::parse_line(&line).unwrap_or_else(|e| panic!("{name}: {e}: {line}"));
        if matches!(inc, Incoming::Init(_)) {
            inits += 1;
            if inits > 1 {
                user_sent(&mut st);
            }
        }
        let request = match &inc {
            Incoming::ControlRequest(cr) => Some(cr.request_id.clone()),
            _ => None,
        };
        st.apply(&ev(key, SessionEventKind::Incoming(Box::new(inc))));
        if let Some(rid) = request {
            check(&st, key, "after_request");
            if let Some(allowed) = decide(&rid) {
                st.apply(&ev(
                    key,
                    SessionEventKind::PermissionResolved {
                        request_id: rid,
                        allowed,
                    },
                ));
                check(&st, key, "after_resolve");
            }
        }
    }
    (st, key)
}

fn noop(_: &AppState, _: SessionKey, _: &str) {}

fn count(items: &[Item], f: impl Fn(&Item) -> bool) -> usize {
    items.iter().filter(|i| f(i)).count()
}

#[test]
fn all_fixture_lines_parse() {
    let mut total = 0;
    for entry in std::fs::read_dir(fixture_dir()).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_owned();
        // stdout captures only; `.stdin.jsonl` and the history sample have other shapes.
        if name.ends_with(".stdin.jsonl") || name == "history_sample.jsonl" {
            continue;
        }
        for line in std::fs::read_to_string(&p).unwrap().lines() {
            if line.trim().is_empty() {
                continue;
            }
            Incoming::parse_line(line).unwrap_or_else(|e| panic!("{name}: {e}"));
            total += 1;
        }
    }
    assert!(total > 100);
}

#[test]
fn simple_one_text_and_summary() {
    let (st, key) = replay("simple", |_| None, noop);
    let s = st.session(key).unwrap();
    assert_eq!(s.transcript.len(), 3, "{:?}", s.transcript);
    assert!(matches!(&s.transcript[0], Item::UserText { text, .. } if text == "hola"));
    assert!(
        matches!(&s.transcript[1], Item::AssistantText { text, finalized: true } if text == "hola")
    );
    assert!(
        matches!(&s.transcript[2], Item::TurnSummary(t) if !t.is_error && t.output_tokens == 4)
    );
    assert_eq!(
        s.status,
        SessionStatus::Pending(PendingReason::AwaitingInput)
    );
    assert!(s.session_id.is_some());
    assert_eq!(s.model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(s.usage.turns, 1);
    assert!(s.usage.cost_usd > 0.05);
    assert_eq!(st.day_usage().unwrap().turns, 1);
    let rl = st.rate_limits();
    assert!((rl.five_hour.as_ref().unwrap().utilization - 0.07).abs() < 1e-9);
    assert!(rl.seven_day.is_some());
}

#[test]
fn permission_allow_flow() {
    let checks = |st: &AppState, key: SessionKey, stage: &str| {
        let s = st.session(key).unwrap();
        if stage == "after_request" {
            assert_eq!(s.status, SessionStatus::Pending(PendingReason::Permission));
            assert_eq!(s.pending_permissions().count(), 1);
        } else {
            assert_eq!(s.status, SessionStatus::Running);
        }
    };
    let (st, key) = replay("permission_allow", |_| Some(true), checks);
    let s = st.session(key).unwrap();
    let perm: Vec<_> = s
        .transcript
        .iter()
        .filter_map(|i| match i {
            Item::PermissionRequest(p) => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(perm.len(), 1);
    assert_eq!(perm[0].tool_name, "Write");
    assert_eq!(perm[0].resolved, Some(true));
    let tu = s
        .transcript
        .iter()
        .find_map(|i| match i {
            Item::ToolUse { name, result, .. } if name == "Write" => Some(result.clone()),
            _ => None,
        })
        .unwrap();
    assert!(matches!(tu, Some((_, false))));
    assert_eq!(
        s.status,
        SessionStatus::Pending(PendingReason::AwaitingInput)
    );
}

#[test]
fn permission_always_exposes_suggestions() {
    let (st, key) = replay("permission_always", |_| Some(true), noop);
    let s = st.session(key).unwrap();
    let p = s
        .transcript
        .iter()
        .find_map(|i| match i {
            Item::PermissionRequest(p) => Some(p),
            _ => None,
        })
        .unwrap();
    assert_eq!(p.tool_name, "Bash");
    assert_eq!(p.suggestions.len(), 3);
    let rule = p.add_rules_suggestion().expect("addRules suggestion");
    assert_eq!(rule["destination"], "localSettings");
    assert_eq!(rule["rules"][0]["toolName"], "Bash");
    assert!(p.accept_edits_suggestion().is_some());
}

#[test]
fn permission_deny_flow() {
    let (st, key) = replay("permission_deny", |_| Some(false), noop);
    let s = st.session(key).unwrap();
    let p = s
        .transcript
        .iter()
        .find_map(|i| match i {
            Item::PermissionRequest(p) => Some(p),
            _ => None,
        })
        .unwrap();
    assert_eq!(p.resolved, Some(false));
    let tu = s
        .transcript
        .iter()
        .find_map(|i| match i {
            Item::ToolUse { result, .. } => Some(result.clone()),
            _ => None,
        })
        .unwrap();
    assert!(matches!(tu, Some((_, true))), "denied result is an error");
    assert_eq!(
        count(&s.transcript, |i| matches!(i, Item::TurnSummary(_))),
        1
    );
}

#[test]
fn permission_noflag_records_denial_note() {
    let (st, key) = replay("permission_noflag", |_| None, noop);
    let s = st.session(key).unwrap();
    assert!(s.transcript.iter().any(|i| matches!(i,
        Item::Note { kind: NoteKind::System, text, .. } if text.contains("Permiso denegado"))));
}

#[test]
fn subagent_is_isolated() {
    let (st, key) = replay("subagent", |_| None, noop);
    let s = st.session(key).unwrap();
    assert_eq!(s.subagents.len(), 1);
    let sub = &s.subagents[0];
    assert_eq!(sub.status, SubagentStatus::Completed);
    assert_eq!(sub.subagent_type.as_deref(), Some("general-purpose"));
    assert!(sub.agent_id.is_some());
    assert!(sub.ended_at.is_some());
    assert!(sub.summary.as_deref().unwrap().contains("AZUL"));
    assert!(sub.message_count >= 2 && sub.tool_count >= 1);
    assert!(sub.last_activity.is_some());
    assert!(sub.items.iter().any(|i| matches!(i, Item::ToolUse { .. })));
    assert!(matches!(&s.transcript[0], Item::UserText { .. }));
    let main_tools: Vec<_> = s
        .transcript
        .iter()
        .filter_map(|i| match i {
            Item::ToolUse { name, result, .. } => Some((name.as_str(), result.is_some())),
            _ => None,
        })
        .collect();
    assert_eq!(main_tools, vec![("Agent", true)]);
    assert_eq!(
        count(&s.transcript, |i| matches!(i, Item::AssistantText { .. })),
        1
    );
    assert_eq!(
        count(&s.transcript, |i| matches!(i, Item::TurnSummary(_))),
        1
    );
}

#[test]
fn controls_change_mode_and_model() {
    let (st, key) = replay("controls", |_| None, noop);
    let s = st.session(key).unwrap();
    assert_eq!(s.mode.as_deref(), Some("plan"));
    assert!(s.model.is_some());
    assert_eq!(
        count(
            &s.transcript,
            |i| matches!(i, Item::TurnSummary(t) if t.is_error)
        ),
        1
    );
    assert!(s.transcript.iter().any(|i| matches!(
        i,
        Item::Note {
            kind: NoteKind::Error,
            ..
        }
    )));
}

#[test]
fn usage_fixture_captures_text() {
    let (st, _key) = replay("usage", |_| None, noop);
    assert!(st.rate_limits().five_hour.is_none());
    let t = st.usage_text().expect("usage text captured");
    assert!(t.text.contains("Current session"));
}

#[test]
fn multiturn_two_summaries_and_cost_delta() {
    let (st, key) = replay("multiturn", |_| None, noop);
    let s = st.session(key).unwrap();
    let sums: Vec<_> = s
        .transcript
        .iter()
        .filter_map(|i| match i {
            Item::TurnSummary(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(sums.len(), 2);
    assert_eq!(s.usage.turns, 2);
    let day = st.day_usage().unwrap();
    assert!((day.cost_usd - sums[1].total_cost_usd).abs() < 1e-9);
    assert_eq!(s.usage.tokens.output, 8);
}

#[test]
fn resume_does_not_count_history_cost_for_day() {
    let key = SessionKey::new();
    let mut st = AppState::new();
    let mut n = NewSession::new(key, "C:\\work");
    n.resume_session_id = Some("abc".into());
    st.add_session(n);
    for line in fixture_lines("resume") {
        st.apply(&incoming(key, &line));
    }
    let s = st.session(key).unwrap();
    assert!(s.usage.cost_usd > 0.1);
    assert_eq!(st.day_usage().unwrap().cost_usd, 0.0);
    assert_eq!(s.session_id.as_deref().map(str::len), Some(36));
}

#[test]
fn exit_marks_finished_and_groups() {
    let (mut st, key) = replay("simple", |_| None, noop);
    assert_eq!(st.grouped().pending.len(), 1);
    st.apply(&ev(
        key,
        SessionEventKind::Exited {
            error: Some("boom".into()),
        },
    ));
    let g = st.grouped();
    assert_eq!(g.finished.len(), 1);
    assert!(
        matches!(&g.finished[0].status, SessionStatus::Finished { error: Some(e) } if e == "boom")
    );
}

#[test]
fn special_tools_are_marked() {
    let line = r#"{"type":"control_request","request_id":"r","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"x"},"tool_use_id":"t"}}"#;
    let Incoming::ControlRequest(cr) = Incoming::parse_line(line).unwrap() else {
        panic!()
    };
    assert!(matches!(cr.body, ControlRequestBody::CanUseTool(_)));
    let key = SessionKey::new();
    let mut st = AppState::new();
    st.add_session(NewSession::new(key, "C:\\work"));
    st.apply(&incoming(key, line));
    let s = st.session(key).unwrap();
    assert!(s.pending_permissions().next().unwrap().is_special_tool());
}

#[test]
fn streaming_deltas_do_not_duplicate() {
    let key = SessionKey::new();
    let mut st = AppState::new();
    st.add_session(NewSession::new(key, "C:\\work"));
    let lines = [
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"m1"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ho"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"la"}}}"#,
    ];
    for l in lines {
        st.apply(&incoming(key, l));
    }
    assert!(matches!(&st.session(key).unwrap().transcript[0],
        Item::AssistantText { text, finalized: false } if text == "hola"));
    let lines = [
        r#"{"type":"assistant","message":{"model":"m1","content":[{"type":"text","text":"hola"}]}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
    ];
    for l in lines {
        st.apply(&incoming(key, l));
    }
    let s = st.session(key).unwrap();
    assert_eq!(s.transcript.len(), 1);
    assert!(
        matches!(&s.transcript[0], Item::AssistantText { text, finalized: true } if text == "hola")
    );
    assert_eq!(s.model.as_deref(), Some("m1"));
}

fn first_permission(st: &AppState, key: SessionKey) -> claude_deck::state::PermissionItem {
    st.session(key)
        .unwrap()
        .transcript
        .iter()
        .find_map(|i| match i {
            Item::PermissionRequest(p) => Some(p.clone()),
            _ => None,
        })
        .expect("a PermissionRequest item")
}

#[test]
fn ask_answer_creates_question_request() {
    let (st, key) = replay("ask_answer", |_| Some(true), noop);
    let p = first_permission(&st, key);
    assert_eq!(p.tool_name, "AskUserQuestion");
    assert!(p.is_special_tool());
    assert_eq!(p.resolved, Some(true));
    let q = &p.input["questions"][0];
    assert_eq!(q["multiSelect"], false);
    assert_eq!(q["options"].as_array().unwrap().len(), 2);
    assert!(q["question"].as_str().unwrap().contains("color"));
}

#[test]
fn plan_edit_creates_plan_request() {
    let (st, key) = replay("plan_edit", |_| Some(true), noop);
    let p = first_permission(&st, key);
    assert_eq!(p.tool_name, "ExitPlanMode");
    assert!(p.is_special_tool());
    assert_eq!(p.resolved, Some(true));
    assert!(p.input["plan"].as_str().unwrap().contains("hola.txt"));
    assert!(p.input["planFilePath"].is_string());
    assert_eq!(
        st.session(key).unwrap().mode.as_deref(),
        Some("acceptEdits")
    );
}
