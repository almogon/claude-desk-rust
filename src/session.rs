//! Process management for one `claude` CLI session.
//!
//! [`SessionHandle::spawn`] launches the CLI in headless stream-json mode and
//! starts tokio tasks (stdout reader, stderr reader, stdin writer, waiter) on a
//! private runtime. Everything is reported as [`SessionEvent`]s through a
//! `std::sync::mpsc` channel that the UI thread drains; after every event the
//! `notify` callback is invoked (the UI passes `ctx.request_repaint`).
//!
//! All `SessionHandle` methods are synchronous and non-blocking, so they can be
//! called directly from the UI thread.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc as tmpsc, oneshot};

use crate::protocol::{
    Attachment, AttachmentSummary, Incoming, PermissionDecision, PermissionMode,
    control_request_line, permission_response_line, user_message_blocks_line, user_message_line,
};

/// Callback invoked after every emitted event (e.g. `ctx.request_repaint`).
pub type Notifier = Arc<dyn Fn() + Send + Sync>;

/// Errors of the session layer. Messages are user-visible (Spanish).
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("No se encontró el ejecutable de Claude Code (`claude`). Instálalo o añádelo al PATH.")]
    ClaudeNotFound,
    #[error("No se pudo iniciar Claude Code: {0}")]
    Spawn(String),
    #[error("La sesión ya no está activa.")]
    Closed,
    #[error("Error ejecutando Claude Code: {0}")]
    Command(String),
}

/// Stable local identity of a session (independent of the CLI `session_id`,
/// which only arrives with the first `system/init`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionKey(pub u64);

impl SessionKey {
    /// Allocates a new process-unique key.
    pub fn new() -> SessionKey {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        SessionKey(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for SessionKey {
    fn default() -> Self {
        SessionKey::new()
    }
}

/// An event produced by (or about) a session. Feed it to
/// [`crate::state::AppState::apply`].
#[derive(Debug, Clone)]
pub struct SessionEvent {
    pub session_key: SessionKey,
    pub kind: SessionEventKind,
}

/// What happened.
#[derive(Debug, Clone)]
pub enum SessionEventKind {
    /// The process was spawned.
    Spawned { pid: Option<u32> },
    /// The client sent a user message (emitted by `send_user_message`).
    UserSent {
        text: String,
        attachments: Vec<AttachmentSummary>,
    },
    /// The client answered a permission request.
    PermissionResolved { request_id: String, allowed: bool },
    /// The client asked to interrupt the current turn.
    InterruptRequested,
    /// The client sent the `initialize` control request (to get slash commands).
    InitializeRequested { request_id: String },
    /// A parsed stdout message from the CLI.
    Incoming(Box<Incoming>),
    /// A stdout line that is not valid JSON.
    ParseError { line: String, error: String },
    /// A line written by the CLI to stderr.
    Stderr(String),
    /// The process ended. `error` is `Some` when it ended abnormally (and was
    /// not killed by the client).
    Exited { error: Option<String> },
}

/// Creates the event channel (sender goes to [`SessionHandle::spawn`], the
/// receiver stays with the UI).
pub fn event_channel() -> (Sender<SessionEvent>, Receiver<SessionEvent>) {
    channel()
}

/// How to launch a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Working directory of the CLI process.
    pub cwd: PathBuf,
    /// `--permission-mode` (always passed explicitly).
    pub permission_mode: PermissionMode,
    /// Optional `--model` (alias or full id).
    pub model: Option<String>,
    /// Optional `--resume <session_id>`.
    pub resume: Option<String>,
    /// Override of the `claude` executable (defaults to [`resolve_claude`]).
    pub claude_path: Option<PathBuf>,
}

impl SessionConfig {
    /// Config with defaults: `default` permission mode, no model, no resume.
    pub fn new(cwd: impl Into<PathBuf>) -> SessionConfig {
        SessionConfig {
            cwd: cwd.into(),
            permission_mode: PermissionMode::Default,
            model: None,
            resume: None,
            claude_path: None,
        }
    }

    /// The CLI arguments (without the executable).
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        a.push(self.permission_mode.as_str().to_owned());
        if let Some(m) = &self.model {
            a.push("--model".into());
            a.push(m.clone());
        }
        if let Some(r) = &self.resume {
            a.push("--resume".into());
            a.push(r.clone());
        }
        // Enables the task-list tools. `None` = no cache yet or CLI updated:
        // spawn without `--tools` so `system/init` teaches us the built-in set.
        if let Some(tools) = crate::tools::tools_arg() {
            a.push("--tools".into());
            a.push(tools);
        }
        a
    }
}

/// Locates the `claude` executable: PATH first (via `which`), then
/// `~/.local/bin/claude[.exe]`.
pub fn resolve_claude() -> Result<PathBuf, SessionError> {
    if let Ok(p) = which::which("claude") {
        return Ok(p);
    }
    if let Some(home) = dirs::home_dir() {
        let name = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let p = home.join(".local").join("bin").join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(SessionError::ClaudeNotFound)
}

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("claude-deck-io")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

#[derive(Clone)]
struct Emitter {
    key: SessionKey,
    tx: Sender<SessionEvent>,
    notify: Notifier,
}

impl Emitter {
    fn emit(&self, kind: SessionEventKind) {
        // A closed receiver just means the UI is gone.
        let _ = self.tx.send(SessionEvent {
            session_key: self.key,
            kind,
        });
        (self.notify)();
    }
}

/// Handle to a running CLI process. Dropping it closes stdin, which makes the
/// CLI exit cleanly after the current turn; use [`SessionHandle::kill`] to stop
/// it immediately.
pub struct SessionHandle {
    key: SessionKey,
    pid: Option<u32>,
    emitter: Emitter,
    stdin_tx: tmpsc::UnboundedSender<String>,
    kill_tx: Mutex<Option<oneshot::Sender<()>>>,
    killed: Arc<AtomicBool>,
}

impl SessionHandle {
    /// Spawns the CLI. Emits `Spawned` and then the stream of events.
    ///
    /// `key` is the local identity the caller already registered in
    /// [`crate::state::AppState`]. `events` receives every [`SessionEvent`];
    /// `notify` is called after each one.
    pub fn spawn(
        key: SessionKey,
        config: &SessionConfig,
        events: Sender<SessionEvent>,
        notify: Notifier,
    ) -> Result<SessionHandle, SessionError> {
        let exe = match &config.claude_path {
            Some(p) => p.clone(),
            None => resolve_claude()?,
        };
        let emitter = Emitter {
            key,
            tx: events,
            notify,
        };

        let _guard = runtime().enter();
        let mut cmd = Command::new(&exe);
        let args = config.args();
        let learning_tools = !args.iter().any(|a| a == "--tools");
        cmd.args(args)
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = cmd
            .spawn()
            .map_err(|e| SessionError::Spawn(format!("{e} ({})", exe.display())))?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        tracing::info!(?pid, cwd = %config.cwd.display(), "claude spawned");

        let killed = Arc::new(AtomicBool::new(false));
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));

        // stdin writer
        let (stdin_tx, mut stdin_rx) = tmpsc::unbounded_channel::<String>();
        runtime().spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = stdin_rx.recv().await {
                let mut buf = line.into_bytes();
                buf.push(b'\n');
                if stdin.write_all(&buf).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
            // Dropping stdin closes it: the CLI finishes the current turn and exits.
        });

