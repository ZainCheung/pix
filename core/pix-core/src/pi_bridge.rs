//! Pi 0.84 RPC compatibility mapping.
//!
//! Pi-specific JSON field names stop here. The host dispatcher and Apple
//! clients operate only on `pix-wire` types.

use pix_wire::{
    CompactionEvent, ErrorCode, ExtensionUiRequest, ModelSummary, ServerEvent,
    SessionSnapshot as WireSessionSnapshot, SessionState, ThinkingLevel, ToolEvent,
};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::pi_defaults::supported_thinking_levels;
use crate::pi_rpc::{PiEvent, PiResponse};
use crate::session::SessionSnapshot;
use crate::session_lock::SessionId;

/// Converts an authoritative Pi snapshot into the stable Pix protocol shape.
///
/// # Errors
///
/// Returns [`PiBridgeError`] when a model or thinking level has a shape that
/// is incompatible with the verified Pi version.
pub fn session_snapshot(
    session_id: SessionId,
    snapshot: SessionSnapshot,
) -> Result<WireSessionSnapshot, PiBridgeError> {
    let state = if snapshot.is_compacting {
        SessionState::Compacting
    } else if snapshot.is_streaming {
        SessionState::Running
    } else {
        SessionState::Idle
    };
    let model = snapshot.model.as_ref().map(model_summary).transpose()?;
    let pending_prompts = (0..snapshot.pending_message_count)
        .map(|_| serde_json::json!({"status": "pending"}))
        .collect();
    Ok(WireSessionSnapshot {
        id: session_id.to_string(),
        name: snapshot.session_name,
        state,
        model,
        thinking_level: parse_thinking_level(&snapshot.thinking_level)?,
        messages: snapshot.messages,
        pending_prompts,
        // Pi's authoritative state does not expose in-progress tool payloads.
        // Live tool events repopulate this disposable client state.
        active_tools: Vec::new(),
    })
}

/// Converts a `get_available_models` response into protocol model summaries.
///
/// # Errors
///
/// Returns [`PiBridgeError`] for missing response data or malformed models.
pub fn available_models(response: &PiResponse) -> Result<Vec<ModelSummary>, PiBridgeError> {
    let models = response
        .data
        .as_ref()
        .and_then(|data| data.get("models"))
        .and_then(Value::as_array)
        .ok_or(PiBridgeError::InvalidResponse(
            "get_available_models.models",
        ))?;
    models.iter().map(model_summary).collect()
}

/// Converts one raw Pi event into zero or one stable Pix protocol events.
///
/// Unknown informational events are deliberately ignored. A newly supported
/// Pi version can extend this mapping without changing the host dispatcher.
///
/// # Errors
///
/// Returns [`PiBridgeError`] when a known event is missing required fields.
pub fn event(session_id: SessionId, event: PiEvent) -> Result<Option<ServerEvent>, PiBridgeError> {
    let session_id = session_id.to_string();
    let PiEvent::Event {
        event_type,
        payload,
    } = event
    else {
        return Ok(Some(match event {
            PiEvent::ProtocolError { .. } => ServerEvent::Error {
                code: ErrorCode::PiUnavailable,
                message: "Pi RPC produced an invalid event".to_owned(),
                retryable: true,
            },
            PiEvent::Closed => ServerEvent::SessionState {
                session_id,
                state: SessionState::Unavailable,
            },
            PiEvent::Event { .. } => unreachable!(),
        }));
    };

    let mapped = match event_type.as_str() {
        "agent_start" => Some(ServerEvent::SessionState {
            session_id,
            state: SessionState::Running,
        }),
        "agent_settled" => Some(ServerEvent::SessionState {
            session_id,
            state: SessionState::Idle,
        }),
        "message_start" if message_role(&payload) == Some("user") => {
            Some(ServerEvent::UserMessage {
                session_id,
                message: required_value(&payload, "message")?.clone(),
            })
        }
        "message_update" => Some(ServerEvent::AssistantDelta {
            session_id,
            delta: required_value(&payload, "assistantMessageEvent")?.clone(),
        }),
        "message_end" if message_role(&payload) == Some("assistant") => {
            Some(ServerEvent::AssistantMessage {
                session_id,
                message: required_value(&payload, "message")?.clone(),
            })
        }
        "tool_execution_start" => Some(ServerEvent::ToolStart {
            session_id,
            tool: tool_event(&payload, "args", None)?,
        }),
        "tool_execution_update" => Some(ServerEvent::ToolUpdate {
            session_id,
            tool: tool_event(&payload, "partialResult", None)?,
        }),
        "tool_execution_end" => Some(ServerEvent::ToolEnd {
            session_id,
            tool: tool_event(
                &payload,
                "result",
                payload.get("isError").and_then(Value::as_bool),
            )?,
        }),
        "extension_ui_request" => Some(ServerEvent::ExtensionUiRequest {
            session_id,
            request: ExtensionUiRequest {
                id: required_string(&payload, "id")?.to_owned(),
                method: required_string(&payload, "method")?.to_owned(),
                payload: without_fields(&payload, &["type", "id", "method"]),
            },
        }),
        "compaction_start" => Some(ServerEvent::Compaction {
            session_id,
            compaction: CompactionEvent {
                phase: "start".to_owned(),
                reason: required_string(&payload, "reason")?.to_owned(),
                result: None,
            },
        }),
        "compaction_end" => Some(ServerEvent::Compaction {
            session_id,
            compaction: CompactionEvent {
                phase: "end".to_owned(),
                reason: required_string(&payload, "reason")?.to_owned(),
                result: payload
                    .get("result")
                    .filter(|value| !value.is_null())
                    .cloned(),
            },
        }),
        "extension_error" => Some(ServerEvent::Error {
            code: ErrorCode::PiUnavailable,
            message: "A Pi extension reported an error".to_owned(),
            retryable: false,
        }),
        _ => None,
    };
    Ok(mapped)
}

