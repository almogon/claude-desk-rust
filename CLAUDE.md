# CLAUDE.md — claude-deck

Personal desktop client for Claude Code (Rust, egui/eframe 0.37) that wraps the `claude` CLI in headless stream-json mode. Public repo `almogon/claude-desk-rust` (MIT). Not released; built locally with `cargo build --release`.

## Layout

- `src/protocol.rs`: serde types for stream-json input/output. Must stay tolerant: unknown events/blocks fall back to raw JSON, never fail.
- `src/session.rs`: spawns and drives one CLI process per conversation (`SessionHandle`), emits `SessionEvent`s through an mpsc channel plus a repaint notifier. No egui here.
- `src/state.rs`: `AppState`, the model the UI draws; `apply(&SessionEvent)` is its only entry point.
- `src/history.rs`: scans and loads `~/.claude/projects/*/*.jsonl`.
- `src/ui/`: egui views (header, sidebar, chat, input, permission, subagents, usage). UI never blocks: slow work (history, `/usage`, folder picker) runs on threads.
- `docs/protocol.md` + `tests/fixtures/`: real CLI captures (sanitized). When the CLI changes, re-capture fixtures and update both.

## Conventions

- Code, comments and docs in English; every user-visible string in Spanish.
- Always pass `--permission-mode` explicitly (the user's default is `plan`). Show the model from `assistant.message.model`, not from `system/init`.
- cargo may not be on PATH in agent shells: use `%USERPROFILE%\.cargo\bin\cargo.exe`. The local git is old (no `git switch`).
- Before committing: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- The repo is public. Fixtures, docs, tests and commits must not contain the user's name, real local paths (use `C:\Users\user`, `C:\work\...`), emails or personal config lists (skills, agents, slash commands, plugins, MCP servers: cut them to 1-2 generic entries).
