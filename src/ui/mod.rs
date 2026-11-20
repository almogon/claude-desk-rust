//! egui front end. `DeckApp` owns the core `AppState`, the live
//! `SessionHandle`s and all UI-only state. Views never mutate the core
//! directly: they push [`Action`]s that `DeckApp` applies at the end of the frame.

pub mod chat;
pub mod diff;
pub mod header;
pub mod input;
pub mod permission;
pub mod sidebar;
pub mod subagents;
pub mod theme;
pub mod usage;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui_commonmark::CommonMarkCache;
use serde::{Deserialize, Serialize};

use crate::git::{self, FileDiff, WorktreeInfo};
use crate::history::{self, HistoryEntry, LoadedSession};
use crate::protocol::{Attachment, Incoming, PermissionDecision, PermissionMode};
use crate::session::{
    Notifier, SessionConfig, SessionEvent, SessionEventKind, SessionHandle, SessionKey,
    run_usage_command,
};
use crate::state::{AppState, Item, NewSession, PendingReason, SessionStatus};

/// Models offered in the header (CLI aliases).
pub const MODELS: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];

/// Modes offered in the header: CLI value and Spanish label.
pub const MODES: [(&str, &str); 4] = [
    ("default", "Preguntar (default)"),
    ("acceptEdits", "Aceptar ediciones"),
    ("plan", "Plan"),
    ("bypassPermissions", "Sin permisos (peligroso)"),
];

/// Spanish label for a CLI permission mode.
pub fn mode_label(mode: &str) -> String {
    MODES
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, l)| (*l).to_owned())
        .unwrap_or_else(|| mode.to_owned())
}

/// Tab of the bottom panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BottomTab {
    #[default]
    Subagents,
    Usage,
}

/// Persisted user settings (eframe storage). Panel sizes are persisted by
/// egui itself (its memory is saved by eframe).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub folder: String,
    pub model: String,
    pub mode: String,
    pub bottom_tab: BottomTab,
    pub all_subagents: bool,
    /// Bottom panel folded to its tab bar.
    pub bottom_collapsed: bool,
    /// Show throwaway sessions (temp dir, `target/tmp`, `sandbox`) in the history.
    pub show_test_sessions: bool,
    /// Diff side panel open.
    pub diff_open: bool,
    /// Run new conversations in their own git worktree (when the folder is a repo).
    pub use_worktree: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            folder: dirs::home_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| ".".into()),
            model: "sonnet".into(),
            mode: "default".into(),
            bottom_tab: BottomTab::Subagents,
            all_subagents: false,
            bottom_collapsed: false,
            show_test_sessions: false,
            diff_open: false,
            use_worktree: false,
        }
    }
}

/// Result of one diff refresh.
pub enum DiffOutcome {
    NotRepo,
    Files(Vec<FileDiff>),
    Error(String),
}

/// Last diff computed, with the folder it belongs to.
pub struct DiffView {
    pub cwd: String,
    pub outcome: DiffOutcome,
}

/// A past session opened read-only.
pub struct HistoryView {
    pub entry: HistoryEntry,
    pub loaded: LoadedSession,
}

/// What the center area shows.
pub enum Selected {
    Session(SessionKey),
    History(Box<HistoryView>),
}

/// Intent produced by a view, applied by [`DeckApp::perform`].
pub enum Action {
    NewConversation,
    Select(SessionKey),
    OpenHistory(Box<HistoryEntry>),
    /// Resume a finished live session or the open history view.
    ResumeSession(SessionKey),
    ResumeHistory,
    Close(SessionKey),
    Delete(SessionKey),
    Send(SessionKey, String, Vec<Attachment>),
    Interrupt(SessionKey),
    Permission {
        key: SessionKey,
        request_id: String,
        decision: PermissionDecision,
        /// Extra instructions, sent as a user message right after an allow.
        note: Option<String>,
    },
    /// Approve an `ExitPlanMode` request (optionally with an edited plan in
    /// `updated_input`) and switch that session to `mode`.
    ApprovePlan {
        key: SessionKey,
        request_id: String,
        updated_input: serde_json::Value,
        mode: PermissionMode,
        /// Extra instructions, sent as a user message after the mode change.
        note: Option<String>,
    },
    SetModel(String),
    SetMode(String),
    PickFolder(FolderTarget),
    RefreshHistory,
    RefreshUsage,
    RefreshDiff,
    ToggleDiff,
}

