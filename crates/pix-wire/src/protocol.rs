use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    ATTACHMENT_MIME_TYPES, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_REQUEST,
    MAX_CLIENT_CAPABILITIES, MAX_ENCRYPTED_FRAME_BYTES, MAX_HISTORY_PAGE_BYTES,
    MAX_HISTORY_PAGE_MESSAGES, MAX_HISTORY_PREVIEW_BYTES, MAX_IMAGE_CHUNK_BYTES,
    MAX_TEXT_FIELD_BYTES, PROTOCOL_MAJOR, WireError,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientEnvelope {
    pub protocol: u16,
    pub request_id: u64,
    #[serde(flatten)]
    pub request: ClientRequest,
}

impl ClientEnvelope {
    /// Encodes a validated plaintext envelope for the secure channel.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] for incompatible versions, invalid resources, or
    /// JSON encoding failures.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        validate_protocol(self.protocol)?;
        validate_client_request(&self.request)?;
        encode_value(self)
    }

    /// Decodes a plaintext envelope after secure-channel authentication.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] for oversized, malformed, incompatible, or
    /// resource-invalid input.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        validate_plaintext_size(bytes)?;
        let envelope: Self = serde_json::from_slice(bytes).map_err(WireError::Decode)?;
        validate_protocol(envelope.protocol)?;
        validate_client_request(&envelope.request)?;
        validate_all_strings(&serde_json::to_value(&envelope).map_err(WireError::Encode)?)?;
        Ok(envelope)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerEnvelope {
    pub protocol: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
    #[serde(flatten)]
    pub event: ServerEvent,
}

