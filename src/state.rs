//! App model drawn by the UI, fed by [`AppState::apply`].
//!
//! The model is plain data (no locks, no UI types). The UI thread owns an
//! [`AppState`], drains the session event channel and calls `apply` for each
//! event, then renders `state.sessions()`, `state.grouped()`, etc.

use std::collections::HashMap;

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use serde_json::Value;

use crate::protocol::{
    AssistantMessage, AttachmentSummary, CanUseTool, ContentBlock, ControlRequestBody, Incoming,
    RateLimitInfo, RateWindow, ResultMessage, SlashCommand, StreamKind, TaskEvent, UserMessage,
    parse_commands,
};
use crate::session::{SessionEvent, SessionEventKind, SessionKey};

// ---------------------------------------------------------------------------
// Transcript items
// ---------------------------------------------------------------------------

/// Kind of a note in the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    System,
    Info,
    Error,
}

/// Summary of a finished turn (from `result`).
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    pub duration_ms: u64,
    pub duration_api_ms: u64,
    /// Session cumulative cost reported by the CLI at the end of this turn.
    pub total_cost_usd: f64,
    /// Cost attributable to this turn (difference of cumulative costs).
    pub turn_cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub is_error: bool,
    pub num_turns: u64,
    pub terminal_reason: Option<String>,
    pub at: DateTime<Local>,
}

/// A permission request (`can_use_tool`) shown inline in the transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionItem {
    pub request_id: String,
    /// e.g. `Write`, `Bash`, `AskUserQuestion`, `ExitPlanMode` (the UI may
    /// render the last two differently; v0.1 treats them as generic permission).
    pub tool_name: String,
    pub description: Option<String>,
    pub input: Value,
    pub tool_use_id: Option<String>,
    /// Raw `permission_suggestions` of the request.
    pub suggestions: Vec<Value>,
    /// `None` while pending, `Some(true)` allowed, `Some(false)` denied.
    pub resolved: Option<bool>,
}

impl PermissionItem {
    /// First `addRules` suggestion: the "allow always" rule the CLI offers.
    pub fn add_rules_suggestion(&self) -> Option<&Value> {
        self.suggestions
            .iter()
            .find(|s| s.get("type").and_then(Value::as_str) == Some("addRules"))
    }

    /// The `setMode acceptEdits` suggestion, if offered.
    pub fn accept_edits_suggestion(&self) -> Option<&Value> {
        self.suggestions.iter().find(|s| {
            s.get("type").and_then(Value::as_str) == Some("setMode")
                && s.get("mode").and_then(Value::as_str) == Some("acceptEdits")
        })
    }

    /// True for tools that deserve a dedicated UI (`AskUserQuestion`, `ExitPlanMode`).
    pub fn is_special_tool(&self) -> bool {
        matches!(self.tool_name.as_str(), "AskUserQuestion" | "ExitPlanMode")
    }
}

/// One element of a transcript (main session or subagent).
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    UserText {
        text: String,
        at: DateTime<Local>,
        /// Images / pasted texts sent along (chips in the bubble).
        attachments: Vec<AttachmentSummary>,
    },
    /// Assistant text. `finalized == false` while it is still streaming.
    AssistantText {
        text: String,
        finalized: bool,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// `(text, is_error)` once the tool result arrived.
        result: Option<(String, bool)>,
    },
    PermissionRequest(PermissionItem),
    Note {
        kind: NoteKind,
        text: String,
        at: DateTime<Local>,
    },
    TurnSummary(TurnSummary),
}

// ---------------------------------------------------------------------------
// Subagents
// ---------------------------------------------------------------------------

/// Subagent status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
}

/// A subagent launched through the `Agent`/`Task` tool.
#[derive(Debug, Clone)]
pub struct Subagent {
    /// Id of the `Agent` tool_use that launched it (index key).
    pub tool_use_id: String,
    /// `agent_id` / `task_id`, known after `task_started`.
    pub agent_id: Option<String>,
    pub description: String,
    pub subagent_type: Option<String>,
    pub model: Option<String>,
    pub status: SubagentStatus,
    pub started_at: DateTime<Local>,
    pub ended_at: Option<DateTime<Local>>,
    /// Short text: last tool used, or the beginning of the last text.
    pub last_activity: Option<String>,
    pub message_count: u32,
    pub tool_count: u32,
    /// Final report (`task_notification.summary`).
    pub summary: Option<String>,
    /// The subagent's own transcript.
    pub items: Vec<Item>,
}

impl Subagent {
    fn new(tool_use_id: &str, now: DateTime<Local>) -> Subagent {
        Subagent {
            tool_use_id: tool_use_id.to_owned(),
            agent_id: None,
            description: String::new(),
            subagent_type: None,
            model: None,
            status: SubagentStatus::Running,
            started_at: now,
            ended_at: None,
            last_activity: None,
            message_count: 0,
            tool_count: 0,
            summary: None,
            items: Vec::new(),
        }
    }
}

fn status_from_str(s: &str) -> Option<SubagentStatus> {
    match s {
        "completed" | "complete" | "done" | "success" => Some(SubagentStatus::Completed),
        "failed" | "error" | "killed" | "cancelled" | "canceled" | "stopped" => {
            Some(SubagentStatus::Failed)
        }
        "running" | "in_progress" | "started" | "pending" => Some(SubagentStatus::Running),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

/// Accumulated token counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenTotals {
    fn add(&mut self, u: &crate::protocol::Usage) {
        self.input += u.input_tokens;
        self.output += u.output_tokens;
        self.cache_read += u.cache_read_input_tokens;
        self.cache_creation += u.cache_creation_input_tokens;
    }
}

/// Usage of one session.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionUsage {
    pub tokens: TokenTotals,
    /// Cumulative cost as last reported by the CLI. For a resumed session it
    /// already includes the cost of the resumed history.
    pub cost_usd: f64,
    /// Number of real turns (results with `num_turns > 0`).
    pub turns: u32,
}