/// What a picked folder is applied to (besides becoming the default).
#[derive(Debug, Clone, Copy)]
pub enum FolderTarget {
    /// Only the default folder for the next conversation.
    Default,
    /// Restart this conversation (nothing sent yet) in the new folder.
    Replace(SessionKey),
    /// Start a new conversation in the new folder.
    NewConversation,
}

/// A tiny typed channel pair.
pub struct Chan<T> {
    pub tx: Sender<T>,
    pub rx: Receiver<T>,
}

impl<T> Chan<T> {
    fn new() -> Self {
        let (tx, rx) = channel();
        Chan { tx, rx }
    }
}

/// The application.
pub struct DeckApp {
    pub state: AppState,
    pub handles: HashMap<SessionKey, SessionHandle>,
    pub settings: Settings,
    pub selected: Option<Selected>,
    pub history: Vec<HistoryEntry>,
    pub history_filter: String,
    /// Folder (full cwd) the history is restricted to; `None` = all.
    pub history_folder: Option<String>,
    pub history_busy: bool,
    /// Transcript markdown caches, one per session.
    pub md: HashMap<SessionKey, CommonMarkCache>,
    /// Cache for history/subagent views.
    pub shared_md: CommonMarkCache,
    pub drafts: HashMap<SessionKey, input::Draft>,
    pub deny_reasons: HashMap<String, String>,
    pub usage_busy: bool,
    pub usage_error: Option<String>,
    /// Transient error shown under the header.
    pub error: Option<String>,
    /// Last diff result (diff panel).
    pub diff: Option<DiffView>,
    pub diff_busy: bool,
    /// Path of the file selected in the diff panel.
    pub diff_selected: Option<String>,
    /// (active session, cwd) of the last diff request; a change triggers a refresh.
    diff_target: Option<(Option<SessionKey>, String)>,
    /// A refresh was requested while another one was running.
    diff_dirty: bool,
    /// Whether `settings.folder` is a git repo: (folder, answer). Computed on a
    /// thread whenever the folder changes.
    repo_cache: Option<(String, bool)>,
    repo_pending: Option<String>,
    ctx: egui::Context,
    events: Chan<SessionEvent>,
    notifier: Notifier,
    history_chan: Chan<Vec<HistoryEntry>>,
    load_chan: Chan<Result<HistoryView, String>>,
    usage_chan: Chan<Result<String, String>>,
    folder_chan: Chan<(String, FolderTarget)>,
    diff_chan: Chan<(String, DiffOutcome)>,
    repo_chan: Chan<(String, bool)>,
    worktree_chan: Chan<Result<WorktreeInfo, String>>,
    notice_chan: Chan<String>,
}

impl DeckApp {
    /// Builds the app, restoring persisted settings and installing the theme.
    pub fn new(cc: &eframe::CreationContext<'_>) -> DeckApp {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, "settings"))
            .unwrap_or_default();
        let ctx = cc.egui_ctx.clone();
        let repaint_ctx = ctx.clone();
        let notifier: Notifier = Arc::new(move || repaint_ctx.request_repaint());
        let mut app = DeckApp {
            state: AppState::new(),
            handles: HashMap::new(),
            settings,
            selected: None,
            history: Vec::new(),
            history_filter: String::new(),
            history_folder: None,
            history_busy: false,
            md: HashMap::new(),
            shared_md: CommonMarkCache::default(),
            drafts: HashMap::new(),
            deny_reasons: HashMap::new(),
            usage_busy: false,
            usage_error: None,
            error: None,
            diff: None,
            diff_busy: false,
            diff_selected: None,
            diff_target: None,
            diff_dirty: false,
            repo_cache: None,
            repo_pending: None,
            ctx,
            events: Chan::new(),
            notifier,
            history_chan: Chan::new(),
            load_chan: Chan::new(),
            usage_chan: Chan::new(),
            folder_chan: Chan::new(),
            diff_chan: Chan::new(),
            repo_chan: Chan::new(),
            worktree_chan: Chan::new(),
            notice_chan: Chan::new(),
        };
        app.refresh_history();
        app
    }