impl ServerEnvelope {
    /// Encodes a validated server event for the secure channel.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] for incompatible versions, invalid resources, or
    /// JSON encoding failures.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        validate_protocol(self.protocol)?;
        validate_server_event(&self.event)?;
        encode_value(self)
    }

    /// Decodes a server event after secure-channel authentication.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] for oversized, malformed, incompatible, or
    /// resource-invalid input.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        validate_plaintext_size(bytes)?;
        let envelope: Self = serde_json::from_slice(bytes).map_err(WireError::Decode)?;
        validate_protocol(envelope.protocol)?;
        validate_server_event(&envelope.event)?;
        validate_all_strings(&serde_json::to_value(&envelope).map_err(WireError::Encode)?)?;
        Ok(envelope)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientRequest {
    /// Requests the host snapshot. `capabilities` declares the optional
    /// protocol extensions this client understands; the host enables each
    /// extension only when the declaration arrives on the connection.
    #[serde(rename = "host.snapshot")]
    HostSnapshot {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
    /// Returns Pi's persisted default model/thinking level and the model
    /// catalog without starting a session runtime.
    #[serde(rename = "host.defaults")]
    HostDefaults,
    #[serde(rename = "workspace.list")]
    WorkspaceList,
    #[serde(rename = "session.list")]
    SessionList {
        workspace_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    #[serde(rename = "session.create")]
    SessionCreate {
        workspace_id: Uuid,
        /// Optional display name. Omit it to let Pi title the session from the
        /// first user message, matching the native TUI.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    #[serde(rename = "session.attach")]
    SessionAttach { session_id: String },
    /// Requests the next bounded page of older history. The cursor is opaque
    /// to clients; `before` is the cursor returned by `session.snapshot` or a
    /// previous `session.history.page` event.
    #[serde(rename = "session.history.request")]
    SessionHistoryRequest {
        session_id: String,
        before: String,
        limit: u32,
    },
    #[serde(rename = "session.rename")]
    SessionRename { session_id: String, name: String },
    #[serde(rename = "session.release")]
    SessionRelease { session_id: String },
    #[serde(rename = "session.prompt")]
    SessionPrompt {
        session_id: String,
        content: String,
        /// Ready attachment IDs uploaded through `attachment.begin` on this
        /// connection. Consumed by the host when the prompt is accepted.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
    },
    #[serde(rename = "session.steer")]
    SessionSteer {
        session_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
    },
    #[serde(rename = "session.follow_up")]
    SessionFollowUp {
        session_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
    },
    #[serde(rename = "session.abort")]
    SessionAbort { session_id: String },
    #[serde(rename = "session.compact")]
    SessionCompact {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    #[serde(rename = "model.list")]
    ModelList { session_id: String },
    #[serde(rename = "model.set")]
    ModelSet {
        session_id: String,
        provider: String,
        model_id: String,
    },
    #[serde(rename = "thinking.set")]
    ThinkingSet {
        session_id: String,
        level: ThinkingLevel,
    },
    #[serde(rename = "extension_ui.respond")]
    ExtensionUiRespond {
        session_id: String,
        extension_request_id: String,
        answer: ExtensionUiAnswer,
    },
    /// Starts an attachment upload scoped to one attached session. The host
    /// stages chunks in bounded memory, then atomically persists a source,
    /// agent, and vision asset at `attachment.finish`.
    #[serde(rename = "attachment.begin")]
    AttachmentBegin {
        session_id: String,
        attachment_id: String,
        mime_type: String,
        size: u64,
    },
    /// Appends one base64 chunk to a pending attachment. Chunk payloads stay
    /// inside the regular encrypted frame and decoded-string limits.
    #[serde(rename = "attachment.chunk")]
    AttachmentChunk { attachment_id: String, data: String },
    /// Marks an attachment ready after all declared bytes have arrived.
    #[serde(rename = "attachment.finish")]
    AttachmentFinish { attachment_id: String },
    /// Reads one bounded range of a host-owned historical image asset. The
    /// request is only accepted by clients that declared `image_refs.v1`.
    #[serde(rename = "image.get")]
    ImageGet {
        session_id: String,
        image_ref: String,
        offset: u64,
        limit: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ServerEvent {
    #[serde(rename = "request.ack")]
    RequestAck,
    #[serde(rename = "host.snapshot")]
    HostSnapshot { snapshot: HostSnapshot },
    #[serde(rename = "host.defaults")]
    HostDefaults { defaults: HostModelDefaults },
    #[serde(rename = "workspace.list")]
    WorkspaceList { workspaces: Vec<WorkspaceSummary> },
    #[serde(rename = "workspace.changed")]
    WorkspaceChanged { workspace: WorkspaceSummary },
    #[serde(rename = "session.list")]
    SessionList {
        workspace_id: Uuid,
        sessions: Vec<SessionSummary>,
    },
    #[serde(rename = "session.snapshot")]
    SessionSnapshot { snapshot: SessionSnapshot },
    /// One bounded page of messages preceding the current client timeline.
    #[serde(rename = "session.history.page")]
    SessionHistoryPage {
        #[serde(flatten)]
        page: SessionHistoryPage,
    },
    #[serde(rename = "session.state")]
    SessionState {
        session_id: String,
        state: SessionState,
    },
    #[serde(rename = "user.message")]
    UserMessage { session_id: String, message: Value },
    #[serde(rename = "assistant.delta")]
    AssistantDelta { session_id: String, delta: Value },
    #[serde(rename = "assistant.message")]
    AssistantMessage { session_id: String, message: Value },
    #[serde(rename = "tool.start")]
    ToolStart { session_id: String, tool: ToolEvent },
    #[serde(rename = "tool.update")]
    ToolUpdate { session_id: String, tool: ToolEvent },
    #[serde(rename = "tool.end")]
    ToolEnd { session_id: String, tool: ToolEvent },
    #[serde(rename = "extension_ui.request")]
    ExtensionUiRequest {
        session_id: String,
        request: ExtensionUiRequest,
    },
    #[serde(rename = "compaction")]
    Compaction {
        session_id: String,
        compaction: CompactionEvent,
    },
    #[serde(rename = "model.list")]
    ModelList {
        session_id: String,
        models: Vec<ModelSummary>,
    },
    /// Live steering and follow-up queue contents for a running session.
    /// Sent only to connections that declared the `queue.v1` capability.
    #[serde(rename = "session.queue")]
    SessionQueue {
        session_id: String,
        queue: SessionQueue,
    },
    /// One lazy image range for a `session.snapshot` image reference.
    #[serde(rename = "image.chunk")]
    ImageChunk {
        image_ref: String,
        mime_type: String,
        offset: u64,
        total_size: u64,
        eof: bool,
        data: String,
    },
    /// Optional session enrichment delivered after the base snapshot. Each
    /// field is omitted when its corresponding capability was not declared.
    #[serde(rename = "session.metadata")]
    SessionMetadata {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<Vec<CommandSummary>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<SessionUsage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_levels: Option<Vec<ThinkingLevel>>,
    },
    #[serde(rename = "error")]
    Error {
        code: ErrorCode,
        message: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSummary {
    pub id: Uuid,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostSnapshot {
    pub host: HostSummary,
    pub workspaces: Vec<WorkspaceSummary>,
    /// Relay reachability for the requesting device. Present only when the
    /// host has relay transport configured, and delivered exclusively inside
    /// the authenticated encrypted channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<RelayAccess>,
    /// Optional protocol extensions this host honors. Absent on hosts without
    /// capability negotiation; clients must treat every listed extension as
    /// unavailable in that case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Read-only Pi preferences used by a new draft session. This is deliberately
/// separate from [`HostSnapshot`]: it is sourced from Pi's persisted settings
/// and model catalog, and never requires launching a Pi child process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostModelDefaults {
    pub model: Option<ModelSummary>,
    pub models: Vec<ModelSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

/// Per-device relay reachability delivered inside `host.snapshot`.
///
/// `channel_secret` is the device's high-entropy rendezvous secret. Clients
/// must store it with the same protection as host trust material and derive
/// the public channel identifier and join proof through `pix-wire`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayAccess {
    pub url: String,
    pub channel_secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub id: Uuid,
    pub name: String,
    pub availability: WorkspaceAvailability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub name: Option<String>,
    pub modified_at: String,
    pub message_count: usize,
    pub first_user_message: Option<String>,
    pub state: SessionState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub name: Option<String>,
    pub state: SessionState,
    pub model: Option<ModelSummary>,
    pub thinking_level: ThinkingLevel,
    pub messages: Vec<Value>,
    /// Optional partial assistant message captured while a TUI-owned runtime
    /// is between `message_update` and `message_end`. Older clients ignore
    /// this additive field and continue rendering the live delta stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inflight_assistant: Option<Value>,
    /// Optional TUI stream cursor covered by this snapshot. RPC snapshots do
    /// not provide a cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through_sequence: Option<u64>,
    pub pending_prompts: Vec<Value>,
    pub active_tools: Vec<ToolEvent>,
    /// Slash commands Pi exposes for this session, without host filesystem
    /// paths. Present only for connections that declared `commands.v1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandSummary>,
    /// Last reported steering and follow-up queue contents. Present only for
    /// connections that declared `queue.v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<SessionQueue>,
    /// Live token and cost usage for this session. Present only for
    /// connections that declared `usage.v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<SessionUsage>,
    /// Bounded history metadata, present only for clients that declared
    /// `session_history.v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryState>,
    /// Structured history representations, present only for clients that
    /// declared `history_items.v1`. Legacy clients use `messages` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history_items: Vec<HistoryPageItem>,
}

/// Cursor state for one bounded session history window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryState {
    /// Index of the first message in `SessionSnapshot.messages` within the
    /// host's current history view. It is used only for stable client item IDs.
    pub start_index: usize,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Revision of the history boundary captured for this view. Clients treat
    /// it as opaque and discard a page from a different revision instead of
    /// merging unrelated history.
    pub revision: u64,
    /// Semantic tail metadata, present whenever `history_presentation.v1` is
    /// negotiated. The envelope is intentionally small and may contain
    /// anchors without embedded previews when the raw page already includes
    /// the referenced messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<HistoryPresentation>,
}

/// One bounded page of older session messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHistoryPage {
    pub session_id: String,
    pub messages: Vec<Value>,
    pub start_index: usize,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub revision: u64,
    /// Structured history representations, present only for clients that
    /// declared `history_items.v1`. Legacy clients use `messages` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history_items: Vec<HistoryPageItem>,
}

/// A contiguous history item. Every logical source index in a page has one
/// representation, but a representation may be a bounded placeholder when a
/// canonical Pi message cannot fit in the wire budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryPageItem {
    /// The complete canonical Pi message for this source index.
    Message { index: usize, message: Value },
    /// A bounded representation for an oversized or otherwise unrenderable
    /// canonical message. `preview` is semantic text, never a raw JSON slice.
    Placeholder {
        index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        preview: String,
        original_bytes: usize,
        truncated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_ref: Option<String>,
    },
}

/// An anchor into the logical history message space. An embedded preview is
/// only necessary when the corresponding page item is not present in the
/// initial window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryAnchor {
    pub source_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<HistoryPreview>,
}

/// A bounded semantic preview of a history message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPreview {
    pub role: String,
    pub text: String,
    pub original_bytes: usize,
    pub truncated: bool,
}

/// Counts for process records belonging to the target Turn. These counters do
/// not carry message bodies and are safe to include in the small envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryProcessSummary {
    pub thought_count: u32,
    pub tool_count: u32,
    pub error_count: u32,
    pub omitted: bool,
}

/// Presentation state for the logical Turn represented by the semantic tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnPresentationState {
    Active,
    Completed,
    Failed,
    Aborted,
    Compacted,
}

/// Semantic tail envelope returned with a history-capable session snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPresentation {
    pub turn_state: TurnPresentationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_anchor: Option<HistoryAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_anchor: Option<HistoryAnchor>,
    pub process: HistoryProcessSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Sleeping,
    Starting,
    Idle,
    Running,
    Compacting,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSummary {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    /// Input modalities advertised by Pi, for example `text` and `image`.
    /// Older Pi versions omit the field and therefore decode as an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
    /// Pi's effective thinking choices for this model. Older hosts omit the
    /// field; clients then retain the standard compatibility choices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_levels: Vec<ThinkingLevel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolEvent {
    pub call_id: String,
    pub name: String,
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionUiRequest {
    pub id: String,
    pub method: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionUiAnswer {
    Value { value: String },
    Confirmed { confirmed: bool },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionEvent {
    pub phase: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    NotFound,
    Conflict,
    Capacity,
    UnsupportedVersion,
    PiUnavailable,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptBehavior {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Text of the messages queued behind a running agent turn. Ephemeral state:
/// the queue disappears with the Pi runtime that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionQueue {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

/// One Pi slash command invocable through a prompt. The wire shape is
/// deliberately narrower than Pi's `get_commands` entry: host filesystem
/// paths and package metadata stay on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSummary {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: CommandSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<CommandScope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSource {
    Extension,
    Prompt,
    Skill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandScope {
    User,
    Project,
    Temporary,
}

/// Cumulative token and cost usage Pi reports for one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub tokens_total: u64,
    pub cost: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_percent: Option<f64>,
}

/// Accepts only the conservative capability vocabulary `[a-z0-9._-]` so a
/// declaration can never smuggle structured data past length checks.
#[must_use]
pub fn is_valid_capability(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 24
        && value.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_'
        })
}

#[allow(clippy::too_many_lines)]
fn validate_client_request(request: &ClientRequest) -> Result<(), WireError> {
    let value = serde_json::to_value(request).map_err(WireError::Encode)?;
    validate_all_strings(&value)?;
    match request {
        ClientRequest::HostSnapshot { capabilities } => {
            if capabilities.len() > MAX_CLIENT_CAPABILITIES {
                return Err(WireError::InvalidCapability(format!(
                    "at most {MAX_CLIENT_CAPABILITIES} capabilities are allowed"
                )));
            }
            for capability in capabilities {
                if !is_valid_capability(capability) {
                    return Err(WireError::InvalidCapability(capability.clone()));
                }
            }
            Ok(())
        }
        ClientRequest::SessionAttach { session_id }
        | ClientRequest::SessionRelease { session_id }
        | ClientRequest::SessionAbort { session_id }
        | ClientRequest::ModelList { session_id } => validate_identifier("session_id", session_id),
        ClientRequest::SessionHistoryRequest {
            session_id,
            before,
            limit,
        } => {
            validate_identifier("session_id", session_id)?;
            validate_identifier("before", before)?;
            if *limit == 0 || *limit > MAX_HISTORY_PAGE_MESSAGES {
                return Err(WireError::HistoryPageSizeInvalid {
                    size: *limit,
                    limit: MAX_HISTORY_PAGE_MESSAGES,
                });
            }
            Ok(())
        }
        ClientRequest::SessionRename { session_id, name } => {
            validate_identifier("session_id", session_id)?;
            validate_text("name", name)
        }
        ClientRequest::SessionPrompt {
            session_id,
            content,
            attachments,
        }
        | ClientRequest::SessionSteer {
            session_id,
            content,
            attachments,
        }
        | ClientRequest::SessionFollowUp {
            session_id,
            content,
            attachments,
        } => {
            validate_identifier("session_id", session_id)?;
            validate_text("content", content)?;
            validate_attachment_references(attachments)
        }
        ClientRequest::SessionCompact {
            session_id,
            instructions,
        } => {
            validate_identifier("session_id", session_id)?;
            if let Some(instructions) = instructions {
                validate_text("instructions", instructions)?;
            }
            Ok(())
        }
        ClientRequest::ModelSet {
            session_id,
            provider,
            model_id,
        } => {
            validate_identifier("session_id", session_id)?;
            validate_identifier("provider", provider)?;
            validate_identifier("model_id", model_id)
        }
        ClientRequest::ThinkingSet { session_id, .. } => {
            validate_identifier("session_id", session_id)
        }
        ClientRequest::ExtensionUiRespond {
            session_id,
            extension_request_id,
            ..
        } => {
            validate_identifier("session_id", session_id)?;
            validate_identifier("extension_request_id", extension_request_id)
        }
        ClientRequest::SessionCreate { name, .. } => match name {
            Some(name) => validate_text("name", name),
            None => Ok(()),
        },
        ClientRequest::HostDefaults
        | ClientRequest::WorkspaceList
        | ClientRequest::SessionList { .. } => Ok(()),
        ClientRequest::AttachmentBegin {
            session_id,
            attachment_id,
            mime_type,
            size,
        } => validate_attachment_begin(session_id, attachment_id, mime_type, *size),
        ClientRequest::AttachmentChunk {
            attachment_id,
            data,
        } => {
            validate_attachment_id(attachment_id)?;
            validate_text("data", data)
        }
        ClientRequest::AttachmentFinish { attachment_id } => validate_attachment_id(attachment_id),
        ClientRequest::ImageGet {
            session_id,
            image_ref,
            offset: _,
            limit,
        } => {
            validate_identifier("session_id", session_id)?;
            validate_image_ref(image_ref)?;
            if *limit == 0 || u64::from(*limit) > u64::from(MAX_IMAGE_CHUNK_BYTES) {
                return Err(WireError::ImageChunkSizeInvalid {
                    size: *limit,
                    limit: MAX_IMAGE_CHUNK_BYTES,
                });
            }
            Ok(())
        }
    }
}

fn validate_attachment_begin(
    session_id: &str,
    attachment_id: &str,
    mime_type: &str,
    size: u64,
) -> Result<(), WireError> {
    validate_identifier("session_id", session_id)?;
    validate_attachment_id(attachment_id)?;
    if !ATTACHMENT_MIME_TYPES.contains(&mime_type) {
        return Err(WireError::UnsupportedAttachmentMime(mime_type.to_owned()));
    }
    if size == 0 || size > MAX_ATTACHMENT_BYTES {
        return Err(WireError::AttachmentSizeInvalid {
            size,
            limit: MAX_ATTACHMENT_BYTES,
        });
    }
    Ok(())
}

fn validate_attachment_references(attachments: &[String]) -> Result<(), WireError> {
    if attachments.len() > MAX_ATTACHMENTS_PER_REQUEST {
        return Err(WireError::TooManyAttachments {
            count: attachments.len(),
            limit: MAX_ATTACHMENTS_PER_REQUEST,
        });
    }
    for attachment_id in attachments {
        validate_attachment_id(attachment_id)?;
    }
    Ok(())
}

/// Attachment IDs are client-generated opaque handles; a conservative charset
/// keeps them safe for host-side map keys and logging.
fn validate_attachment_id(value: &str) -> Result<(), WireError> {
    if value.is_empty()
        || value.len() > 64
        || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(WireError::EmptyIdentifier("attachment_id"));
    }
    Ok(())
}

fn validate_image_ref(value: &str) -> Result<(), WireError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(WireError::InvalidImageReference);
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WireError::InvalidImageReference);
    }
    Ok(())
}

