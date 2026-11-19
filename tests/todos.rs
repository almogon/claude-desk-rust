//! Task list derived from the transcript: replays the real `TaskCreate` /
//! `TaskUpdate` capture (tests/fixtures/todos.jsonl) through `AppState::apply`,
//! plus a hand-written `TodoWrite` case (that tool is not enabled by default).

use std::path::PathBuf;

use claude_deck::protocol::Incoming;
use claude_deck::session::{SessionEvent, SessionEventKind, SessionKey};
use claude_deck::state::{AppState, NewSession, TodoStatus};

fn ev(key: SessionKey, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        session_key: key,
        kind,
    }
}

fn new_state() -> (AppState, SessionKey) {
    let key = SessionKey::new();
    let mut st = AppState::new();
    st.add_session(NewSession::new(key, "C:\\work\\sandbox"));
    st.apply(&ev(key, SessionEventKind::Spawned { pid: Some(1) }));
    st.apply(&ev(
        key,
        SessionEventKind::UserSent {
            text: "hola".into(),
            attachments: Vec::new(),
        },
    ));
    (st, key)
}

fn feed(st: &mut AppState, key: SessionKey, line: &str) {
    let inc = Incoming::parse_line(line).unwrap_or_else(|e| panic!("{e}: {line}"));
    st.apply(&ev(key, SessionEventKind::Incoming(Box::new(inc))));
}

#[test]
fn real_capture_builds_completed_task_list() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("todos.jsonl");
    let (mut st, key) = new_state();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        if !line.trim().is_empty() {
            feed(&mut st, key, line);
        }
    }
    let todos = st.session(key).unwrap().todos();
    assert_eq!(todos.len(), 3);
    assert!(todos.iter().all(|t| t.status == TodoStatus::Completed));
    assert!(todos[0].content.contains("a.txt"));
    assert_eq!(todos[2].active_form.as_deref(), Some("Creando c.txt"));
}

#[test]
fn todo_write_replays_into_session_todos() {
    let (mut st, key) = new_state();
    let write = |id: &str, todos: &str| {
        format!(
            r#"{{"type":"assistant","message":{{"model":"claude-haiku-5-5","id":"msg_{id}","role":"assistant","content":[{{"type":"tool_use","id":"{id}","name":"TodoWrite","input":{{"todos":{todos}}}}}]}},"parent_tool_use_id":null,"session_id":"s1","uuid":"u_{id}"}}"#
        )
    };
    feed(
        &mut st,
        key,
        &write(
            "t1",
            r#"[{"content":"A","status":"in_progress","activeForm":"Doing A"},{"content":"B","status":"pending"}]"#,
        ),
    );
    feed(
        &mut st,
        key,
        &write(
            "t2",
            r#"[{"content":"A","status":"completed"},{"content":"B","status":"in_progress"}]"#,
        ),
    );
    let todos = st.session(key).unwrap().todos();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[0].status, TodoStatus::Completed);
    assert_eq!(todos[1].status, TodoStatus::InProgress);
}