    /// Key of the selected live-or-finished session.
    pub fn active_key(&self) -> Option<SessionKey> {
        match &self.selected {
            Some(Selected::Session(k)) => Some(*k),
            _ => None,
        }
    }

    /// True if the session has a running process.
    pub fn is_alive(&self, key: SessionKey) -> bool {
        self.handles.contains_key(&key)
            && self
                .state
                .session(key)
                .is_some_and(|s| !matches!(s.status, SessionStatus::Finished { .. }))
    }

    /// True if the session is alive and nothing has been sent to it yet, so
    /// it can be restarted in another folder without losing anything.
    pub fn is_fresh(&self, key: SessionKey) -> bool {
        self.is_alive(key)
            && self.state.session(key).is_some_and(|s| {
                !s.transcript
                    .iter()
                    .any(|i| matches!(i, Item::UserText { .. }))
            })
    }

    fn drain(&mut self) {
        while let Ok(ev) = self.events.rx.try_recv() {
            let exited = matches!(ev.kind, SessionEventKind::Exited { .. });
            let is_result = matches!(
                &ev.kind,
                SessionEventKind::Incoming(i) if matches!(**i, Incoming::Result(_))
            );
            let before = self.state.session(ev.session_key).map(|s| s.status.clone());
            self.state.apply(&ev);
            if is_result && self.settings.diff_open && self.active_key() == Some(ev.session_key) {
                self.refresh_diff();
            }
            let after = self.state.session(ev.session_key).map(|s| s.status.clone());
            if before != after
                && matches!(
                    after,
                    Some(SessionStatus::Pending(
                        PendingReason::Permission | PendingReason::AwaitingInput
                    ))
                )
                && !self.ctx.input(|i| i.focused)
            {
                self.ctx
                    .send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
            }
            if exited {
                self.handles.remove(&ev.session_key);
            }
        }
        while let Ok(list) = self.history_chan.rx.try_recv() {
            self.history = list;
            self.history_busy = false;
        }
        while let Ok(r) = self.load_chan.rx.try_recv() {
            match r {
                Ok(v) => self.selected = Some(Selected::History(Box::new(v))),
                Err(e) => self.error = Some(e),
            }
        }
        while let Ok(r) = self.usage_chan.rx.try_recv() {
            self.usage_busy = false;
            match r {
                Ok(t) => {
                    self.usage_error = None;
                    self.state.set_usage_text(t);
                }
                Err(e) => self.usage_error = Some(e),
            }
        }
        while let Ok((f, target)) = self.folder_chan.rx.try_recv() {
            self.settings.folder = f.clone();
            match target {
                FolderTarget::Default => {}
                FolderTarget::Replace(k) => {
                    if self.is_fresh(k) {
                        self.remove_session(k);
                    }
                    self.new_conversation();
                }
                FolderTarget::NewConversation => self.new_conversation(),
            }
        }
        while let Ok((cwd, outcome)) = self.diff_chan.rx.try_recv() {
            self.diff_busy = false;
            self.diff = Some(DiffView { cwd, outcome });
            if std::mem::take(&mut self.diff_dirty) && self.settings.diff_open {
                self.refresh_diff();
            }
        }
        while let Ok(r) = self.repo_chan.rx.try_recv() {
            if self.repo_pending.as_deref() == Some(r.0.as_str()) {
                self.repo_pending = None;
            }
            self.repo_cache = Some(r);
        }
        while let Ok(r) = self.worktree_chan.rx.try_recv() {
            match r {
                Ok(info) => {
                    let cwd = info.path.display().to_string();
                    self.spawn_session(cwd, None, Some(info));
                }
                Err(e) => self.error = Some(e),
            }
        }
        while let Ok(msg) = self.notice_chan.rx.try_recv() {
            self.error = Some(msg);
        }
    }

