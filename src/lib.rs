//! Core (UI-free) of claude-deck: a wrapper around the `claude` CLI running in
//! headless `stream-json` mode.
//!
//! * [`protocol`]: tolerant serde-ish types for the CLI wire format.
//! * [`session`]: process management ([`session::SessionHandle`]) and the event
//!   type ([`session::SessionEvent`]) delivered through a `std::sync::mpsc` channel.
//! * [`state`]: the app model ([`state::AppState`]) the UI draws, fed by
//!   [`state::AppState::apply`].
//! * [`history`]: scanning/loading of `~/.claude/projects/*/*.jsonl`.

pub mod git;
pub mod history;
pub mod protocol;
pub mod read_guard;
pub mod session;
pub mod state;
pub mod tools;
pub mod ui;

/// Installs a `tracing` subscriber writing to stderr (filter via `RUST_LOG`,
/// default `claude_deck=info`). Safe to call more than once.
pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("claude_deck=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
