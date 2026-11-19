# claude-deck

Personal desktop client for [Claude Code](https://claude.com/claude-code), written in Rust with egui/eframe. It wraps the installed `claude` CLI in headless `stream-json` mode, so it reuses your login, subscription, `CLAUDE.md` files, skills, agents and settings.

## Features (v0.1)

- **Visual system**: IBM Plex Sans/Mono embedded; session state is one colour across sidebar, header chip and chat (working = orange, needs permission = amber, waiting for you = blue, finished = grey, error = red).
- **Header**: icon, active conversation title with a state chip, and model / permission mode / folder dropdown chips. Model and mode apply live to the active session.
- **Sidebar**: conversations grouped as *Corriendo*, *Pendientes* and *Terminadas* (empty groups hidden), plus the Claude Code history from `~/.claude/projects` grouped by day, with search, folder filter, a toggle for test sessions (hidden by default) and resume.
- **Conversation view**: full-width reading column, streaming markdown, compact tool rows (grouped when consecutive), a live "working" line, inline permission cards (allow / allow always / deny with reason), answering Claude's questions (`AskUserQuestion`: single/multiple choice or free text), plan approval with a choice of the next mode (auto / accept edits / ask) and optional plan editing, auto-growing input (Enter sends, Shift+Enter new line), stop button.
- **Task list**: a collapsible strip above the input shows Claude's task list (pending / in progress / done, with a counter). The CLI's task tools are off in headless mode, so claude-deck enables them through `--tools` (see `docs/protocol.md` section 14).
- **Attachments**: Ctrl+V pastes screenshots/images and copied files, dropping files on the window attaches images or inserts `@path`, and long pasted text (over 15 lines or 1500 characters) becomes a removable chip.
- **Diff panel**: a "Cambios" button in the header toggles a right-hand panel with the working-tree changes against `HEAD` (changed and untracked files, per-file `+/-` counts, hunks with coloured lines). It refreshes when a turn ends or the active conversation changes.
- **Worktree per conversation**: the "Aislar en worktree" checkbox (enabled when the folder is a git repo) starts each new conversation in its own `git worktree` under `.claude/worktrees/` on a `deck/...` branch. Removing the conversation deletes the worktree if it has no changes and keeps it otherwise.
- **Bottom tabs** (collapsible):
  - *Subagentes*: table of the subagents working right now (status, type, description, current activity, duration and tool count); a finished one stays listed for 3 s with a green/red dot and then disappears. Click a row to see its transcript.
  - *Usage*: 5-hour and 7-day plan limits (from the CLI's `rate_limit_event`), per-session tokens/cost, day total, and the `/usage` output on demand.
- **Shortcuts**: Ctrl+N new conversation, Esc stop the active turn, Ctrl+F focus the history filter. The taskbar flashes when a permission or a finished turn needs you while the window is unfocused.

The UI is in Spanish.

## Requirements

- Windows 11 (other platforms compile but are untested).
- Rust stable (MSVC toolchain) and Visual Studio C++ Build Tools.
- Claude Code CLI installed and logged in (`claude` on `PATH`, or `~/.local/bin/claude.exe`).

## Build and run

```powershell
cargo build --release
.\target\release\claude-deck.exe
```

Checks: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. The live CLI test is opt-in: `cargo test -- --ignored`.

## How it works

Each conversation is a long-lived `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --permission-mode <mode>` process. Permission prompts arrive as `control_request`/`can_use_tool` and are answered from the UI; `interrupt`, `set_model` and `set_permission_mode` are sent as control requests. See [`docs/protocol.md`](docs/protocol.md) for the captured message formats.

## Limitations

- Parts of the stream-json protocol (control requests, `rate_limit_event`) are not officially documented and may change with CLI updates.
- Plan-limit percentages appear only after the CLI emits a `rate_limit_event`; use "Actualizar /usage" otherwise.
- The day cost total lives in memory and undercounts the first turn of a resumed session.

## License

MIT, see [`LICENSE`](LICENSE). The bundled IBM Plex fonts are under the SIL Open Font License ([`assets/fonts/LICENSE.txt`](assets/fonts/LICENSE.txt)).
