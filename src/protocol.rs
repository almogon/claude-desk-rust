//! Wire protocol of `claude -p --input-format stream-json --output-format stream-json`.
//!
//! Parsing is deliberately tolerant: every message is first read as a
//! [`serde_json::Value`] and then mapped by hand. Unknown event types become
//! [`Incoming::Unknown`], unknown system subtypes become [`Incoming::OtherSystem`],
//! unknown content blocks become [`ContentBlock::Unknown`]; missing fields fall
//! back to defaults. Nothing here panics or fails on unexpected shapes.
//!
//! See `docs/protocol.md` for the captured protocol facts.

use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn get_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn get_u64(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn get_f64(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn get_bool(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Permission modes and decisions (client -> CLI)
// ---------------------------------------------------------------------------

/// Permission modes accepted by `--permission-mode` / `set_permission_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Auto,
    BypassPermissions,
    DontAsk,
    Plan,
}

impl PermissionMode {
    /// Every mode, in a stable order (handy for UI pickers).
    pub const ALL: [PermissionMode; 6] = [
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::Auto,
        PermissionMode::BypassPermissions,
        PermissionMode::DontAsk,
        PermissionMode::Plan,
    ];

    /// The string the CLI expects.
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Auto => "auto",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }

    /// Parses the CLI spelling; `None` for unknown modes.
    pub fn parse(s: &str) -> Option<PermissionMode> {
        PermissionMode::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// The answer to a `can_use_tool` request.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    /// Allow the tool call. `updated_input` is normally the original input
    /// (possibly edited by the user).
    Allow {
        updated_input: Value,
        /// Permission updates to apply (usually one `permission_suggestions` entry echoed
        /// back, e.g. an `addRules` object). Empty = plain allow.
        updated_permissions: Vec<Value>,
    },
    /// Deny the tool call; `message` is shown to the model.
    Deny { message: String },
}

// ---------------------------------------------------------------------------
// Outgoing messages (stdin lines, without trailing newline)
// ---------------------------------------------------------------------------

/// Builds the JSON line for a user message (also used for slash commands).
pub fn user_message_line(text: &str) -> String {
    json!({"type":"user","message":{"role":"user","content":text}}).to_string()
}

/// Something attached to a user message besides its text.
#[derive(Debug, Clone, PartialEq)]
pub enum Attachment {
    /// A PNG image. `id` is unique per attachment (used for the thumbnail URI).
    Image {
        id: u64,
        png: std::sync::Arc<[u8]>,
        w: u32,
        h: u32,
    },
    /// Long pasted text, sent wrapped in `<pasted_text>`.
    PastedText { text: String, lines: usize },
}

impl Attachment {
    /// Chip label shown in the UI.
    pub fn label(&self) -> String {
        match self {
            Attachment::Image { w, h, .. } => format!("Imagen {w}×{h}"),
            Attachment::PastedText { lines, .. } => format!("Texto pegado · {lines} líneas"),
        }
    }

    /// Summary kept in the transcript.
    pub fn summary(&self) -> AttachmentSummary {
        AttachmentSummary {
            label: self.label(),
        }
    }
}

/// Transcript-side description of an attachment (no payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSummary {
    pub label: String,
}

/// Builds the JSON line for a user message with attachments: `content` is an
/// array with the images first, then pasted texts, then the user's text.
pub fn user_message_blocks_line(text: &str, attachments: &[Attachment]) -> String {
    use base64::Engine as _;
    let mut blocks: Vec<Value> = Vec::new();
    for a in attachments {
        if let Attachment::Image { png, .. } = a {
            blocks.push(json!({
                "type":"image",
                "source":{
                    "type":"base64",
                    "media_type":"image/png",
                    "data": base64::engine::general_purpose::STANDARD.encode(png)
                }
            }));
        }
    }
    for a in attachments {
        if let Attachment::PastedText { text, .. } = a {
            blocks.push(json!({
                "type":"text",
                "text": format!("<pasted_text>\n{text}\n</pasted_text>")
            }));
        }
    }
    if !text.is_empty() {
        blocks.push(json!({"type":"text","text":text}));
    }
    json!({"type":"user","message":{"role":"user","content":blocks}}).to_string()
}

/// Builds a client-initiated `control_request` line.
pub fn control_request_line(request_id: &str, request: Value) -> String {
    json!({"type":"control_request","request_id":request_id,"request":request}).to_string()
}

/// Builds the `control_response` line answering a CLI `can_use_tool` request.
pub fn permission_response_line(request_id: &str, decision: &PermissionDecision) -> String {
    let response = match decision {
        PermissionDecision::Allow {
            updated_input,
            updated_permissions,
        } => {
            let mut r = json!({"behavior":"allow","updatedInput":updated_input});
            if !updated_permissions.is_empty() {
                r["updatedPermissions"] = Value::Array(updated_permissions.clone());
            }
            r
        }
        PermissionDecision::Deny { message } => json!({"behavior":"deny","message":message}),
    };
    json!({
        "type":"control_response",
        "response":{"subtype":"success","request_id":request_id,"response":response}
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Content blocks and usage
// ---------------------------------------------------------------------------

/// One block of an Anthropic-style message content array.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        /// Flattened text of the result (arrays of text blocks are joined by `\n`).
        content: String,
        is_error: bool,
    },
    /// Any block type we do not know; raw JSON preserved.
    Unknown(Value),
}

impl ContentBlock {
    /// Maps one JSON block.
    pub fn from_value(v: &Value) -> ContentBlock {
        match v.get("type").and_then(Value::as_str) {
            Some("text") => ContentBlock::Text(get_str(v, "text").unwrap_or_default()),
            Some("thinking") => ContentBlock::Thinking(get_str(v, "thinking").unwrap_or_default()),
            Some("tool_use") => ContentBlock::ToolUse {
                id: get_str(v, "id").unwrap_or_default(),
                name: get_str(v, "name").unwrap_or_default(),
                input: v.get("input").cloned().unwrap_or(Value::Null),
            },
            Some("tool_result") => ContentBlock::ToolResult {
                tool_use_id: get_str(v, "tool_use_id").unwrap_or_default(),
                content: flatten_text(v.get("content")),
                is_error: get_bool(v, "is_error"),
            },
            _ => ContentBlock::Unknown(v.clone()),
        }
    }
}

/// Flattens `content` that may be a string or an array of `{type:"text"}` blocks.
fn flatten_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|b| {
                b.get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| b.as_str().map(str::to_owned))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Parses a message `content` field: a plain string becomes a single `Text`
/// block; an array is mapped block by block.
pub fn parse_content(v: Option<&Value>) -> Vec<ContentBlock> {
    match v {
        Some(Value::String(s)) => vec![ContentBlock::Text(s.clone())],
        Some(Value::Array(a)) => a.iter().map(ContentBlock::from_value).collect(),
        _ => Vec::new(),
    }
}

/// Token usage of one turn / message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    fn from_value(v: Option<&Value>) -> Usage {
        let Some(v) = v else {
            return Usage::default();
        };
        Usage {
            input_tokens: get_u64(v, "input_tokens"),
            output_tokens: get_u64(v, "output_tokens"),
            cache_read_input_tokens: get_u64(v, "cache_read_input_tokens"),
            cache_creation_input_tokens: get_u64(v, "cache_creation_input_tokens"),
        }
    }
}

