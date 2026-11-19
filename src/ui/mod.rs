//! egui front end. `DeckApp` owns the core `AppState`, the live
//! `SessionHandle`s and all UI-only state. Views never mutate the core
//! directly: they push [`Action`]s that `DeckApp` applies at the end of the frame.

pub mod chat;
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

use crate::history::{self, HistoryEntry, LoadedSession};
use crate::protocol::{Attachment, PermissionDecision, PermissionMode};
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
        }
    }
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
    },
    SetModel(String),
    SetMode(String),
    PickFolder(FolderTarget),
    RefreshHistory,
    RefreshUsage,
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
    ctx: egui::Context,
    events: Chan<SessionEvent>,
    notifier: Notifier,
    history_chan: Chan<Vec<HistoryEntry>>,
    load_chan: Chan<Result<HistoryView, String>>,
    usage_chan: Chan<Result<String, String>>,
    folder_chan: Chan<(String, FolderTarget)>,
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
            ctx,
            events: Chan::new(),
            notifier,
            history_chan: Chan::new(),
            load_chan: Chan::new(),
            usage_chan: Chan::new(),
            folder_chan: Chan::new(),
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
            let before = self.state.session(ev.session_key).map(|s| s.status.clone());
            self.state.apply(&ev);
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
                    self.spawn_session(f, None);
                }
                FolderTarget::NewConversation => {
                    self.spawn_session(f, None);
                }
            }
        }
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
    ) -> Option<SessionKey> {
        let key = SessionKey::new();
        let mode = PermissionMode::parse(&self.settings.mode).unwrap_or(PermissionMode::Default);
        let mut cfg = SessionConfig::new(&cwd);
        cfg.permission_mode = mode;
        cfg.model = Some(self.settings.model.clone());
        let mut n = NewSession::new(key, cwd);
        n.model = Some(self.settings.model.clone());
        n.mode = Some(mode.as_str().to_owned());
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
            Action::NewConversation => {
                let cwd = self.settings.folder.clone();
                self.spawn_session(cwd, None);
            }
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
                if self.spawn_session(cwd, Some((id, title, items))).is_some() {
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
                self.spawn_session(cwd, Some(r));
            }
            Action::Close(k) => {
                if let Some(h) = self.handles.get(&k) {
                    h.kill();
                }
            }
            Action::Delete(k) => self.remove_session(k),
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
            } => {
                if let Some(h) = self.handles.get(&key)
                    && let Err(e) = h.respond_permission(&request_id, decision)
                {
                    self.error = Some(e.to_string());
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
        let mut actions: Vec<Action> = Vec::new();
        self.shortcuts(ui.ctx(), &mut actions);

        header::show(ui, self, &mut actions);
        usage::show_bottom(ui, self, &mut actions);
        sidebar::show(ui, self, &mut actions);
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
