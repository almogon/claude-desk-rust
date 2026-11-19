//! Real-CLI smoke test. Run with: `cargo test -- --ignored`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use claude_deck::protocol::{Incoming, PermissionMode};
use claude_deck::session::{
    SessionConfig, SessionEventKind, SessionHandle, SessionKey, event_channel,
};
use claude_deck::state::{AppState, Item, NewSession, PendingReason, SessionStatus};

#[test]
#[ignore = "launches the real `claude` CLI (uses quota)"]
fn real_cli_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let key = SessionKey::new();
    let (tx, rx) = event_channel();
    let mut cfg = SessionConfig::new(dir.path());
    cfg.model = Some("haiku".into());
    cfg.permission_mode = PermissionMode::Plan;

    let mut state = AppState::new();
    state.add_session(NewSession::new(key, dir.path().display().to_string()));
    let handle = SessionHandle::spawn(key, &cfg, tx, Arc::new(|| {})).expect("spawn");
    handle.send_user_message("responde solo: hola").unwrap();

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut got_result = false;
    while Instant::now() < deadline && !got_result {
        if let Ok(ev) = rx.recv_timeout(Duration::from_secs(1)) {
            if let SessionEventKind::Incoming(inc) = &ev.kind {
                got_result |= matches!(inc.as_ref(), Incoming::Result(_));
            }
            state.apply(&ev);
        }
    }
    assert!(got_result, "no result within timeout");
    let s = state.session(key).unwrap();
    assert_eq!(
        s.status,
        SessionStatus::Pending(PendingReason::AwaitingInput)
    );
    assert!(s.session_id.is_some());
    assert!(
        s.transcript
            .iter()
            .any(|i| matches!(i, Item::AssistantText { .. }))
    );

    handle.kill();
    let end = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < end && !exited {
        if let Ok(ev) = rx.recv_timeout(Duration::from_secs(1)) {
            exited |= matches!(ev.kind, SessionEventKind::Exited { .. });
            state.apply(&ev);
        }
    }
    assert!(exited);
    assert!(matches!(
        state.session(key).unwrap().status,
        SessionStatus::Finished { error: None }
    ));
}