/// Usage of all sessions of the app for one local calendar day. In-memory
/// only: it restarts from zero when the app restarts.
#[derive(Debug, Clone, PartialEq)]
pub struct DayUsage {
    pub date: NaiveDate,
    pub tokens: TokenTotals,
    pub cost_usd: f64,
    pub turns: u32,
}

/// A rate-limit window converted for display.
#[derive(Debug, Clone, PartialEq)]
pub struct RateWindowState {
    /// Fraction `0.0..=1.0`.
    pub utilization: f64,
    pub resets_at: DateTime<Local>,
    pub status: Option<String>,
    /// When the event carrying this window was received.
    pub received_at: DateTime<Local>,
}

/// Latest known subscription windows (merged across `rate_limit_event`s).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimits {
    pub five_hour: Option<RateWindowState>,
    pub seven_day: Option<RateWindowState>,
}

/// Text of the last `/usage` report.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageText {
    pub text: String,
    pub at: DateTime<Local>,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Why a session needs attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReason {
    /// Waiting for the user to answer a permission request.
    Permission,
    /// Turn finished; waiting for the next user message.
    AwaitingInput,
}

/// Lifecycle of a session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    Starting,
    Running,
    Pending(PendingReason),
    Finished { error: Option<String> },
}

/// UI grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionGroup {
    /// "Corriendo"
    Running,
    /// "Pendientes"
    Pending,
    /// "Terminadas"
    Finished,
}

/// Parameters to register a session in [`AppState::add_session`].
#[derive(Debug, Clone)]
pub struct NewSession {
    pub key: SessionKey,
    pub cwd: String,
    /// Requested model (alias), shown until the real one is known.
    pub model: Option<String>,
    pub mode: Option<String>,
    /// Set when resuming an existing session.
    pub resume_session_id: Option<String>,
    /// Title for resumed sessions (otherwise derived from the first prompt).
    pub title: Option<String>,
    /// Transcript loaded from history (resumed sessions).
    pub transcript: Vec<Item>,
}

impl NewSession {
    /// Minimal fresh session.
    pub fn new(key: SessionKey, cwd: impl Into<String>) -> NewSession {
        NewSession {
            key,
            cwd: cwd.into(),
            model: None,
            mode: None,
            resume_session_id: None,
            title: None,
            transcript: Vec::new(),
        }
    }
}

/// One session as the UI sees it.
#[derive(Debug, Clone)]
pub struct Session {
    pub key: SessionKey,
    /// CLI session id; known after the first `system/init`.
    pub session_id: Option<String>,
    pub cwd: String,
    /// Real model (from `assistant.message.model`), or the requested one until then.
    pub model: Option<String>,
    /// Current permission mode (CLI spelling).
    pub mode: Option<String>,
    /// First prompt, truncated.
    pub title: String,
    pub status: SessionStatus,
    pub created_at: DateTime<Local>,
    pub updated_at: DateTime<Local>,
    pub transcript: Vec<Item>,
    pub subagents: Vec<Subagent>,
    pub usage: SessionUsage,
    /// Slash commands for autocomplete (filled from `initialize` / `system/init`).
    pub commands: Vec<SlashCommand>,
    /// Request id of the `initialize` control request we sent.
    initialize_request: Option<String>,
    resumed: bool,
    outstanding_turns: u32,
    last_total_cost: Option<f64>,
    /// Item index of the assistant text currently streaming.
    streaming_text: Option<usize>,
}

const TITLE_MAX: usize = 60;

/// Status of one task in the model's task list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// One task of the model's task list (`TodoWrite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    /// Present-continuous wording shown while the task is in progress.
    pub active_form: Option<String>,
    pub status: TodoStatus,
}

/// Id from a `TaskCreate` result text such as `Task #1 created successfully: ...`.
fn task_id_from_text(text: &str) -> Option<String> {
    let rest = text.split("Task #").nth(1)?;
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!id.is_empty()).then_some(id)
}

/// Parses `input.todos[]` of a `TodoWrite` call; `None` if it has no `todos` array.
fn parse_todos(input: &Value) -> Option<Vec<TodoItem>> {
    let arr = input.get("todos")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|t| {
                let content = t.get("content")?.as_str()?.to_owned();
                let status = match t.get("status").and_then(Value::as_str) {
                    Some("in_progress") => TodoStatus::InProgress,
                    Some("completed") => TodoStatus::Completed,
                    _ => TodoStatus::Pending,
                };
                let active_form = t
                    .get("activeForm")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                Some(TodoItem {
                    content,
                    active_form,
                    status,
                })
            })
            .collect(),
    )
}

/// Filters slash commands for autocomplete. `query` is the text after the `/`
/// (case-insensitive). Prefix matches (name or alias) come first, then
/// substring matches; within each group builtin commands first, then the
/// original order.
pub fn filter_commands<'a>(
    cmds: &'a [SlashCommand],
    query: &str,
    limit: usize,
) -> Vec<&'a SlashCommand> {
    let q = query.to_lowercase();
    let mut prefix: Vec<&SlashCommand> = Vec::new();
    let mut contains: Vec<&SlashCommand> = Vec::new();
    for c in cmds {
        let names = std::iter::once(&c.name)
            .chain(c.aliases.iter())
            .map(|n| n.to_lowercase());
        let (mut is_prefix, mut is_contains) = (false, false);
        for n in names {
            if n.starts_with(&q) {
                is_prefix = true;
            } else if n.contains(&q) {
                is_contains = true;
            }
        }
        if is_prefix {
            prefix.push(c);
        } else if is_contains {
            contains.push(c);
        }
    }
    // `sort_by_key` is stable, so original order is kept within each class.
    prefix.sort_by_key(|c| !c.builtin);
    contains.sort_by_key(|c| !c.builtin);
    prefix.into_iter().chain(contains).take(limit).collect()
}