    /// Folder the diff panel looks at: the active session's, else the default.
    fn diff_cwd(&self) -> String {
        self.active_key()
            .and_then(|k| self.state.session(k))
            .map(|s| s.cwd.clone())
            .unwrap_or_else(|| self.settings.folder.clone())
    }

    /// Recomputes the diff on a worker thread (queued if one is running).
    fn refresh_diff(&mut self) {
        let cwd = self.diff_cwd();
        self.diff_target = Some((self.active_key(), cwd.clone()));
        if self.diff_busy {
            self.diff_dirty = true;
            return;
        }
        self.diff_busy = true;
        let tx = self.diff_chan.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let dir = std::path::Path::new(&cwd);
            let outcome = if !git::is_repo(dir) {
                DiffOutcome::NotRepo
            } else {
                match git::diff(dir) {
                    Ok(f) => DiffOutcome::Files(f),
                    Err(e) => DiffOutcome::Error(e),
                }
            };
            let _ = tx.send((cwd, outcome));
            ctx.request_repaint();
        });
    }

    /// Per-frame bookkeeping: refresh the diff when the session/folder it
    /// shows changed, and probe whether the default folder is a git repo.
    fn housekeeping(&mut self) {
        if self.settings.diff_open {
            let now = (self.active_key(), self.diff_cwd());
            if self.diff_target.as_ref() != Some(&now) {
                self.refresh_diff();
            }
        } else {
            self.diff_target = None;
        }
        let folder = &self.settings.folder;
        let known = self.repo_cache.as_ref().is_some_and(|(f, _)| f == folder);
        if !known && self.repo_pending.as_ref() != Some(folder) {
            self.repo_pending = Some(folder.clone());
            let f = folder.clone();
            let tx = self.repo_chan.tx.clone();
            let ctx = self.ctx.clone();
            std::thread::spawn(move || {
                let ok = git::is_repo(std::path::Path::new(&f));
                let _ = tx.send((f, ok));
                ctx.request_repaint();
            });
        }
    }

    /// True if the default folder is known to be a git repo (cached).
    pub fn folder_is_repo(&self) -> bool {
        self.repo_cache
            .as_ref()
            .is_some_and(|(f, ok)| *ok && *f == self.settings.folder)
    }

    /// Starts a conversation in the default folder, in a fresh worktree when
    /// the option is on and the folder is a repo.
    fn new_conversation(&mut self) {
        let cwd = self.settings.folder.clone();
        if !(self.settings.use_worktree && self.folder_is_repo()) {
            self.spawn_session(cwd, None, None);
            return;
        }
        let tx = self.worktree_chan.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let r = git::repo_root(std::path::Path::new(&cwd))
                .ok_or_else(|| "La carpeta no es un repositorio git".to_owned())
                .and_then(|root| git::create_worktree_auto(&root));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    }

    /// Removes the worktree if it has no changes, otherwise keeps it; reports
    /// the outcome through `notice_chan`. Retries because Windows may still
    /// hold the folder for a moment after the process was killed.
    fn cleanup_worktree(&self, info: WorktreeInfo) {
        let tx = self.notice_chan.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let keep = format!(
                "Worktree conservado en {} (rama {})",
                info.path.display(),
                info.branch
            );
            let msg = if !git::worktree_is_pristine(&info) {
                keep
            } else {
                let mut last = String::new();
                let mut done = false;
                for attempt in 0..5 {
                    if attempt > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(600));
                    }
                    match git::remove_worktree(&info) {
                        Ok(()) => {
                            done = true;
                            break;
                        }
                        Err(e) => last = e,
                    }
                }
                if done {
                    "Worktree eliminado (sin cambios)".to_owned()
                } else {
                    format!("{keep}. No se pudo eliminar: {last}")
                }
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn refresh_history(&mut self) {
        if self.history_busy {
            return;
        }
        self.history_busy = true;
        let tx = self.history_chan.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let list = history::default_projects_dir()
                .map(|d| history::scan_history(&d, 100))
                .unwrap_or_default();
            let _ = tx.send(list);
            ctx.request_repaint();
        });
    }

    /// Starts a new CLI process and registers it. Returns the new key.
    fn spawn_session(
        &mut self,
        cwd: String,
        resume: Option<(String, String, Vec<Item>)>,
        worktree: Option<WorktreeInfo>,
    ) -> Option<SessionKey> {
        let key = SessionKey::new();
        let mode = PermissionMode::parse(&self.settings.mode).unwrap_or(PermissionMode::Default);
        let mut cfg = SessionConfig::new(&cwd);
        cfg.permission_mode = mode;
        cfg.model = Some(self.settings.model.clone());
        let mut n = NewSession::new(key, cwd);
        n.model = Some(self.settings.model.clone());
        n.mode = Some(mode.as_str().to_owned());
        n.worktree = worktree;
        if let Some((id, title, items)) = resume {
            cfg.resume = Some(id.clone());
            n.resume_session_id = Some(id);
            n.title = Some(title);
            n.transcript = items;
        }
        self.state.add_session(n);
        match SessionHandle::spawn(key, &cfg, self.events.tx.clone(), self.notifier.clone()) {
            Ok(h) => {
                self.handles.insert(key, h);
                self.selected = Some(Selected::Session(key));
                self.error = None;
                Some(key)
            }
            Err(e) => {
                self.state.remove_session(key);
                self.error = Some(e.to_string());
                None
            }
        }
    }

    fn remove_session(&mut self, key: SessionKey) {
        if let Some(h) = self.handles.remove(&key) {
            h.kill();
        }
        self.state.remove_session(key);
        self.md.remove(&key);
        self.drafts.remove(&key);
        if self.active_key() == Some(key) {
            self.selected = None;
        }
    }

    /// Applies one action.
    pub fn perform(&mut self, action: Action) {
        match action {
            Action::NewConversation => self.new_conversation(),
            Action::Select(k) => self.selected = Some(Selected::Session(k)),
            Action::OpenHistory(entry) => {
                let tx = self.load_chan.tx.clone();
                let ctx = self.ctx.clone();
                std::thread::spawn(move || {
                    let r = history::load_session(&entry.path)
                        .map(|loaded| HistoryView {
                            entry: (*entry).clone(),
                            loaded,
                        })
                        .map_err(|e| format!("No se pudo leer la conversación: {e}"));
                    let _ = tx.send(r);
                    ctx.request_repaint();
                });
            }
            Action::ResumeSession(k) => {
                let Some(s) = self.state.session(k) else {
                    return;
                };
                let Some(id) = s.session_id.clone() else {
                    self.error = Some("Esta conversación aún no tiene identificador.".into());
                    return;
                };
                let (cwd, title, items) = (s.cwd.clone(), s.title.clone(), s.transcript.clone());
                let worktree = s.worktree.clone();
                if self
                    .spawn_session(cwd, Some((id, title, items)), worktree)
                    .is_some()
                {
                    self.remove_session(k);
                    // remove_session cleared the selection if it pointed at `k`;
                    // spawn_session already selected the new one afterwards.
                }
            }
            Action::ResumeHistory => {
                let Some(Selected::History(h)) = &self.selected else {
                    return;
                };
                let cwd = h
                    .entry
                    .cwd
                    .clone()
                    .unwrap_or_else(|| self.settings.folder.clone());
                let r = (
                    h.entry.session_id.clone(),
                    h.loaded.title.clone(),
                    h.loaded.items.clone(),
                );
                self.spawn_session(cwd, Some(r), None);
            }
            Action::Close(k) => {
                if let Some(h) = self.handles.get(&k) {
                    h.kill();
                }
            }
            Action::Delete(k) => {
                let worktree = self.state.session(k).and_then(|s| s.worktree.clone());
                // remove_session kills the process if it is still alive.
                self.remove_session(k);
                if let Some(info) = worktree {
                    self.cleanup_worktree(info);
                }
            }
            Action::Send(k, text, attachments) => {
                if let Some(h) = self.handles.get(&k) {
                    let r = if attachments.is_empty() {
                        h.send_user_message(&text)
                    } else {
                        h.send_user_message_blocks(&text, &attachments)
                    };
                    if let Err(e) = r {
                        self.error = Some(e.to_string());
                    }
                }
            }
            Action::Interrupt(k) => {
                if let Some(h) = self.handles.get(&k) {
                    let _ = h.interrupt();
                }
            }
            Action::Permission {
                key,
                request_id,
                decision,
                note,
            } => {
                if let Some(h) = self.handles.get(&key)
                    && let Err(e) = h.respond_permission(&request_id, decision).and_then(|()| {
                        match note.as_deref().map(str::trim) {
                            Some(n) if !n.is_empty() => h.send_user_message(n),
                            _ => Ok(()),
                        }
                    })
                {
                    self.error = Some(e.to_string());
                }
            }
            Action::ApprovePlan {
                key,
                request_id,
                updated_input,
                mode,
                note,
            } => {
                if let Some(h) = self.handles.get(&key) {
                    let decision = PermissionDecision::Allow {
                        updated_input,
                        updated_permissions: Vec::new(),
                    };
                    // Same order as the spike: allow first, then the mode change.
                    if let Err(e) = h
                        .respond_permission(&request_id, decision)
                        .and_then(|()| h.set_permission_mode(mode))
                        .and_then(|()| match note.as_deref().map(str::trim) {
                            Some(n) if !n.is_empty() => h.send_user_message(n),
                            _ => Ok(()),
                        })
                    {
                        self.error = Some(e.to_string());
                    }
                }
            }
            Action::SetModel(m) => {
                if let Some(k) = self.active_key()
                    && self.is_alive(k)
                    && let Some(h) = self.handles.get(&k)
                {
                    let _ = h.set_model(&m);
                }
            }
            Action::SetMode(m) => {
                if let Some(k) = self.active_key()
                    && self.is_alive(k)
                    && let Some(h) = self.handles.get(&k)
                    && let Some(mode) = PermissionMode::parse(&m)
                {
                    let _ = h.set_permission_mode(mode);
                }
            }
            Action::PickFolder(target) => {
                let tx = self.folder_chan.tx.clone();
                let ctx = self.ctx.clone();
                let start = self.settings.folder.clone();
                std::thread::spawn(move || {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_title("Elegir carpeta de trabajo")
                        .set_directory(start)
                        .pick_folder()
                    {
                        let _ = tx.send((p.display().to_string(), target));
                        ctx.request_repaint();
                    }
                });
            }
            Action::RefreshHistory => self.refresh_history(),
            Action::RefreshDiff => self.refresh_diff(),
            Action::ToggleDiff => {
                self.settings.diff_open = !self.settings.diff_open;
                if self.settings.diff_open {
                    self.refresh_diff();
                }
            }
            Action::RefreshUsage => {
                if self.usage_busy {
                    return;
                }
                self.usage_busy = true;
                let cwd = self
                    .active_key()
                    .and_then(|k| self.state.session(k))
                    .map(|s| s.cwd.clone())
                    .unwrap_or_else(|| self.settings.folder.clone());
                let tx = self.usage_chan.tx.clone();
                let ctx = self.ctx.clone();
                std::thread::spawn(move || {
                    let r =
                        run_usage_command(std::path::Path::new(&cwd)).map_err(|e| e.to_string());
                    let _ = tx.send(r);
                    ctx.request_repaint();
                });
            }
        }
    }

    /// Ctrl+N new conversation, Esc stops the active turn, Ctrl+F focuses the history filter.
    fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use egui::{Key, Modifiers};
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            actions.push(Action::NewConversation);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            ctx.memory_mut(|m| m.request_focus(sidebar::filter_id()));
        }
        if let Some(k) = self.active_key()
            && self.is_alive(k)
            && self
                .state
                .session(k)
                .is_some_and(|s| s.status == SessionStatus::Running)
            && !input::popup_open(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            actions.push(Action::Interrupt(k));
        }
    }

    fn kill_all(&mut self) {
        for h in self.handles.values() {
            h.kill();
        }
    }
}