fn model_summary(value: &Value) -> Result<ModelSummary, PiBridgeError> {
    let reasoning = value
        .get("reasoning")
        .and_then(Value::as_bool)
        .ok_or(PiBridgeError::MissingField("reasoning"))?;
    Ok(ModelSummary {
        provider: required_string(value, "provider")?.to_owned(),
        id: required_string(value, "id")?.to_owned(),
        name: required_string(value, "name")?.to_owned(),
        reasoning,
        thinking_levels: supported_thinking_levels(value, reasoning),
    })
}

fn parse_thinking_level(value: &str) -> Result<ThinkingLevel, PiBridgeError> {
    match value {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::Xhigh),
        "max" => Ok(ThinkingLevel::Max),
        _ => Err(PiBridgeError::UnknownThinkingLevel(value.to_owned())),
    }
}

fn tool_event(
    payload: &Value,
    payload_field: &'static str,
    is_error: Option<bool>,
) -> Result<ToolEvent, PiBridgeError> {
    Ok(ToolEvent {
        call_id: required_string(payload, "toolCallId")?.to_owned(),
        name: required_string(payload, "toolName")?.to_owned(),
        payload: required_value(payload, payload_field)?.clone(),
        is_error,
    })
}

fn message_role(payload: &Value) -> Option<&str> {
    payload.get("message")?.get("role")?.as_str()
}

fn required_string<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, PiBridgeError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(PiBridgeError::MissingField(field))
}

fn required_value<'a>(value: &'a Value, field: &'static str) -> Result<&'a Value, PiBridgeError> {
    value.get(field).ok_or(PiBridgeError::MissingField(field))
}

fn without_fields(value: &Value, fields: &[&str]) -> Value {
    let mut object = value.as_object().cloned().unwrap_or_else(Map::new);
    for field in fields {
        object.remove(*field);
    }
    Value::Object(object)
}

#[derive(Debug, Error)]
pub enum PiBridgeError {
    #[error("Pi RPC field is missing or invalid: {0}")]
    MissingField(&'static str),
    #[error("Pi RPC response has an invalid shape: {0}")]
    InvalidResponse(&'static str),
    #[error("Pi RPC returned an unknown thinking level: {0}")]
    UnknownThinkingLevel(String),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{event, session_snapshot};
    use crate::pi_rpc::PiEvent;
    use crate::session::SessionSnapshot;
    use crate::session_lock::SessionId;
    use pix_wire::{ServerEvent, SessionState, ThinkingLevel};

    #[test]
    fn snapshot_conversion_uses_stable_pix_identity_and_pending_placeholders() {
        let id = SessionId::new();
        let snapshot = session_snapshot(
            id,
            SessionSnapshot {
                session_id: "pi-internal-id".to_owned(),
                session_name: Some("Test".to_owned()),
                model: Some(json!({
                    "provider": "test",
                    "id": "model-1",
                    "name": "Model 1",
                    "reasoning": true
                })),
                thinking_level: "high".to_owned(),
                is_streaming: true,
                is_compacting: false,
                pending_message_count: 2,
                messages: vec![json!({"role": "user", "content": "hello"})],
            },
        )
        .expect("convert snapshot");

        assert_eq!(snapshot.id, id.to_string());
        assert_eq!(snapshot.state, SessionState::Running);
        assert_eq!(snapshot.thinking_level, ThinkingLevel::High);
        assert_eq!(snapshot.pending_prompts.len(), 2);
    }

    #[test]
    fn maps_stream_and_tool_events_without_exposing_pi_names_to_dispatcher() {
        let id = SessionId::new();
        let delta = event(
            id,
            PiEvent::Event {
                event_type: "message_update".to_owned(),
                payload: json!({
                    "type": "message_update",
                    "assistantMessageEvent": {"type": "text_delta", "delta": "hi"}
                }),
            },
        )
        .expect("map delta")
        .expect("known event");
        assert!(matches!(delta, ServerEvent::AssistantDelta { .. }));

        let tool = event(
            id,
            PiEvent::Event {
                event_type: "tool_execution_end".to_owned(),
                payload: json!({
                    "type": "tool_execution_end",
                    "toolCallId": "call-1",
                    "toolName": "read",
                    "result": {"content": "done"},
                    "isError": false
                }),
            },
        )
        .expect("map tool")
        .expect("known event");
        assert!(matches!(tool, ServerEvent::ToolEnd { .. }));
    }
}