fn validate_server_event(event: &ServerEvent) -> Result<(), WireError> {
    validate_all_strings(&serde_json::to_value(event).map_err(WireError::Encode)?)?;
    if let ServerEvent::SessionHistoryPage { page } = event {
        validate_history_page(page)?;
    }
    Ok(())
}

fn validate_history_page(page: &SessionHistoryPage) -> Result<(), WireError> {
    let count = if page.history_items.is_empty() {
        page.messages.len()
    } else {
        if !page.messages.is_empty() {
            return Err(WireError::HistoryItemsInvalid(
                "messages and history_items cannot both be populated",
            ));
        }
        let expected = page.start_index;
        for (offset, item) in page.history_items.iter().enumerate() {
            let index = match item {
                HistoryPageItem::Message { index, .. }
                | HistoryPageItem::Placeholder { index, .. } => *index,
            };
            if index != expected.saturating_add(offset) {
                return Err(WireError::HistoryItemsInvalid(
                    "history item indexes must be contiguous",
                ));
            }
            if let HistoryPageItem::Placeholder { preview, .. } = item
                && preview.len() > MAX_HISTORY_PREVIEW_BYTES
            {
                return Err(WireError::HistoryItemsInvalid(
                    "history preview exceeds the bounded preview limit",
                ));
            }
        }
        page.history_items.len()
    };
    if count > usize::try_from(MAX_HISTORY_PAGE_MESSAGES).unwrap_or(usize::MAX) {
        return Err(WireError::HistoryPageSizeInvalid {
            size: u32::try_from(count).unwrap_or(u32::MAX),
            limit: MAX_HISTORY_PAGE_MESSAGES,
        });
    }
    let encoded = serde_json::to_vec(page).map_err(WireError::Encode)?;
    if encoded.len() > MAX_HISTORY_PAGE_BYTES {
        return Err(WireError::HistoryPageTooLarge {
            size: encoded.len(),
            limit: MAX_HISTORY_PAGE_BYTES,
        });
    }
    Ok(())
}