impl Drop for DeckApp {
    fn drop(&mut self) {
        self.kill_all();
    }
}

impl eframe::App for DeckApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        self.housekeeping();
        let mut actions: Vec<Action> = Vec::new();
        self.shortcuts(ui.ctx(), &mut actions);

        header::show(ui, self, &mut actions);
        usage::show_bottom(ui, self, &mut actions);
        sidebar::show(ui, self, &mut actions);
        diff::show(ui, self, &mut actions);
        chat::show(ui, self, &mut actions);

        for a in actions {
            self.perform(a);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "settings", &self.settings);
    }

    fn on_exit(&mut self) {
        self.kill_all();
    }
}

/// Rasterizes the 8-point star into a window icon (no external assets needed).
pub fn window_icon() -> egui::IconData {
    const N: u32 = 64;
    const SS: u32 = 4;
    let (outer, inner, c) = (30.0_f32, 8.5_f32, 32.0_f32);
    let poly: Vec<(f32, f32)> = (0..16)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / 16.0 - std::f32::consts::FRAC_PI_2;
            let r = if i % 2 == 0 { outer } else { inner };
            (c + r * a.cos(), c + r * a.sin())
        })
        .collect();
    let inside = |x: f32, y: f32| {
        let mut hit = false;
        let mut j = poly.len() - 1;
        for i in 0..poly.len() {
            let (xi, yi) = poly[i];
            let (xj, yj) = poly[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                hit = !hit;
            }
            j = i;
        }
        hit
    };
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for py in 0..N {
        for px in 0..N {
            let mut cnt = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = px as f32 + (sx as f32 + 0.5) / SS as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SS as f32;
                    if inside(x, y) {
                        cnt += 1;
                    }
                }
            }
            let a = (cnt * 255 / (SS * SS)) as u8;
            rgba.extend_from_slice(&[0xD9, 0x77, 0x57, a]);
        }
    }
    egui::IconData {
        rgba,
        width: N,
        height: N,
    }
}