/// Collapses whitespace and truncates to `max` chars with an ellipsis.
pub fn truncate_title(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let mut s: String = collapsed.chars().take(max).collect();
        s.push('…');
        s
    }
}

impl Session {
    fn new(n: NewSession, now: DateTime<Local>) -> Session {
        Session {
            key: n.key,
            session_id: n.resume_session_id.clone(),
            cwd: n.cwd,
            model: n.model,
            mode: n.mode,
            title: n.title.unwrap_or_default(),
            status: SessionStatus::Starting,
            created_at: now,
            updated_at: now,
            transcript: n.transcript,
            subagents: Vec::new(),
            usage: SessionUsage::default(),
            commands: Vec::new(),
            initialize_request: None,
            resumed: n.resume_session_id.is_some(),
            outstanding_turns: 0,
            last_total_cost: None,
            streaming_text: None,
        }
    }

    /// Current task list of the main conversation, from the last `TodoWrite`
    /// call (its `todos` array replaces the whole list). Subagent items live in
    /// `subagents`, so they never show up here. Tolerant: calls or entries with
    /// an unknown shape are skipped.
    pub fn todos(&self) -> Vec<TodoItem> {
        // (task id, item). `TodoWrite` entries have no id.
        let mut list: Vec<(Option<String>, TodoItem)> = Vec::new();
        let mut created = 0usize;
        for it in &self.transcript {
            let Item::ToolUse {
                name,
                input,
                result,
                ..
            } = it
            else {
                continue;
            };
            let failed = matches!(result, Some((_, true)));
            match name.as_str() {
                "TodoWrite" => {
                    if let Some(v) = parse_todos(input) {
                        list = v.into_iter().map(|t| (None, t)).collect();
                    }
                }
                "TaskCreate" if !failed => {
                    let Some(subject) = input.get("subject").and_then(Value::as_str) else {
                        continue;
                    };
                    created += 1;
                    // Ids only come back in the result ("Task #N created ...");
                    // without it assume they are sequential from 1.
                    let id = result
                        .as_ref()
                        .and_then(|(text, _)| task_id_from_text(text))
                        .unwrap_or_else(|| created.to_string());
                    list.push((
                        Some(id),
                        TodoItem {
                            content: subject.to_owned(),
                            active_form: input
                                .get("activeForm")
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned),
                            status: TodoStatus::Pending,
                        },
                    ));
                }
                "TaskUpdate" if !failed => {
                    let id = match input.get("taskId") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Number(n)) => n.to_string(),
                        _ => continue,
                    };
                    let Some(pos) = list.iter().position(|(i, _)| i.as_deref() == Some(&id)) else {
                        continue;
                    };
                    match input.get("status").and_then(Value::as_str) {
                        Some("deleted") => {
                            list.remove(pos);
                            continue;
                        }
                        Some("pending") => list[pos].1.status = TodoStatus::Pending,
                        Some("in_progress") => list[pos].1.status = TodoStatus::InProgress,
                        Some("completed") => list[pos].1.status = TodoStatus::Completed,
                        _ => {}
                    }
                    if let Some(s) = input.get("subject").and_then(Value::as_str) {
                        list[pos].1.content = s.to_owned();
                    }
                    if let Some(s) = input.get("activeForm").and_then(Value::as_str) {
                        list[pos].1.active_form = Some(s.to_owned()).filter(|s| !s.is_empty());
                    }
                }
                _ => {}
            }
        }
        list.into_iter().map(|(_, t)| t).collect()
    }

    /// Group for the Corriendo / Pendientes / Terminadas lists.
    pub fn group(&self) -> SessionGroup {
        match self.status {
            SessionStatus::Starting | SessionStatus::Running => SessionGroup::Running,
            SessionStatus::Pending(_) => SessionGroup::Pending,
            SessionStatus::Finished { .. } => SessionGroup::Finished,
        }
    }

    /// Finds a subagent by the id of the `Agent` tool_use that launched it.
    pub fn subagent(&self, tool_use_id: &str) -> Option<&Subagent> {
        self.subagents.iter().find(|s| s.tool_use_id == tool_use_id)
    }

    /// Permission requests still waiting for an answer.
    pub fn pending_permissions(&self) -> impl Iterator<Item = &PermissionItem> {
        self.transcript.iter().filter_map(|i| match i {
            Item::PermissionRequest(p) if p.resolved.is_none() => Some(p),
            _ => None,
        })
    }

    fn is_finished(&self) -> bool {
        matches!(self.status, SessionStatus::Finished { .. })
    }

    fn note(&mut self, kind: NoteKind, text: String, now: DateTime<Local>) {
        self.transcript.push(Item::Note {
            kind,
            text,
            at: now,
        });
    }

    // -- streaming text ---------------------------------------------------

    /// Closes the currently streaming text (if any), dropping it if empty.
    fn close_streaming(&mut self) {
        if let Some(i) = self.streaming_text.take()
            && let Some(Item::AssistantText { text, finalized }) = self.transcript.get_mut(i)
        {
            *finalized = true;
            if text.is_empty() && i + 1 == self.transcript.len() {
                self.transcript.pop();
            }
        }
    }

    fn stream_delta(&mut self, delta: &str) {
        let idx = match self.streaming_text {
            Some(i) if matches!(self.transcript.get(i), Some(Item::AssistantText { .. })) => i,
            _ => {
                self.transcript.push(Item::AssistantText {
                    text: String::new(),
                    finalized: false,
                });
                let i = self.transcript.len() - 1;
                self.streaming_text = Some(i);
                i
            }
        };
        if let Item::AssistantText { text, .. } = &mut self.transcript[idx] {
            text.push_str(delta);
        }
    }

    /// A complete text block arrived: replace/finalize the streamed item, or
    /// append a new finalized one when nothing was streaming.
    fn finalize_text(&mut self, full: &str) {
        if let Some(i) = self.streaming_text.take()
            && let Some(Item::AssistantText { text, finalized }) = self.transcript.get_mut(i)
        {
            *text = full.to_owned();
            *finalized = true;
            return;
        }
        self.transcript.push(Item::AssistantText {
            text: full.to_owned(),
            finalized: true,
        });
    }

    // -- incoming ----------------------------------------------------------

    fn apply_incoming(&mut self, inc: &Incoming, now: DateTime<Local>) {
        match inc {
            Incoming::Init(i) => {
                if let Some(id) = &i.session_id {
                    self.session_id = Some(id.clone());
                }
                if let Some(m) = &i.permission_mode {
                    self.mode = Some(m.clone());
                }
                if self.model.is_none() {
                    self.model = i.model.clone();
                }
                if self.commands.is_empty() {
                    self.commands = i
                        .slash_commands
                        .iter()
                        .map(|n| SlashCommand {
                            name: n.clone(),
                            description: String::new(),
                            argument_hint: String::new(),
                            aliases: Vec::new(),
                            builtin: false,
                        })
                        .collect();
                }
            }
            Incoming::Status(s) => {
                if let Some(m) = &s.permission_mode {
                    self.mode = Some(m.clone());
                }
            }
            Incoming::PermissionDenied {
                tool_name, message, ..
            } => {
                let text = if message.is_empty() {
                    format!("Permiso denegado para {tool_name}.")
                } else {
                    format!("Permiso denegado para {tool_name}: {message}")
                };
                self.note(NoteKind::System, text, now);
            }
            Incoming::Task(t) => self.apply_task(t, now),
            Incoming::OtherSystem { .. } | Incoming::Unknown(_) => {}
            Incoming::StreamEvent(se) => {
                if se.parent_tool_use_id.is_some() {
                    return; // subagent partials are not rendered
                }
                match &se.kind {
                    StreamKind::MessageStart { model } => {
                        self.close_streaming();
                        if let Some(m) = model {
                            self.set_real_model(m);
                        }
                    }
                    StreamKind::BlockStart { block, .. } => {
                        self.close_streaming();
                        if matches!(block, ContentBlock::Text(_)) {
                            self.stream_delta("");
                        }
                    }
                    StreamKind::TextDelta { text, .. } => self.stream_delta(text),
                    StreamKind::BlockStop { .. } | StreamKind::MessageStop => {
                        self.close_streaming()
                    }
                    _ => {}
                }
            }
            Incoming::Assistant(a) => self.apply_assistant(a, now),
            Incoming::User(u) => self.apply_user(u, now),
            Incoming::RateLimit(_) => {} // global, handled by AppState
            Incoming::Result(r) => {
                // handled by AppState (needs the global usage); see AppState::apply
                let _ = r;
            }
            Incoming::ControlRequest(cr) => {
                if let ControlRequestBody::CanUseTool(c) = &cr.body {
                    self.add_permission(&cr.request_id, c);
                }
            }
            Incoming::ControlResponse(cr) => {
                let is_init = self.initialize_request.as_deref() == Some(cr.request_id.as_str());
                if is_init {
                    self.initialize_request = None;
                }
                if cr.ok {
                    if let Some(c) = cr.response.get("commands") {
                        let cmds = parse_commands(c);
                        if !cmds.is_empty() {
                            self.commands = cmds;
                        }
                    }
                    if let Some(m) = cr.response.get("model").and_then(Value::as_str) {
                        self.model = Some(m.to_owned());
                    }
                    if let Some(m) = cr.response.get("mode").and_then(Value::as_str) {
                        self.mode = Some(m.to_owned());
                    }
                } else if !is_init {
                    let err = cr
                        .error
                        .clone()
                        .unwrap_or_else(|| "error desconocido".into());
                    self.note(
                        NoteKind::Error,
                        format!("Claude Code rechazó una petición de control: {err}"),
                        now,
                    );
                }
            }
        }
    }

    fn set_real_model(&mut self, m: &str) {
        if !m.is_empty() && m != "<synthetic>" {
            self.model = Some(m.to_owned());
        }
    }

    fn add_permission(&mut self, request_id: &str, c: &CanUseTool) {
        let exists = self
            .transcript
            .iter()
            .any(|i| matches!(i, Item::PermissionRequest(p) if p.request_id == request_id));
        if !exists {
            self.close_streaming();
            self.transcript
                .push(Item::PermissionRequest(PermissionItem {
                    request_id: request_id.to_owned(),
                    tool_name: c.tool_name.clone(),
                    description: c.description.clone(),
                    input: c.input.clone(),
                    tool_use_id: c.tool_use_id.clone(),
                    suggestions: c.permission_suggestions.clone(),
                    resolved: None,
                }));
        }
        if !self.is_finished() {
            self.status = SessionStatus::Pending(PendingReason::Permission);
        }
    }

    fn resolve_permission(&mut self, request_id: &str, allowed: bool) {
        for i in &mut self.transcript {
            if let Item::PermissionRequest(p) = i
                && p.request_id == request_id
            {
                p.resolved = Some(allowed);
            }
        }
        if self.status == SessionStatus::Pending(PendingReason::Permission)
            && self.pending_permissions().next().is_none()
        {
            self.status = SessionStatus::Running;
        }
    }

    /// Resolves which subagent (index) a message belongs to, if any.
    fn subagent_index(
        &mut self,
        parent: Option<&str>,
        agent_id: Option<&str>,
        now: DateTime<Local>,
    ) -> Option<usize> {
        if let Some(p) = parent {
            if let Some(i) = self.subagents.iter().position(|s| s.tool_use_id == p) {
                return Some(i);
            }
            self.subagents.push(Subagent::new(p, now));
            return Some(self.subagents.len() - 1);
        }
        let a = agent_id?;
        self.subagents
            .iter()
            .position(|s| s.agent_id.as_deref() == Some(a))
    }

    fn apply_assistant(&mut self, a: &AssistantMessage, now: DateTime<Local>) {
        if let Some(si) =
            self.subagent_index(a.parent_tool_use_id.as_deref(), a.agent_id.as_deref(), now)
        {
            let sub = &mut self.subagents[si];
            sub.message_count += 1;
            if let Some(m) = &a.model
                && m != "<synthetic>"
            {
                sub.model = Some(m.clone());
            }
            for b in &a.content {
                match b {
                    ContentBlock::Text(t) if !t.is_empty() => {
                        sub.items.push(Item::AssistantText {
                            text: t.clone(),
                            finalized: true,
                        });
                        sub.last_activity = Some(truncate_title(t, 80));
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        sub.tool_count += 1;
                        sub.last_activity = Some(name.clone());
                        push_tool_use(&mut sub.items, id, name, input);
                    }
                    _ => {}
                }
            }
            return;
        }

        if let Some(m) = &a.model {
            self.set_real_model(m);
        }
        if self.session_id.is_none() {
            self.session_id = a.session_id.clone();
        }
        for b in &a.content {
            match b {
                ContentBlock::Text(t) => {
                    if t.is_empty() {
                        self.close_streaming();
                    } else {
                        self.finalize_text(t);
                    }
                }
                ContentBlock::ToolUse { id, name, input } => {
                    self.close_streaming();
                    push_tool_use(&mut self.transcript, id, name, input);
                    if is_agent_tool(name) && self.subagent(id).is_none() {
                        let mut sub = Subagent::new(id, now);
                        sub.description = input
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        sub.subagent_type = input
                            .get("subagent_type")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        self.subagents.push(sub);
                    }
                }
                _ => {}
            }
        }
    }

    fn apply_user(&mut self, u: &UserMessage, now: DateTime<Local>) {
        if let Some(si) =
            self.subagent_index(u.parent_tool_use_id.as_deref(), u.agent_id.as_deref(), now)
        {
            let sub = &mut self.subagents[si];
            sub.message_count += 1;
            for b in &u.content {
                match b {
                    ContentBlock::Text(t) => sub.items.push(Item::UserText {
                        text: t.clone(),
                        at: now,
                        attachments: Vec::new(),
                    }),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        set_tool_result(&mut sub.items, tool_use_id, content, *is_error);
                    }
                    _ => {}
                }
            }
            return;
        }

        for b in &u.content {
            match b {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    set_tool_result(&mut self.transcript, tool_use_id, content, *is_error);
                    // The Agent result closes the subagent if no notification did.
                    if let Some(sub) = self
                        .subagents
                        .iter_mut()
                        .find(|s| &s.tool_use_id == tool_use_id)
                        && sub.status == SubagentStatus::Running
                    {
                        sub.status = if *is_error {
                            SubagentStatus::Failed
                        } else {
                            SubagentStatus::Completed
                        };
                        sub.ended_at.get_or_insert(now);
                    }
                }
                // The CLI echoes this on interrupt; `InterruptRequested` already
                // produced a note. Other plain user text from the CLI is ignored
                // (our own messages are recorded by `UserSent`).
                ContentBlock::Text(_) => {}
                _ => {}
            }
        }
    }

    fn apply_task(&mut self, t: &TaskEvent, now: DateTime<Local>) {
        match t {
            TaskEvent::Started {
                task_id,
                tool_use_id,
                description,
                subagent_type,
                ..
            } => {
                let Some(tu) = tool_use_id else { return };
                let idx = self.subagent_index(Some(tu), None, now).unwrap();
                let s = &mut self.subagents[idx];
                s.agent_id = Some(task_id.clone());
                if !description.is_empty() {
                    s.description = description.clone();
                }
                if subagent_type.is_some() {
                    s.subagent_type = subagent_type.clone();
                }
                s.status = SubagentStatus::Running;
            }
            TaskEvent::Progress {
                task_id,
                tool_use_id,
                description,
                last_tool_name,
                usage,
            } => {
                let Some(s) = self.find_task(task_id, tool_use_id.as_deref()) else {
                    return;
                };
                if let Some(n) = last_tool_name {
                    s.last_activity = Some(n.clone());
                } else if let Some(d) = description {
                    s.last_activity = Some(truncate_title(d, 80));
                }
                s.tool_count = s.tool_count.max(usage.tool_uses as u32);
            }
            TaskEvent::Updated {
                task_id,
                status,
                end_time_ms,
            } => {
                let Some(s) = self.find_task(task_id, None) else {
                    return;
                };
                if let Some(st) = status.as_deref().and_then(status_from_str) {
                    s.status = st;
                }
                if let Some(ms) = end_time_ms {
                    if let Some(dt) = Local.timestamp_millis_opt(*ms).single() {
                        s.ended_at = Some(dt);
                    }
                } else if s.status != SubagentStatus::Running {
                    s.ended_at.get_or_insert(now);
                }
            }
            TaskEvent::Notification {
                task_id,
                tool_use_id,
                status,
                summary,
                usage,
            } => {
                let Some(s) = self.find_task(task_id, tool_use_id.as_deref()) else {
                    return;
                };
                if let Some(st) = status.as_deref().and_then(status_from_str) {
                    s.status = st;
                }
                if s.status != SubagentStatus::Running {
                    s.ended_at.get_or_insert(now);
                }
                if summary.is_some() {
                    s.summary = summary.clone();
                }
                s.tool_count = s.tool_count.max(usage.tool_uses as u32);
            }
        }
    }

    fn find_task(&mut self, task_id: &str, tool_use_id: Option<&str>) -> Option<&mut Subagent> {
        let pos = self
            .subagents
            .iter()
            .position(|s| s.agent_id.as_deref() == Some(task_id))
            .or_else(|| {
                tool_use_id.and_then(|t| self.subagents.iter().position(|s| s.tool_use_id == t))
            })?;
        let s = &mut self.subagents[pos];
        if s.agent_id.is_none() {
            s.agent_id = Some(task_id.to_owned());
        }
        Some(s)
    }

    /// Applies a `result`; returns the cost delta of the turn.
    fn apply_result(&mut self, r: &ResultMessage, now: DateTime<Local>) -> f64 {
        self.close_streaming();
        if self.session_id.is_none() {
            self.session_id = r.session_id.clone();
        }

        let delta = match self.last_total_cost {
            Some(prev) => (r.total_cost_usd - prev).max(0.0),
            // First result of a resumed session: the cumulative cost includes
            // history we cannot attribute to today.
            None if self.resumed => 0.0,
            None => r.total_cost_usd,
        };
        self.last_total_cost = Some(r.total_cost_usd);
        self.usage.cost_usd = r.total_cost_usd;
        self.usage.tokens.add(&r.usage);
        if r.num_turns > 0 {
            self.usage.turns += 1;
        }

        self.transcript.push(Item::TurnSummary(TurnSummary {
            duration_ms: r.duration_ms,
            duration_api_ms: r.duration_api_ms,
            total_cost_usd: r.total_cost_usd,
            turn_cost_usd: delta,
            input_tokens: r.usage.input_tokens,
            output_tokens: r.usage.output_tokens,
            cache_read_tokens: r.usage.cache_read_input_tokens,
            cache_creation_tokens: r.usage.cache_creation_input_tokens,
            is_error: r.is_error,
            num_turns: r.num_turns,
            terminal_reason: r.terminal_reason.clone(),
            at: now,
        }));
        if r.is_error
            && r.terminal_reason.as_deref() != Some("aborted_streaming")
            && let Some(t) = r.result.as_deref().filter(|t| !t.is_empty())
        {
            self.note(NoteKind::Error, format!("Error: {t}"), now);
        }

        self.outstanding_turns = self.outstanding_turns.saturating_sub(1);
        if !self.is_finished() {
            self.status = if self.outstanding_turns == 0 {
                SessionStatus::Pending(PendingReason::AwaitingInput)
            } else {
                SessionStatus::Running
            };
        }
        delta
    }
}