        // stdout reader
        let em = emitter.clone();
        let reader = runtime().spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        match Incoming::parse_line(line) {
                            Ok(inc) => {
                                if let Incoming::Init(i) = &inc {
                                    crate::tools::on_init(
                                        learning_tools,
                                        i.claude_code_version.as_deref(),
                                        &i.tools,
                                    );
                                }
                                em.emit(SessionEventKind::Incoming(Box::new(inc)));
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "unparsable stdout line");
                                em.emit(SessionEventKind::ParseError {
                                    line: line.chars().take(300).collect(),
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "stdout read error");
                        break;
                    }
                }
            }
        });

        // stderr reader
        let em = emitter.clone();
        let tail = stderr_tail.clone();
        let err_task = runtime().spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                tracing::warn!(target: "claude_stderr", "{line}");
                {
                    let mut t = tail.lock().unwrap();
                    t.push_back(line.clone());
                    if t.len() > 5 {
                        t.pop_front();
                    }
                }
                em.emit(SessionEventKind::Stderr(line));
            }
        });

        // waiter: owns the child
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let em = emitter.clone();
        let killed_w = killed.clone();
        runtime().spawn(async move {
            let kill = async {
                if kill_rx.await.is_err() {
                    std::future::pending::<()>().await;
                }
            };
            let status = tokio::select! {
                s = child.wait() => s,
                _ = kill => {
                    let _ = child.kill().await;
                    child.wait().await
                }
            };
            let _ = reader.await;
            let _ = err_task.await;
            let error = if killed_w.load(Ordering::SeqCst) {
                None
            } else {
                match status {
                    Ok(s) if s.success() => None,
                    Ok(s) => {
                        let tail: Vec<String> =
                            stderr_tail.lock().unwrap().iter().cloned().collect();
                        let code = s
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "desconocido".into());
                        let mut msg = format!("Claude Code terminó con código {code}.");
                        if !tail.is_empty() {
                            msg.push(' ');
                            msg.push_str(&tail.join(" | "));
                        }
                        Some(msg)
                    }
                    Err(e) => Some(format!("Error esperando al proceso de Claude Code: {e}")),
                }
            };
            em.emit(SessionEventKind::Exited { error });
        });

        emitter.emit(SessionEventKind::Spawned { pid });
        let handle = SessionHandle {
            key,
            pid,
            emitter,
            stdin_tx,
            kill_tx: Mutex::new(Some(kill_tx)),
            killed,
        };
        // Ask for the slash command list (autocomplete). Failure is harmless.
        if let Ok(request_id) = handle.send_control(json!({"subtype":"initialize"})) {
            handle
                .emitter
                .emit(SessionEventKind::InitializeRequested { request_id });
        }
        Ok(handle)
    }

    /// Local key of this session.
    pub fn key(&self) -> SessionKey {
        self.key
    }

    /// OS process id, if known.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn send_line(&self, line: String) -> Result<(), SessionError> {
        self.stdin_tx.send(line).map_err(|_| SessionError::Closed)
    }

    /// Sends a user message (or a slash command such as `/usage`). Messages
    /// sent while a turn is running are queued by the CLI.
    pub fn send_user_message(&self, text: &str) -> Result<(), SessionError> {
        self.send_line(user_message_line(text))?;
        self.emitter.emit(SessionEventKind::UserSent {
            text: text.to_owned(),
            attachments: Vec::new(),
        });
        Ok(())
    }

    /// Sends a user message whose content is an array of blocks (images,
    /// pasted texts, then the user's text).
    pub fn send_user_message_blocks(
        &self,
        text: &str,
        attachments: &[Attachment],
    ) -> Result<(), SessionError> {
        self.send_line(user_message_blocks_line(text, attachments))?;
        self.emitter.emit(SessionEventKind::UserSent {
            text: text.to_owned(),
            attachments: attachments.iter().map(Attachment::summary).collect(),
        });
        Ok(())
    }

    /// Answers a `can_use_tool` request identified by `request_id`.
    pub fn respond_permission(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<(), SessionError> {
        self.send_line(permission_response_line(request_id, &decision))?;
        self.emitter.emit(SessionEventKind::PermissionResolved {
            request_id: request_id.to_owned(),
            allowed: matches!(decision, PermissionDecision::Allow { .. }),
        });
        Ok(())
    }

    /// Interrupts the running turn. The process stays alive; the CLI then
    /// emits a `result` with `error_during_execution`.
    pub fn interrupt(&self) -> Result<(), SessionError> {
        self.send_control(json!({"subtype":"interrupt"}))?;
        self.emitter.emit(SessionEventKind::InterruptRequested);
        Ok(())
    }

    /// Changes the model for following turns (alias or full id). The resolved
    /// id arrives in a `control_response`, which `AppState` applies.
    pub fn set_model(&self, model: &str) -> Result<(), SessionError> {
        self.send_control(json!({"subtype":"set_model","model":model}))
            .map(|_| ())
    }

    /// Changes the permission mode (takes effect immediately).
    pub fn set_permission_mode(&self, mode: PermissionMode) -> Result<(), SessionError> {
        self.send_control(json!({"subtype":"set_permission_mode","mode":mode.as_str()}))
            .map(|_| ())
    }

    /// Sends a control request and returns its request id.
    fn send_control(&self, request: serde_json::Value) -> Result<String, SessionError> {
        let id = uuid::Uuid::new_v4().to_string();
        self.send_line(control_request_line(&id, request))?;
        Ok(id)
    }

    /// Kills the process immediately. A later `Exited` event has `error: None`.
    pub fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        if let Some(tx) = self.kill_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

/// Runs `claude -p "/usage"` one-shot in `cwd` and returns the report text.
///
/// Blocks the calling thread (up to 60 s), so call it from a worker thread.
pub fn run_usage_command(cwd: &Path) -> Result<String, SessionError> {
    let exe = resolve_claude()?;
    runtime().block_on(async {
        let mut cmd = Command::new(&exe);
        cmd.arg("-p")
            .arg("/usage")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
            .await
            .map_err(|_| {
                SessionError::Command("tiempo de espera agotado ejecutando /usage".into())
            })?
            .map_err(|e| SessionError::Spawn(e.to_string()))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(SessionError::Command(format!(
                "/usage terminó con error: {}",
                err.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    })
}