/// Runs `add` inside a column that uses the full available width minus a
/// small side margin (transcript and input share it).
pub fn reading_column<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    const SIDE_MARGIN: f32 = 16.0;
    let avail = ui.available_width();
    let pad = SIDE_MARGIN.min(avail / 4.0);
    let w = (avail - 2.0 * pad).max(0.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(w);
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui)
        })
        .inner
    })
    .inner
}

/// Draws a filled status dot and returns its response.
pub fn dot(ui: &mut egui::Ui, color: egui::Color32, radius: f32) -> egui::Response {
    let (r, resp) =
        ui.allocate_exact_size(egui::vec2(radius * 2.0 + 4.0, 16.0), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), radius, color);
    resp
}

// ---------------------------------------------------------------------------
// Shared formatting helpers
// ---------------------------------------------------------------------------

/// Truncates to `max` chars, appending an ellipsis.
pub fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// Last path component (or the whole string).
pub fn short_dir(p: &str) -> String {
    let t = p.trim_end_matches(['\\', '/']);
    t.rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(t)
        .to_owned()
}

/// "850 ms", "12 s", "1 min 5 s".
pub fn fmt_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// "950", "12,3 k", "1,2 M".
pub fn fmt_tokens(n: u64) -> String {
    if n < 10_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1} k", n as f64 / 1000.0).replace('.', ",")
    } else {
        format!("{:.2} M", n as f64 / 1e6).replace('.', ",")
    }
}

/// "$0,0564".
pub fn fmt_cost(c: f64) -> String {
    format!("${c:.4}").replace('.', ",")
}