fn is_agent_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task")
}

fn push_tool_use(items: &mut Vec<Item>, id: &str, name: &str, input: &Value) {
    if let Some(Item::ToolUse {
        name: n, input: i, ..
    }) = items
        .iter_mut()
        .find(|it| matches!(it, Item::ToolUse { id: x, .. } if x == id))
    {
        *n = name.to_owned();
        *i = input.clone();
        return;
    }
    items.push(Item::ToolUse {
        id: id.to_owned(),
        name: name.to_owned(),
        input: input.clone(),
        result: None,
    });
}

fn set_tool_result(items: &mut [Item], tool_use_id: &str, content: &str, is_error: bool) {
    for it in items.iter_mut().rev() {
        if let Item::ToolUse { id, result, .. } = it
            && id == tool_use_id
        {
            *result = Some((content.to_owned(), is_error));
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Sessions grouped for the three UI lists, each sorted by last update (newest first).
#[derive(Debug, Default)]
pub struct Grouped<'a> {
    pub running: Vec<&'a Session>,
    pub pending: Vec<&'a Session>,
    pub finished: Vec<&'a Session>,
}

/// The whole app model.
#[derive(Debug, Default)]
pub struct AppState {
    sessions: Vec<Session>,
    index: HashMap<SessionKey, usize>,
    day_usage: Option<DayUsage>,
    rate_limits: RateLimits,
    usage_text: Option<UsageText>,
}

impl AppState {
    pub fn new() -> AppState {
        AppState::default()
    }

    /// Registers a session (call before/when spawning it, with the same key).
    pub fn add_session(&mut self, n: NewSession) {
        let key = n.key;
        let s = Session::new(n, Local::now());
        self.index.insert(key, self.sessions.len());
        self.sessions.push(s);
    }

    /// Removes a session from the model.
    pub fn remove_session(&mut self, key: SessionKey) {
        if let Some(pos) = self.index.remove(&key) {
            self.sessions.remove(pos);
            self.index = self
                .sessions
                .iter()
                .enumerate()
                .map(|(i, s)| (s.key, i))
                .collect();
        }
    }

    /// All sessions, in creation order.
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn session(&self, key: SessionKey) -> Option<&Session> {
        self.index.get(&key).map(|&i| &self.sessions[i])
    }

    /// Mutable access (e.g. for UI-side edits such as renaming the title).
    pub fn session_mut(&mut self, key: SessionKey) -> Option<&mut Session> {
        let i = *self.index.get(&key)?;
        Some(&mut self.sessions[i])
    }

    /// Sessions grouped as Corriendo / Pendientes / Terminadas.
    pub fn grouped(&self) -> Grouped<'_> {
        let mut g = Grouped::default();
        for s in &self.sessions {
            match s.group() {
                SessionGroup::Running => g.running.push(s),
                SessionGroup::Pending => g.pending.push(s),
                SessionGroup::Finished => g.finished.push(s),
            }
        }
        for v in [&mut g.running, &mut g.pending, &mut g.finished] {
            v.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        }
        g
    }

    /// Today's usage across all sessions of the app (`None` before the first
    /// result, or if the stored day is not today).
    pub fn day_usage(&self) -> Option<&DayUsage> {
        let today = Local::now().date_naive();
        self.day_usage.as_ref().filter(|d| d.date == today)
    }

    /// Latest rate-limit windows.
    pub fn rate_limits(&self) -> &RateLimits {
        &self.rate_limits
    }

    /// Last `/usage` report text.
    pub fn usage_text(&self) -> Option<&UsageText> {
        self.usage_text.as_ref()
    }

    /// Stores the output of `run_usage_command`.
    pub fn set_usage_text(&mut self, text: String) {
        self.usage_text = Some(UsageText {
            text,
            at: Local::now(),
        });
    }

    /// Applies one session event to the model.
    pub fn apply(&mut self, ev: &SessionEvent) {
        let now = Local::now();

        // Global (session-independent) parts first.
        if let SessionEventKind::Incoming(inc) = &ev.kind
            && let Incoming::RateLimit(info) = inc.as_ref()
        {
            self.apply_rate_limit(info, now);
        }

        let Some(&idx) = self.index.get(&ev.session_key) else {
            tracing::debug!(?ev.session_key, "event for unknown session ignored");
            return;
        };

        let mut day_delta: Option<(f64, crate::protocol::Usage, bool)> = None;
        {
            let s = &mut self.sessions[idx];
            s.updated_at = now;
            match &ev.kind {
                SessionEventKind::Spawned { .. } => {}
                SessionEventKind::UserSent { text, attachments } => {
                    s.close_streaming();
                    if s.title.is_empty() {
                        s.title = if text.trim().is_empty() {
                            attachments
                                .first()
                                .map(|a| a.label.clone())
                                .unwrap_or_default()
                        } else {
                            truncate_title(text, TITLE_MAX)
                        };
                    }
                    s.transcript.push(Item::UserText {
                        text: text.clone(),
                        at: now,
                        attachments: attachments.clone(),
                    });
                    s.outstanding_turns += 1;
                    if !s.is_finished()
                        && !matches!(s.status, SessionStatus::Pending(PendingReason::Permission))
                    {
                        s.status = SessionStatus::Running;
                    }
                }
                SessionEventKind::PermissionResolved {
                    request_id,
                    allowed,
                } => s.resolve_permission(request_id, *allowed),
                SessionEventKind::InitializeRequested { request_id } => {
                    s.initialize_request = Some(request_id.clone());
                }
                SessionEventKind::InterruptRequested => {
                    s.note(NoteKind::Info, "Interrumpido por el usuario.".into(), now)
                }
                SessionEventKind::Incoming(inc) => {
                    if let Incoming::Result(r) = inc.as_ref() {
                        let delta = s.apply_result(r, now);
                        let real_turn = r.num_turns > 0;
                        day_delta = Some((delta, r.usage, real_turn));
                        if let Some(text) = r.result.as_deref() {
                            // `/usage` reports arrive as the text of a zero-cost result.
                            if r.num_turns == 0 && text.contains("% used") {
                                self.usage_text = Some(UsageText {
                                    text: text.to_owned(),
                                    at: now,
                                });
                            }
                        }
                    } else {
                        s.apply_incoming(inc, now);
                    }
                }
                SessionEventKind::ParseError { line, error } => {
                    tracing::warn!(%error, %line, "unparsable CLI line");
                }
                SessionEventKind::Stderr(line) => {
                    s.note(NoteKind::Error, line.clone(), now);
                }
                SessionEventKind::Exited { error } => {
                    s.close_streaming();
                    s.outstanding_turns = 0;
                    for sub in &mut s.subagents {
                        if sub.status == SubagentStatus::Running {
                            sub.status = SubagentStatus::Failed;
                            sub.ended_at.get_or_insert(now);
                        }
                    }
                    s.status = SessionStatus::Finished {
                        error: error.clone(),
                    };
                    if let Some(e) = error {
                        s.note(NoteKind::Error, e.clone(), now);
                    }
                }
            }
        }

        if let Some((cost, usage, real_turn)) = day_delta {
            self.add_day_usage(cost, &usage, real_turn, now);
        }
    }

    fn add_day_usage(
        &mut self,
        cost: f64,
        usage: &crate::protocol::Usage,
        real_turn: bool,
        now: DateTime<Local>,
    ) {
        let today = now.date_naive();
        let d = match &mut self.day_usage {
            Some(d) if d.date == today => d,
            slot => slot.insert(DayUsage {
                date: today,
                tokens: TokenTotals::default(),
                cost_usd: 0.0,
                turns: 0,
            }),
        };
        d.tokens.add(usage);
        d.cost_usd += cost;
        if real_turn {
            d.turns += 1;
        }
    }

    fn apply_rate_limit(&mut self, info: &RateLimitInfo, now: DateTime<Local>) {
        let conv = |w: &RateWindow| -> Option<RateWindowState> {
            Some(RateWindowState {
                utilization: w.utilization,
                resets_at: Local.timestamp_opt(w.resets_at, 0).single()?,
                status: w.status.clone(),
                received_at: now,
            })
        };
        if let Some(w) = info.five_hour.as_ref().and_then(conv) {
            self.rate_limits.five_hour = Some(w);
        }
        if let Some(w) = info.seven_day.as_ref().and_then(conv) {
            self.rate_limits.seven_day = Some(w);
        }
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;

    fn cmd(name: &str, aliases: &[&str], builtin: bool) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: String::new(),
            argument_hint: String::new(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            builtin,
        }
    }

    fn names<'a>(v: &[&'a SlashCommand]) -> Vec<&'a str> {
        v.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn prefix_before_contains() {
        let cmds = vec![cmd("xcompact", &[], false), cmd("compact", &[], false)];
        assert_eq!(
            names(&filter_commands(&cmds, "comp", 5)),
            ["compact", "xcompact"]
        );
    }

    #[test]
    fn alias_matches() {
        let cmds = vec![cmd("clear", &["reset"], false), cmd("help", &[], false)];
        assert_eq!(names(&filter_commands(&cmds, "res", 5)), ["clear"]);
    }

    #[test]
    fn builtin_first_within_group() {
        let cmds = vec![cmd("co-a", &[], false), cmd("co-b", &[], true)];
        assert_eq!(names(&filter_commands(&cmds, "co", 5)), ["co-b", "co-a"]);
    }

    #[test]
    fn limit_and_empty_query() {
        let cmds = vec![
            cmd("a", &[], false),
            cmd("b", &[], true),
            cmd("c", &[], false),
        ];
        assert_eq!(names(&filter_commands(&cmds, "", 2)), ["b", "a"]);
    }

    #[test]
    fn case_insensitive() {
        let cmds = vec![cmd("Compact", &[], false)];
        assert_eq!(filter_commands(&cmds, "COMP", 5).len(), 1);
    }
}