fn encode_value<T: Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    let json = serde_json::to_value(value).map_err(WireError::Encode)?;
    validate_all_strings(&json)?;
    let encoded = serde_json::to_vec(&json).map_err(WireError::Encode)?;
    validate_plaintext_size(&encoded)?;
    Ok(encoded)
}

fn validate_protocol(protocol: u16) -> Result<(), WireError> {
    if protocol != PROTOCOL_MAJOR {
        return Err(WireError::ProtocolVersion {
            found: protocol,
            supported: PROTOCOL_MAJOR,
        });
    }
    Ok(())
}

fn validate_plaintext_size(bytes: &[u8]) -> Result<(), WireError> {
    if bytes.len() > MAX_ENCRYPTED_FRAME_BYTES {
        return Err(WireError::FrameTooLarge(bytes.len()));
    }
    Ok(())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), WireError> {
    if value.trim().is_empty() {
        return Err(WireError::EmptyIdentifier(field));
    }
    validate_text(field, value)
}

fn validate_text(field: &'static str, value: &str) -> Result<(), WireError> {
    if value.len() > MAX_TEXT_FIELD_BYTES {
        return Err(WireError::TextTooLarge {
            field,
            size: value.len(),
        });
    }
    Ok(())
}

fn validate_all_strings(value: &Value) -> Result<(), WireError> {
    match value {
        Value::String(text) => validate_text("decoded_string", text),
        Value::Array(values) => values.iter().try_for_each(validate_all_strings),
        Value::Object(values) => values.values().try_for_each(validate_all_strings),
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{ClientEnvelope, ClientRequest, ServerEnvelope, ServerEvent};
    use crate::{MAX_TEXT_FIELD_BYTES, PROTOCOL_MAJOR, WireError};

    #[test]
    fn session_create_accepts_an_omitted_or_empty_name() {
        let workspace = "4cc891bc-30b9-4b5f-9298-38471d9b27ea";
        let omitted = format!(
            r#"{{"protocol":1,"request_id":7,"type":"session.create","workspace_id":"{workspace}"}}"#
        );
        let empty = format!(
            r#"{{"protocol":1,"request_id":8,"type":"session.create","workspace_id":"{workspace}","name":""}}"#
        );
        let omitted = ClientEnvelope::decode(omitted.as_bytes()).expect("omit name");
        let empty = ClientEnvelope::decode(empty.as_bytes()).expect("empty name");
        assert!(matches!(
            omitted.request,
            ClientRequest::SessionCreate { name: None, .. }
        ));
        assert!(matches!(
            empty.request,
            ClientRequest::SessionCreate {
                name: Some(name),
                ..
            } if name.is_empty()
        ));
    }

    #[test]
    fn request_round_trip_preserves_dotted_operation_name() {
        let message = ClientEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id: 7,
            request: ClientRequest::SessionPrompt {
                session_id: "session-1".into(),
                content: "hello".into(),
                attachments: Vec::new(),
            },
        };
        let encoded = message.encode().expect("encode");
        assert!(String::from_utf8_lossy(&encoded).contains("session.prompt"));
        assert_eq!(ClientEnvelope::decode(&encoded).expect("decode"), message);
    }

    #[test]
    fn history_request_and_page_use_dotted_names_and_flattened_fields() {
        let request = ClientEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id: 8,
            request: ClientRequest::SessionHistoryRequest {
                session_id: "session-1".into(),
                before: "opaque-cursor".into(),
                limit: 50,
            },
        };
        assert_eq!(
            ClientEnvelope::decode(&request.encode().expect("encode request")).expect("decode"),
            request
        );

        let event = ServerEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id: Some(8),
            event: ServerEvent::SessionHistoryPage {
                page: super::SessionHistoryPage {
                    session_id: "session-1".into(),
                    messages: vec![serde_json::json!({"role":"user","content":"old"})],
                    start_index: 0,
                    has_more: false,
                    next_cursor: None,
                    revision: 1,
                    history_items: Vec::new(),
                },
            },
        };
        let encoded = event.encode().expect("encode page");
        let value: serde_json::Value = serde_json::from_slice(&encoded).expect("JSON page");
        assert_eq!(value["type"], "session.history.page");
        assert_eq!(value["session_id"], "session-1");
        assert!(value.get("page").is_none());
        assert_eq!(
            ServerEnvelope::decode(&encoded).expect("decode page"),
            event
        );

        assert!(matches!(
            ClientEnvelope::decode(
                br#"{"protocol":1,"request_id":9,"type":"session.history.request","session_id":"session-1","before":"cursor","limit":51}"#
            ),
            Err(WireError::HistoryPageSizeInvalid { size: 51, .. })
        ));
    }

    #[test]
    fn structured_history_items_round_trip_and_validate_contiguous_indexes() {
        let event = ServerEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id: Some(11),
            event: ServerEvent::SessionHistoryPage {
                page: super::SessionHistoryPage {
                    session_id: "session-1".into(),
                    messages: Vec::new(),
                    start_index: 7,
                    has_more: true,
                    next_cursor: Some("opaque".into()),
                    revision: 12,
                    history_items: vec![
                        super::HistoryPageItem::Placeholder {
                            index: 7,
                            role: Some("user".into()),
                            preview: "question".into(),
                            original_bytes: 70 * 1024 * 1024,
                            truncated: true,
                            content_ref: None,
                        },
                        super::HistoryPageItem::Message {
                            index: 8,
                            message: serde_json::json!({"role":"assistant","content":"done"}),
                        },
                    ],
                },
            },
        };
        let encoded = event.encode().expect("encode structured page");
        let decoded = ServerEnvelope::decode(&encoded).expect("decode structured page");
        assert_eq!(decoded, event);

        let mut invalid = event.clone();
        if let ServerEvent::SessionHistoryPage { page } = &mut invalid.event {
            page.history_items[1] = super::HistoryPageItem::Message {
                index: 9,
                message: serde_json::json!({"role":"assistant","content":"gap"}),
            };
        }
        assert!(matches!(
            invalid.encode(),
            Err(WireError::HistoryItemsInvalid(
                "history item indexes must be contiguous"
            ))
        ));
    }

    #[test]
    fn history_presentation_is_additive_and_old_snapshots_still_decode() {
        let value = serde_json::json!({
            "protocol": 1,
            "type": "session.snapshot",
            "snapshot": {
                "id": "session-1",
                "name": null,
                "state": "idle",
                "model": null,
                "thinking_level": "medium",
                "messages": [],
                "pending_prompts": [],
                "active_tools": [],
                "history": {
                    "start_index": 0,
                    "has_more": false,
                    "cursor": null,
                    "revision": 1,
                    "presentation": {
                        "turn_state": "completed",
                        "user_anchor": {
                            "source_index": 0,
                            "preview": {
                                "role": "user",
                                "text": "hello",
                                "original_bytes": 42,
                                "truncated": false
                            }
                        },
                        "terminal_anchor": null,
                        "process": {
                            "thought_count": 0,
                            "tool_count": 0,
                            "error_count": 0,
                            "omitted": false
                        },
                        "error_summary": null
                    }
                }
            }
        });
        let encoded = serde_json::to_vec(&value).expect("encode snapshot");
        let envelope = ServerEnvelope::decode(&encoded).expect("decode snapshot");
        let ServerEvent::SessionSnapshot { snapshot } = envelope.event else {
            panic!("expected snapshot");
        };
        assert_eq!(
            snapshot
                .history
                .and_then(|history| history.presentation)
                .and_then(|presentation| presentation.user_anchor)
                .map(|anchor| anchor.source_index),
            Some(0)
        );
        assert!(snapshot.history_items.is_empty());
    }

    #[test]
    fn host_snapshot_negotiates_capabilities_and_old_clients_stay_compatible() {
        let negotiated = ClientEnvelope::decode(
            br#"{"protocol":1,"request_id":1,"type":"host.snapshot","capabilities":["commands.v1","queue.v1"]}"#,
        )
        .expect("decode with capabilities");
        match &negotiated.request {
            ClientRequest::HostSnapshot { capabilities } => {
                assert_eq!(
                    capabilities,
                    &["commands.v1".to_owned(), "queue.v1".to_owned()]
                );
            }
            other => panic!("unexpected request: {other:?}"),
        }

        let legacy =
            ClientEnvelope::decode(br#"{"protocol":1,"request_id":2,"type":"host.snapshot"}"#)
                .expect("decode without capabilities");
        match legacy.request {
            ClientRequest::HostSnapshot { capabilities } => assert!(capabilities.is_empty()),
            other => panic!("unexpected request: {other:?}"),
        }

        assert!(matches!(
            ClientEnvelope::decode(
                br#"{"protocol":1,"request_id":3,"type":"host.snapshot","capabilities":["bad capability!"]}"#
            ),
            Err(WireError::InvalidCapability(_))
        ));
    }

    #[test]
    fn prompt_attachments_are_bounded_and_backward_compatible() {
        let with_attachments = ClientEnvelope::decode(
            br#"{"protocol":1,"request_id":1,"type":"session.prompt","session_id":"s","content":"see this","attachments":["att-1","att-2"]}"#,
        )
        .expect("decode attachments");
        match &with_attachments.request {
            ClientRequest::SessionPrompt { attachments, .. } => {
                assert_eq!(attachments.len(), 2);
            }
            other => panic!("unexpected request: {other:?}"),
        }

        ClientEnvelope::decode(
            br#"{"protocol":1,"request_id":2,"type":"session.prompt","session_id":"s","content":"plain"}"#,
        )
        .expect("legacy prompt without attachments");

        let nine_attachments = (1..=9)
            .map(|index| format!(r#""att-{index}""#))
            .collect::<Vec<_>>()
            .join(",");
        let nine = format!(
            r#"{{"protocol":1,"request_id":3,"type":"session.prompt","session_id":"s","content":"x","attachments":[{nine_attachments}]}}"#
        );
        let nine = ClientEnvelope::decode(nine.as_bytes()).expect("nine attachments are allowed");
        match nine.request {
            ClientRequest::SessionPrompt { attachments, .. } => assert_eq!(attachments.len(), 9),
            other => panic!("unexpected request: {other:?}"),
        }

        assert!(matches!(
            ClientEnvelope::decode(
                br#"{"protocol":1,"request_id":4,"type":"session.prompt","session_id":"s","content":"x","attachments":["1","2","3","4","5","6","7","8","9","10"]}"#
            ),
            Err(WireError::TooManyAttachments { count: 10, limit: 9 })
        ));
    }

    #[test]
    fn attachment_transfer_requests_enforce_mime_size_and_id_shape() {
        for mime in ["image/png", "image/jpeg", "image/webp", "image/gif"] {
            let request = format!(
                r#"{{"protocol":1,"request_id":1,"type":"attachment.begin","session_id":"s","attachment_id":"att-1","mime_type":"{mime}","size":1024}}"#
            );
            ClientEnvelope::decode(request.as_bytes())
                .unwrap_or_else(|error| panic!("accept {mime}: {error}"));
        }

        assert!(matches!(
            ClientEnvelope::decode(
                br#"{"protocol":1,"request_id":2,"type":"attachment.begin","session_id":"s","attachment_id":"att-1","mime_type":"image/tiff","size":10}"#
            ),
            Err(WireError::UnsupportedAttachmentMime(_))
        ));
        assert!(matches!(
            ClientEnvelope::decode(
                br#"{"protocol":1,"request_id":3,"type":"attachment.begin","session_id":"s","attachment_id":"att-1","mime_type":"image/png","size":0}"#
            ),
            Err(WireError::AttachmentSizeInvalid { size: 0, .. })
        ));
        assert!(matches!(
            ClientEnvelope::decode(
                br#"{"protocol":1,"request_id":4,"type":"attachment.chunk","attachment_id":"bad id","data":"aGk="}"#
            ),
            Err(WireError::EmptyIdentifier("attachment_id"))
        ));
    }

    #[test]
    fn image_get_requests_are_bounded_and_reference_sha256_assets() {
        let valid = ClientEnvelope::decode(
            br#"{"protocol":1,"request_id":1,"type":"image.get","session_id":"session","image_ref":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","offset":0,"limit":1024}"#,
        )
        .expect("valid image range");
        assert!(matches!(
            valid.request,
            ClientRequest::ImageGet { limit: 1024, .. }
        ));

        assert!(matches!(
            ClientEnvelope::decode(
                br#"{"protocol":1,"request_id":2,"type":"image.get","session_id":"session","image_ref":"sha256:not-a-hash","offset":0,"limit":1024}"#
            ),
            Err(WireError::InvalidImageReference)
        ));
        assert!(matches!(
            ClientEnvelope::decode(
                br#"{"protocol":1,"request_id":3,"type":"image.get","session_id":"session","image_ref":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","offset":0,"limit":524289}"#
            ),
            Err(WireError::ImageChunkSizeInvalid { .. })
        ));
    }

    #[test]
    fn recursively_rejects_oversized_event_text() {
        let message = ServerEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id: None,
            event: ServerEvent::AssistantDelta {
                session_id: "session-1".into(),
                delta: serde_json::json!({"nested": "x".repeat(MAX_TEXT_FIELD_BYTES + 1)}),
            },
        };
        assert!(matches!(
            message.encode(),
            Err(WireError::TextTooLarge { .. })
        ));
    }
}
