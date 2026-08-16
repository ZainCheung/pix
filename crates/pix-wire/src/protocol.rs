use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{MAX_ENCRYPTED_FRAME_BYTES, MAX_TEXT_FIELD_BYTES, PROTOCOL_MAJOR, WireError};

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
    #[serde(rename = "host.snapshot")]
    HostSnapshot,
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
    #[serde(rename = "session.rename")]
    SessionRename { session_id: String, name: String },
    #[serde(rename = "session.release")]
    SessionRelease { session_id: String },
    #[serde(rename = "session.prompt")]
    SessionPrompt { session_id: String, content: String },
    #[serde(rename = "session.steer")]
    SessionSteer { session_id: String, content: String },
    #[serde(rename = "session.follow_up")]
    SessionFollowUp { session_id: String, content: String },
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
    pub pending_prompts: Vec<Value>,
    pub active_tools: Vec<ToolEvent>,
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

fn validate_client_request(request: &ClientRequest) -> Result<(), WireError> {
    let value = serde_json::to_value(request).map_err(WireError::Encode)?;
    validate_all_strings(&value)?;
    match request {
        ClientRequest::SessionAttach { session_id }
        | ClientRequest::SessionRelease { session_id }
        | ClientRequest::SessionAbort { session_id }
        | ClientRequest::ModelList { session_id } => validate_identifier("session_id", session_id),
        ClientRequest::SessionRename { session_id, name } => {
            validate_identifier("session_id", session_id)?;
            validate_text("name", name)
        }
        ClientRequest::SessionPrompt {
            session_id,
            content,
        }
        | ClientRequest::SessionSteer {
            session_id,
            content,
        }
        | ClientRequest::SessionFollowUp {
            session_id,
            content,
        } => {
            validate_identifier("session_id", session_id)?;
            validate_text("content", content)
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
        ClientRequest::HostSnapshot
        | ClientRequest::HostDefaults
        | ClientRequest::WorkspaceList
        | ClientRequest::SessionList { .. } => Ok(()),
    }
}

fn validate_server_event(event: &ServerEvent) -> Result<(), WireError> {
    validate_all_strings(&serde_json::to_value(event).map_err(WireError::Encode)?)
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
            },
        };
        let encoded = message.encode().expect("encode");
        assert!(String::from_utf8_lossy(&encoded).contains("session.prompt"));
        assert_eq!(ClientEnvelope::decode(&encoded).expect("decode"), message);
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