// ---------------------------------------------------------------------------
// Incoming messages (stdout lines)
// ---------------------------------------------------------------------------

/// One parsed stdout line.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// `system/init` (emitted at the start of every turn).
    Init(SystemInit),
    /// `system/status` (also carries `permissionMode` after a mode change).
    Status(SystemStatus),
    /// `system/permission_denied`.
    PermissionDenied {
        tool_name: String,
        tool_use_id: Option<String>,
        message: String,
    },
    /// Subagent lifecycle (`system/task_*`).
    Task(TaskEvent),
    /// Any other `system/*` subtype (e.g. `thinking_tokens`).
    OtherSystem { subtype: String, raw: Value },
    /// Partial-message stream event.
    StreamEvent(StreamEvent),
    /// A complete assistant message (one content block per event in practice).
    Assistant(AssistantMessage),
    /// A user message emitted by the CLI (tool results, subagent prompts, ...).
    User(UserMessage),
    /// `rate_limit_event`.
    RateLimit(RateLimitInfo),
    /// End of a turn.
    Result(ResultMessage),
    /// CLI-initiated control request (permissions).
    ControlRequest(ControlRequest),
    /// Response to a client control request.
    ControlResponse(ControlResponse),
    /// Unknown top-level `type`; raw JSON preserved.
    Unknown(Value),
}

