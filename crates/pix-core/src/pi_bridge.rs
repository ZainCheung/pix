//! Pi 0.84 RPC compatibility mapping.
//!
//! Pi-specific JSON field names stop here. The host dispatcher and Apple
//! clients operate only on `pix-wire` types.

use pix_wire::{
    CommandScope, CommandSource, CommandSummary, CompactionEvent, ErrorCode, ExtensionUiRequest,
    ModelSummary, ServerEvent, SessionQueue, SessionSnapshot as WireSessionSnapshot, SessionState,
    SessionUsage, ThinkingLevel, ToolEvent,
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
        // Capability-gated enrichment filled in by the host dispatcher after
        // this conversion: commands, queue text, and usage.
        commands: Vec::new(),
        queue: None,
        usage: None,
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

/// Converts a `get_commands` response into protocol command summaries.
///
/// Pi's `sourceInfo` carries host filesystem paths and package metadata; only
/// the non-secret vocabulary crosses this bridge.
///
/// # Errors
///
/// Returns [`PiBridgeError`] for missing data or malformed command entries.
pub fn commands(response: &PiResponse) -> Result<Vec<CommandSummary>, PiBridgeError> {
    let commands = response
        .data
        .as_ref()
        .and_then(|data| data.get("commands"))
        .and_then(Value::as_array)
        .ok_or(PiBridgeError::InvalidResponse("get_commands.commands"))?;
    commands.iter().map(command_summary).collect()
}

/// Converts a `get_available_thinking_levels` response for the session's
/// current model into protocol thinking levels.
///
/// # Errors
///
/// Returns [`PiBridgeError`] when Pi omits the `levels` array or reports an
/// unknown level name.
pub fn thinking_levels(response: &PiResponse) -> Result<Vec<ThinkingLevel>, PiBridgeError> {
    let levels = response
        .data
        .as_ref()
        .and_then(|data| data.get("levels"))
        .and_then(Value::as_array)
        .ok_or(PiBridgeError::InvalidResponse(
            "get_available_thinking_levels.levels",
        ))?;
    levels
        .iter()
        .map(|level| {
            let name = level.as_str().ok_or(PiBridgeError::InvalidResponse(
                "get_available_thinking_levels.levels",
            ))?;
            parse_thinking_level(name)
        })
        .collect()
}

/// Converts a `get_session_stats` response into protocol usage. `sessionFile`
/// and `sessionId` are host-local and never cross this bridge.
///
/// # Errors
///
/// Returns [`PiBridgeError`] when the usage shape is incompatible.
pub fn usage(response: &PiResponse) -> Result<SessionUsage, PiBridgeError> {
    let data = response
        .data
        .as_ref()
        .ok_or(PiBridgeError::InvalidResponse("get_session_stats"))?;
    let tokens = data
        .get("tokens")
        .ok_or(PiBridgeError::InvalidResponse("get_session_stats.tokens"))?;
    let context = data.get("contextUsage").filter(|value| !value.is_null());
    let (context_tokens, context_window, context_percent) = match context {
        Some(context) => (
            context.get("tokens").and_then(Value::as_u64),
            context.get("contextWindow").and_then(Value::as_u64),
            context.get("percent").and_then(Value::as_f64),
        ),
        None => (None, None, None),
    };
    Ok(SessionUsage {
        tokens_total: tokens.get("total").and_then(Value::as_u64).ok_or(
            PiBridgeError::InvalidResponse("get_session_stats.tokens.total"),
        )?,
        cost: data
            .get("cost")
            .and_then(Value::as_f64)
            .ok_or(PiBridgeError::InvalidResponse("get_session_stats.cost"))?,
        context_tokens,
        context_window,
        context_percent,
    })
}

/// Converts one raw Pi event into zero or one stable Pix protocol events.
///
/// Unknown informational events are deliberately ignored. A newly supported
/// Pi version can extend this mapping without changing the host dispatcher.
///
/// # Errors
///
/// Returns [`PiBridgeError`] when a known event is missing required fields.
#[allow(clippy::too_many_lines)]
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
        "queue_update" => Some(ServerEvent::SessionQueue {
            session_id,
            queue: SessionQueue {
                steering: string_array(&payload, "steering")?,
                follow_up: string_array(&payload, "followUp")?,
            },
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

fn command_summary(value: &Value) -> Result<CommandSummary, PiBridgeError> {
    let source = match required_string(value, "source")? {
        "extension" => CommandSource::Extension,
        "prompt" => CommandSource::Prompt,
        "skill" => CommandSource::Skill,
        other => return Err(PiBridgeError::UnknownCommandSource(other.to_owned())),
    };
    let scope = value
        .get("sourceInfo")
        .and_then(|info| info.get("scope"))
        .and_then(Value::as_str)
        .map(|scope| match scope {
            "user" => Ok(CommandScope::User),
            "project" => Ok(CommandScope::Project),
            "temporary" => Ok(CommandScope::Temporary),
            other => Err(PiBridgeError::UnknownCommandScope(other.to_owned())),
        })
        .transpose()?;
    Ok(CommandSummary {
        name: required_string(value, "name")?.to_owned(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        source,
        scope,
    })
}

fn string_array(payload: &Value, field: &'static str) -> Result<Vec<String>, PiBridgeError> {
    let values = payload
        .get(field)
        .and_then(Value::as_array)
        .ok_or(PiBridgeError::MissingField(field))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or(PiBridgeError::MissingField(field))
        })
        .collect()
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
    #[error("Pi RPC returned an unknown command source: {0}")]
    UnknownCommandSource(String),
    #[error("Pi RPC returned an unknown command scope: {0}")]
    UnknownCommandScope(String),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PiResponse, event, session_snapshot};
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

    #[test]
    fn maps_queue_update_without_pi_field_names() {
        let id = SessionId::new();
        let mapped = event(
            id,
            PiEvent::Event {
                event_type: "queue_update".to_owned(),
                payload: json!({
                    "type": "queue_update",
                    "steering": ["Focus on error handling"],
                    "followUp": ["Then summarize"]
                }),
            },
        )
        .expect("map queue")
        .expect("known event");
        let ServerEvent::SessionQueue { queue, .. } = mapped else {
            panic!("expected session queue event");
        };
        assert_eq!(queue.steering, ["Focus on error handling"]);
        assert_eq!(queue.follow_up, ["Then summarize"]);
    }

    #[test]
    fn commands_response_drops_host_paths_and_maps_vocabulary() {
        let response = PiResponse {
            command: "get_commands".to_owned(),
            data: Some(json!({
                "commands": [
                    {
                        "name": "review",
                        "description": "Review current changes",
                        "source": "extension",
                        "sourceInfo": {
                            "path": "/Users/example/.pi/agent/extensions/review.ts",
                            "source": "review",
                            "scope": "user",
                            "origin": "top-level"
                        }
                    },
                    {
                        "name": "fix-tests",
                        "source": "prompt",
                        "sourceInfo": {
                            "path": "/Users/example/Developer/app/.pi/prompts/fix-tests.md",
                            "scope": "project",
                            "origin": "top-level"
                        }
                    },
                    {
                        "name": "skill:ship",
                        "source": "skill",
                        "sourceInfo": {
                            "path": "/Users/example/.pi/agent/skills/ship",
                            "scope": "temporary",
                            "origin": "package"
                        }
                    }
                ]
            })),
        };

        let commands = super::commands(&response).expect("map commands");
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].name, "review");
        assert_eq!(
            commands[0].description.as_deref(),
            Some("Review current changes")
        );
        assert_eq!(commands[0].source, pix_wire::CommandSource::Extension);
        assert_eq!(commands[0].scope, Some(pix_wire::CommandScope::User));
        assert_eq!(commands[2].name, "skill:ship");
        assert_eq!(commands[2].scope, Some(pix_wire::CommandScope::Temporary));

        let encoded = serde_json::to_value(&commands).expect("encode commands");
        let text = encoded.to_string();
        assert!(
            !text.contains("/Users/"),
            "host paths must not cross the bridge: {text}"
        );
        assert!(
            !text.contains("sourceInfo"),
            "source metadata must not cross the bridge: {text}"
        );
    }

    #[test]
    fn thinking_levels_and_usage_use_authoritative_pi_values() {
        let levels = super::thinking_levels(&PiResponse {
            command: "get_available_thinking_levels".to_owned(),
            data: Some(json!({"levels": ["off", "low", "high", "xhigh"]})),
        })
        .expect("map levels");
        assert_eq!(
            levels,
            vec![
                pix_wire::ThinkingLevel::Off,
                pix_wire::ThinkingLevel::Low,
                pix_wire::ThinkingLevel::High,
                pix_wire::ThinkingLevel::Xhigh
            ]
        );

        let usage = super::usage(&PiResponse {
            command: "get_session_stats".to_owned(),
            data: Some(json!({
                "sessionFile": "/Users/example/.pi/agent/sessions/x.jsonl",
                "sessionId": "pi-internal",
                "userMessages": 3,
                "assistantMessages": 2,
                "toolCalls": 4,
                "toolResults": 4,
                "totalMessages": 5,
                "tokens": {"input": 100, "output": 50, "cacheRead": 10, "cacheWrite": 5, "total": 165},
                "cost": 0.0125,
                "contextUsage": {"tokens": 4096, "contextWindow": 200_000, "percent": 2.05}
            })),
        })
        .expect("map usage");
        assert_eq!(usage.tokens_total, 165);
        assert!((usage.cost - 0.0125).abs() < f64::EPSILON);
        assert_eq!(usage.context_tokens, Some(4096));
        assert_eq!(usage.context_window, Some(200_000));
        assert_eq!(usage.context_percent, Some(2.05));
        let encoded = serde_json::to_value(&usage).expect("encode usage");
        assert!(!encoded.to_string().contains("/Users/"));
    }
}