#[cfg(test)]
mod todo_tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, input: Value) -> Item {
        Item::ToolUse {
            id: "t1".into(),
            name: name.into(),
            input,
            result: None,
        }
    }

    fn session(transcript: Vec<Item>) -> Session {
        let mut n = NewSession::new(SessionKey::new(), r"C:\work");
        n.transcript = transcript;
        Session::new(n, Local::now())
    }

    #[test]
    fn todos_last_call_replaces_list() {
        let s = session(vec![
            tool(
                "TodoWrite",
                json!({"todos":[{"content":"a","status":"pending","activeForm":"Doing a"}]}),
            ),
            tool("Bash", json!({"command":"ls"})),
            tool(
                "TodoWrite",
                json!({"todos":[
                    {"content":"a","status":"completed","activeForm":"Doing a"},
                    {"content":"b","status":"in_progress","activeForm":"Doing b"},
                    {"content":"c","status":"pending"}
                ]}),
            ),
        ]);
        let t = s.todos();
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].status, TodoStatus::Completed);
        assert_eq!(t[1].status, TodoStatus::InProgress);
        assert_eq!(t[1].active_form.as_deref(), Some("Doing b"));
        assert_eq!(t[2].status, TodoStatus::Pending);
        assert_eq!(t[2].active_form, None);
    }

    fn done(name: &str, input: Value, text: &str) -> Item {
        Item::ToolUse {
            id: "x".into(),
            name: name.into(),
            input,
            result: Some((text.into(), false)),
        }
    }

    #[test]
    fn task_tools_build_list() {
        let s = session(vec![
            done(
                "TaskCreate",
                json!({"subject":"A","description":"d","activeForm":"Doing A"}),
                "Task #1 created successfully: A",
            ),
            done(
                "TaskCreate",
                json!({"subject":"B"}),
                "Task #2 created successfully: B",
            ),
            // No result text: assumed sequential (3).
            tool("TaskCreate", json!({"subject":"C"})),
            done(
                "TaskUpdate",
                json!({"taskId":"1","status":"completed"}),
                "Updated task #1 status",
            ),
            done(
                "TaskUpdate",
                json!({"taskId":"2","status":"in_progress"}),
                "Updated task #2 status",
            ),
            done("TaskUpdate", json!({"taskId":"3","status":"deleted"}), "ok"),
            done(
                "TaskUpdate",
                json!({"taskId":"99","status":"completed"}),
                "ok",
            ),
            done("TaskList", json!({}), "whatever"),
        ]);
        let t = s.todos();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].content, "A");
        assert_eq!(t[0].status, TodoStatus::Completed);
        assert_eq!(t[0].active_form.as_deref(), Some("Doing A"));
        assert_eq!(t[1].status, TodoStatus::InProgress);
    }

    #[test]
    fn task_id_parsing() {
        assert_eq!(task_id_from_text("Task #12 created"), Some("12".into()));
        assert_eq!(task_id_from_text("nothing"), None);
    }

    #[test]
    fn todos_tolerates_unknown_shapes() {
        let s = session(vec![
            tool(
                "TodoWrite",
                json!({"todos":[{"content":"ok","status":"pending"}]}),
            ),
            tool("TodoWrite", json!({"nothing": 1})),
            tool("TodoWrite", json!({"todos":[{"status":"pending"}, 5]})),
        ]);
        // The last call has a `todos` array (all entries invalid): empty list.
        assert!(s.todos().is_empty());
        let s = session(vec![
            tool(
                "TodoWrite",
                json!({"todos":[{"content":"ok","status":"weird"}]}),
            ),
            tool("TodoWrite", json!("garbage")),
        ]);
        // Garbage calls are skipped; the previous valid call wins.
        assert_eq!(s.todos().len(), 1);
        assert_eq!(s.todos()[0].status, TodoStatus::Pending);
        assert!(session(Vec::new()).todos().is_empty());
    }
}