impl Incoming {
    /// Parses one stdout line. Fails only if the line is not valid JSON.
    pub fn parse_line(line: &str) -> Result<Incoming, serde_json::Error> {
        let v: Value = serde_json::from_str(line)?;
        Ok(Incoming::from_value(v))
    }

    /// Maps an already parsed JSON value. Never fails.
    pub fn from_value(v: Value) -> Incoming {
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        match ty {
            "system" => Self::parse_system(v),
            "stream_event" => Incoming::StreamEvent(StreamEvent::from_value(&v)),
            "assistant" => Incoming::Assistant(AssistantMessage::from_value(&v)),
            "user" => Incoming::User(UserMessage::from_value(&v)),
            "rate_limit_event" => Incoming::RateLimit(RateLimitInfo::from_value(&v)),
            "result" => Incoming::Result(ResultMessage::from_value(&v)),
            "control_request" => Incoming::ControlRequest(ControlRequest::from_value(&v)),
            "control_response" => Incoming::ControlResponse(ControlResponse::from_value(&v)),
            _ => Incoming::Unknown(v),
        }
    }

    fn parse_system(v: Value) -> Incoming {
        let subtype = get_str(&v, "subtype").unwrap_or_default();
        match subtype.as_str() {
            "init" => Incoming::Init(SystemInit {
                session_id: get_str(&v, "session_id"),
                model: get_str(&v, "model"),
                permission_mode: get_str(&v, "permissionMode"),
                cwd: get_str(&v, "cwd"),
                claude_code_version: get_str(&v, "claude_code_version"),
                tools: v
                    .get("tools")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                slash_commands: v
                    .get("slash_commands")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
            "status" => Incoming::Status(SystemStatus {
                status: get_str(&v, "status"),
                permission_mode: get_str(&v, "permissionMode"),
            }),
            "permission_denied" => Incoming::PermissionDenied {
                tool_name: get_str(&v, "tool_name").unwrap_or_default(),
                tool_use_id: get_str(&v, "tool_use_id"),
                message: get_str(&v, "message").unwrap_or_default(),
            },
            "task_started" | "task_progress" | "task_updated" | "task_notification" => {
                Incoming::Task(TaskEvent::from_value(&subtype, &v))
            }
            _ => Incoming::OtherSystem { subtype, raw: v },
        }
    }
}

/// `system/init`.
#[derive(Debug, Clone, Default)]
pub struct SystemInit {
    pub session_id: Option<String>,
    /// Model reported at init. NOT reliable: prefer `assistant.message.model`.
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub cwd: Option<String>,
    pub claude_code_version: Option<String>,
    pub tools: Vec<String>,
    /// Names of available slash commands (without the leading `/`).
    pub slash_commands: Vec<String>,
}

/// A slash command offered by the CLI (from the `initialize` control response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub aliases: Vec<String>,
    pub builtin: bool,
}

/// Parses the `commands` array of an `initialize` response. Tolerant: entries
/// without a name are skipped and a non-array yields an empty list.
pub fn parse_commands(v: &Value) -> Vec<SlashCommand> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|c| {
            let name = get_str(c, "name").filter(|n| !n.is_empty())?;
            Some(SlashCommand {
                name,
                description: get_str(c, "description").unwrap_or_default(),
                argument_hint: get_str(c, "argumentHint").unwrap_or_default(),
                aliases: c
                    .get("aliases")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                builtin: c.get("builtin").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// `system/status`.
#[derive(Debug, Clone, Default)]
pub struct SystemStatus {
    /// e.g. `"requesting"`, or `None` after a mode change.
    pub status: Option<String>,
    pub permission_mode: Option<String>,
}

/// Token/tool stats attached to subagent task events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskUsage {
    pub total_tokens: u64,
    pub tool_uses: u64,
    pub duration_ms: u64,
}

/// Subagent lifecycle events. `task_id` equals the subagent's `agent_id`.
#[derive(Debug, Clone)]
pub enum TaskEvent {
    Started {
        task_id: String,
        tool_use_id: Option<String>,
        description: String,
        subagent_type: Option<String>,
        prompt: Option<String>,
    },
    Progress {
        task_id: String,
        tool_use_id: Option<String>,
        description: Option<String>,
        last_tool_name: Option<String>,
        usage: TaskUsage,
    },
    /// `task_updated` carries only `task_id` and a `patch` (no `tool_use_id`).
    Updated {
        task_id: String,
        status: Option<String>,
        /// Epoch milliseconds.
        end_time_ms: Option<i64>,
    },
    Notification {
        task_id: String,
        tool_use_id: Option<String>,
        status: Option<String>,
        summary: Option<String>,
        usage: TaskUsage,
    },
}

impl TaskEvent {
    fn from_value(subtype: &str, v: &Value) -> TaskEvent {
        let task_id = get_str(v, "task_id").unwrap_or_default();
        let tool_use_id = get_str(v, "tool_use_id");
        let usage = v
            .get("usage")
            .map(|u| TaskUsage {
                total_tokens: get_u64(u, "total_tokens"),
                tool_uses: get_u64(u, "tool_uses"),
                duration_ms: get_u64(u, "duration_ms"),
            })
            .unwrap_or_default();
        match subtype {
            "task_started" => TaskEvent::Started {
                task_id,
                tool_use_id,
                description: get_str(v, "description").unwrap_or_default(),
                subagent_type: get_str(v, "subagent_type"),
                prompt: get_str(v, "prompt"),
            },
            "task_progress" => TaskEvent::Progress {
                task_id,
                tool_use_id,
                description: get_str(v, "description"),
                last_tool_name: get_str(v, "last_tool_name"),
                usage,
            },
            "task_updated" => {
                let patch = v.get("patch").cloned().unwrap_or(Value::Null);
                TaskEvent::Updated {
                    task_id,
                    status: get_str(&patch, "status"),
                    end_time_ms: patch.get("end_time").and_then(Value::as_i64),
                }
            }
            _ => TaskEvent::Notification {
                task_id,
                tool_use_id,
                status: get_str(v, "status"),
                summary: get_str(v, "summary"),
                usage,
            },
        }
    }
}

/// A `stream_event` wrapper.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub parent_tool_use_id: Option<String>,
    pub kind: StreamKind,
}

/// The inner raw SSE event.
#[derive(Debug, Clone)]
pub enum StreamKind {
    MessageStart { model: Option<String> },
    BlockStart { index: u32, block: ContentBlock },
    TextDelta { index: u32, text: String },
    InputJsonDelta { index: u32, partial_json: String },
    ThinkingDelta { index: u32 },
    BlockStop { index: u32 },
    MessageDelta { stop_reason: Option<String> },
    MessageStop,
    Other(Value),
}

impl StreamEvent {
    fn from_value(v: &Value) -> StreamEvent {
        let parent_tool_use_id = get_str(v, "parent_tool_use_id");
        let ev = v.get("event").cloned().unwrap_or(Value::Null);
        let index = ev.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        let kind = match ev.get("type").and_then(Value::as_str) {
            Some("message_start") => StreamKind::MessageStart {
                model: ev.get("message").and_then(|m| get_str(m, "model")),
            },
            Some("content_block_start") => StreamKind::BlockStart {
                index,
                block: ev
                    .get("content_block")
                    .map(ContentBlock::from_value)
                    .unwrap_or(ContentBlock::Unknown(Value::Null)),
            },
            Some("content_block_delta") => {
                let d = ev.get("delta").cloned().unwrap_or(Value::Null);
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => StreamKind::TextDelta {
                        index,
                        text: get_str(&d, "text").unwrap_or_default(),
                    },
                    Some("input_json_delta") => StreamKind::InputJsonDelta {
                        index,
                        partial_json: get_str(&d, "partial_json").unwrap_or_default(),
                    },
                    Some("thinking_delta") => StreamKind::ThinkingDelta { index },
                    _ => StreamKind::Other(ev),
                }
            }
            Some("content_block_stop") => StreamKind::BlockStop { index },
            Some("message_delta") => StreamKind::MessageDelta {
                stop_reason: ev.get("delta").and_then(|d| get_str(d, "stop_reason")),
            },
            Some("message_stop") => StreamKind::MessageStop,
            _ => StreamKind::Other(ev),
        };
        StreamEvent {
            parent_tool_use_id,
            kind,
        }
    }
}

/// A complete `assistant` event.
#[derive(Debug, Clone)]
pub struct AssistantMessage {
    /// Real model that produced the message (`"<synthetic>"` for local slash commands).
    pub model: Option<String>,
    pub message_id: Option<String>,
    pub content: Vec<ContentBlock>,
    pub usage: Usage,
    pub session_id: Option<String>,
    /// Set for messages of a subagent: the `Agent` tool_use id.
    pub parent_tool_use_id: Option<String>,
    pub agent_id: Option<String>,
}

impl AssistantMessage {
    fn from_value(v: &Value) -> AssistantMessage {
        let msg = v.get("message");
        AssistantMessage {
            model: msg.and_then(|m| get_str(m, "model")),
            message_id: msg.and_then(|m| get_str(m, "id")),
            content: parse_content(msg.and_then(|m| m.get("content"))),
            usage: Usage::from_value(msg.and_then(|m| m.get("usage"))),
            session_id: get_str(v, "session_id"),
            parent_tool_use_id: get_str(v, "parent_tool_use_id"),
            agent_id: get_str(v, "agent_id"),
        }
    }
}

/// A `user` event emitted by the CLI.
#[derive(Debug, Clone)]
pub struct UserMessage {
    pub content: Vec<ContentBlock>,
    pub session_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub agent_id: Option<String>,
}

impl UserMessage {
    fn from_value(v: &Value) -> UserMessage {
        UserMessage {
            content: parse_content(v.get("message").and_then(|m| m.get("content"))),
            session_id: get_str(v, "session_id"),
            parent_tool_use_id: get_str(v, "parent_tool_use_id"),
            agent_id: get_str(v, "agent_id"),
        }
    }
}

/// One rate-limit window (`five_hour` or `seven_day`).
#[derive(Debug, Clone, PartialEq)]
pub struct RateWindow {
    /// Fraction in `0.0..=1.0`.
    pub utilization: f64,
    /// Unix epoch seconds.
    pub resets_at: i64,
    pub status: Option<String>,
}

/// `rate_limit_event.rate_limit_info`. A window is `None` when the event did
/// not include it (later events may only carry changed windows).
#[derive(Debug, Clone, Default)]
pub struct RateLimitInfo {
    pub status: Option<String>,
    pub five_hour: Option<RateWindow>,
    pub seven_day: Option<RateWindow>,
}

impl RateLimitInfo {
    fn from_value(v: &Value) -> RateLimitInfo {
        let info = v.get("rate_limit_info").cloned().unwrap_or(Value::Null);
        let windows = info.get("unifiedWindows").cloned().unwrap_or(Value::Null);
        let win = |name: &str| {
            windows.get(name).map(|w| RateWindow {
                utilization: get_f64(w, "utilization"),
                resets_at: w.get("resetsAt").and_then(Value::as_i64).unwrap_or(0),
                status: get_str(w, "status"),
            })
        };
        RateLimitInfo {
            status: get_str(&info, "status"),
            five_hour: win("five_hour"),
            seven_day: win("seven_day"),
        }
    }
}

/// `result`: end of a turn.
#[derive(Debug, Clone, Default)]
pub struct ResultMessage {
    /// `"success"`, `"error_during_execution"`, ...
    pub subtype: String,
    pub is_error: bool,
    /// Final text (for `/usage` this is the usage report).
    pub result: Option<String>,
    pub session_id: Option<String>,
    pub terminal_reason: Option<String>,
    pub num_turns: u64,
    pub duration_ms: u64,
    pub duration_api_ms: u64,
    /// Cumulative cost of the session (not per turn).
    pub total_cost_usd: f64,
    /// Tokens of THIS turn.
    pub usage: Usage,
}

impl ResultMessage {
    fn from_value(v: &Value) -> ResultMessage {
        ResultMessage {
            subtype: get_str(v, "subtype").unwrap_or_default(),
            is_error: get_bool(v, "is_error"),
            result: get_str(v, "result"),
            session_id: get_str(v, "session_id"),
            terminal_reason: get_str(v, "terminal_reason"),
            num_turns: get_u64(v, "num_turns"),
            duration_ms: get_u64(v, "duration_ms"),
            duration_api_ms: get_u64(v, "duration_api_ms"),
            total_cost_usd: get_f64(v, "total_cost_usd"),
            usage: Usage::from_value(v.get("usage")),
        }
    }
}

/// CLI-initiated control request.
#[derive(Debug, Clone)]
pub struct ControlRequest {
    pub request_id: String,
    pub body: ControlRequestBody,
}

/// Body of a [`ControlRequest`].
#[derive(Debug, Clone)]
pub enum ControlRequestBody {
    /// The CLI asks permission to run a tool. Answer with
    /// `SessionHandle::respond_permission`.
    CanUseTool(CanUseTool),
    /// Any other subtype; raw request preserved.
    Other { subtype: String, raw: Value },
}

/// Payload of a `can_use_tool` request.
#[derive(Debug, Clone)]
pub struct CanUseTool {
    pub tool_name: String,
    pub display_name: Option<String>,
    pub input: Value,
    pub description: Option<String>,
    pub tool_use_id: Option<String>,
    /// Raw `permission_suggestions` (e.g. `setMode` objects).
    pub permission_suggestions: Vec<Value>,
}

impl ControlRequest {
    fn from_value(v: &Value) -> ControlRequest {
        let request_id = get_str(v, "request_id").unwrap_or_default();
        let req = v.get("request").cloned().unwrap_or(Value::Null);
        let subtype = get_str(&req, "subtype").unwrap_or_default();
        let body = if subtype == "can_use_tool" {
            ControlRequestBody::CanUseTool(CanUseTool {
                tool_name: get_str(&req, "tool_name").unwrap_or_default(),
                display_name: get_str(&req, "display_name"),
                input: req.get("input").cloned().unwrap_or(Value::Null),
                description: get_str(&req, "description"),
                tool_use_id: get_str(&req, "tool_use_id"),
                permission_suggestions: req
                    .get("permission_suggestions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            })
        } else {
            ControlRequestBody::Other { subtype, raw: req }
        };
        ControlRequest { request_id, body }
    }
}

/// Response to a client-initiated control request.
#[derive(Debug, Clone)]
pub struct ControlResponse {
    pub request_id: String,
    pub ok: bool,
    /// Payload on success (e.g. `{"model": ...}`, `{"mode": ...}`).
    pub response: Value,
    pub error: Option<String>,
    pub error_code: Option<String>,
}

impl ControlResponse {
    fn from_value(v: &Value) -> ControlResponse {
        let r = v.get("response").cloned().unwrap_or(Value::Null);
        ControlResponse {
            request_id: get_str(&r, "request_id").unwrap_or_default(),
            ok: r.get("subtype").and_then(Value::as_str) == Some("success"),
            response: r.get("response").cloned().unwrap_or(Value::Null),
            error: get_str(&r, "error"),
            error_code: get_str(&r, "error_code"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_types_are_tolerated() {
        assert!(matches!(
            Incoming::parse_line(r#"{"type":"brand_new","x":1}"#).unwrap(),
            Incoming::Unknown(_)
        ));
        assert!(matches!(
            Incoming::parse_line(r#"{"type":"system","subtype":"wat"}"#).unwrap(),
            Incoming::OtherSystem { .. }
        ));
        assert!(matches!(
            ContentBlock::from_value(&json!({"type":"image"})),
            ContentBlock::Unknown(_)
        ));
        assert!(Incoming::parse_line("not json").is_err());
    }

    const CONTROLS: &str = include_str!("../tests/fixtures/controls.jsonl");

    #[test]
    fn parse_commands_reads_initialize_response() {
        let line = CONTROLS.lines().next().unwrap();
        let v: Value = serde_json::from_str(line).unwrap();
        let cmds = parse_commands(&v["response"]["response"]["commands"]);
        assert!(!cmds.is_empty());
        let compact = cmds.iter().find(|c| c.name == "compact").unwrap();
        assert_eq!(
            compact.argument_hint,
            "<optional custom summarization instructions>"
        );
        assert!(compact.builtin);
    }

    #[test]
    fn parse_commands_is_tolerant() {
        assert!(parse_commands(&json!({"a":1})).is_empty());
        let cmds = parse_commands(&json!([{"description":"x"},{"name":"a","aliases":["b"]}]));
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "a");
        assert_eq!(cmds[0].aliases, vec!["b".to_string()]);
        assert!(!cmds[0].builtin);
        assert!(cmds[0].description.is_empty());
    }

    #[test]
    fn init_carries_slash_commands() {
        let line = CONTROLS
            .lines()
            .find(|l| l.contains(r#""subtype":"init""#))
            .unwrap();
        match Incoming::parse_line(line).unwrap() {
            Incoming::Init(i) => assert!(i.slash_commands.iter().any(|c| c == "design")),
            other => panic!("expected init, got {other:?}"),
        }
    }

    #[test]
    fn outgoing_lines_have_expected_shape() {
        let l = permission_response_line(
            "r1",
            &PermissionDecision::Deny {
                message: "no".into(),
            },
        );
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(v["response"]["request_id"], "r1");
        let u: Value = serde_json::from_str(&user_message_line("hola")).unwrap();
        assert_eq!(u["message"]["content"], "hola");
    }

    #[test]
    fn user_message_with_attachments_serializes_blocks() {
        let atts = vec![
            Attachment::Image {
                id: 1,
                png: std::sync::Arc::from(vec![1u8, 2, 3]),
                w: 10,
                h: 20,
            },
            Attachment::PastedText {
                text: "a\nb".into(),
                lines: 2,
            },
        ];
        let v: Value = serde_json::from_str(&user_message_blocks_line("hola", &atts)).unwrap();
        assert_eq!(v["type"], "user");
        let c = v["message"]["content"].as_array().unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0]["type"], "image");
        assert_eq!(c[0]["source"]["type"], "base64");
        assert_eq!(c[0]["source"]["media_type"], "image/png");
        assert_eq!(c[0]["source"]["data"], "AQID");
        assert_eq!(c[1]["text"], "<pasted_text>\na\nb\n</pasted_text>");
        assert_eq!(c[2]["text"], "hola");
        assert_eq!(atts[0].label(), "Imagen 10×20");
    }

    #[test]
    fn allow_with_updated_permissions_line() {
        let rule = json!({"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"mkdir d1 *"}],"behavior":"allow","destination":"localSettings"});
        let l = permission_response_line(
            "r1",
            &PermissionDecision::Allow {
                updated_input: json!({"command":"mkdir d1"}),
                updated_permissions: vec![rule.clone()],
            },
        );
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["response"]["updatedPermissions"][0], rule);
        let plain = permission_response_line(
            "r1",
            &PermissionDecision::Allow {
                updated_input: json!({}),
                updated_permissions: vec![],
            },
        );
        assert!(!plain.contains("updatedPermissions"));
    }

    #[test]
    fn permission_mode_roundtrip() {
        for m in PermissionMode::ALL {
            assert_eq!(PermissionMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(PermissionMode::parse("bogus"), None);
    }
}
