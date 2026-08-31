use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::{ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// Pix rejects an individual Pi RPC record larger than 16 MiB.
pub const MAX_RPC_RECORD_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A Pi RPC command supported by the Pix compatibility adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiCommand {
    Prompt {
        message: String,
        #[serde(rename = "streamingBehavior", skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<StreamingBehavior>,
        #[serde(skip_serializing_if = "<[PiImage]>::is_empty")]
        images: Vec<PiImage>,
    },
    Steer {
        message: String,
        #[serde(skip_serializing_if = "<[PiImage]>::is_empty")]
        images: Vec<PiImage>,
    },
    FollowUp {
        message: String,
        #[serde(skip_serializing_if = "<[PiImage]>::is_empty")]
        images: Vec<PiImage>,
    },
    Abort,
    GetState,
    GetMessages,
    GetAvailableModels,
    SetModel {
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    SetThinkingLevel {
        level: ThinkingLevel,
    },
    Compact {
        #[serde(rename = "customInstructions", skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    SetSessionName {
        name: String,
    },
    ExtensionUiResponse {
        id: String,
        #[serde(flatten)]
        response: ExtensionUiAnswer,
    },
    GetCommands,
    GetAvailableThinkingLevels,
    GetSessionStats,
}

impl PiCommand {
    fn command_name(&self) -> &'static str {
        match self {
            Self::Prompt { .. } => "prompt",
            Self::Steer { .. } => "steer",
            Self::FollowUp { .. } => "follow_up",
            Self::Abort => "abort",
            Self::GetState => "get_state",
            Self::GetMessages => "get_messages",
            Self::GetAvailableModels => "get_available_models",
            Self::SetModel { .. } => "set_model",
            Self::SetThinkingLevel { .. } => "set_thinking_level",
            Self::Compact { .. } => "compact",
            Self::SetSessionName { .. } => "set_session_name",
            Self::ExtensionUiResponse { .. } => "extension_ui_response",
            Self::GetCommands => "get_commands",
            Self::GetAvailableThinkingLevels => "get_available_thinking_levels",
            Self::GetSessionStats => "get_session_stats",
        }
    }
}

/// Pi `ImageContent`: base64 bytes plus the declared mime type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PiImage {
    /// Fixed discriminator Pi requires on every content part.
    #[serde(rename = "type")]
    image_type: &'static str,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub data: String,
}

impl PiImage {
    #[must_use]
    pub fn new(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            image_type: "image",
            mime_type: mime_type.into(),
            data: data.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StreamingBehavior {
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "followUp")]
    FollowUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ExtensionUiAnswer {
    Value { value: String },
    Confirmed { confirmed: bool },
    Cancelled { cancelled: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PiResponse {
    pub command: String,
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcSnapshot {
    pub state: Value,
    pub messages: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PiEvent {
    Event {
        /// TUI bridge events carry a stream-local cursor; native RPC events
        /// leave it absent because the RPC protocol has no snapshot cursor.
        sequence: Option<u64>,
        event_type: String,
        payload: Value,
    },
    ProtocolError {
        message: String,
    },
    Closed,
}

type PendingResult = Result<Value, RpcError>;
type PendingSender = mpsc::SyncSender<PendingResult>;

struct Shared {
    pending: Mutex<HashMap<String, PendingSender>>,
    subscribers: Mutex<Vec<mpsc::Sender<PiEvent>>>,
    terminal_error: Mutex<Option<String>>,
}

/// Correlated Pi RPC client over a child process's stdin and stdout.
pub struct RpcClient {
    input: Mutex<Option<ChildStdin>>,
    shared: Arc<Shared>,
    next_request_id: AtomicU64,
    dispatcher: Mutex<Option<JoinHandle<()>>>,
}

impl RpcClient {
    /// Starts a strict LF-only JSONL dispatcher for Pi stdout.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError`] if the operating system cannot start the output
    /// dispatcher thread.
    pub fn new(input: ChildStdin, output: ChildStdout) -> Result<Self, RpcError> {
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            terminal_error: Mutex::new(None),
        });
        let dispatcher_shared = Arc::clone(&shared);
        let dispatcher = thread::Builder::new()
            .name("pix-pi-rpc".to_owned())
            .spawn(move || dispatch_output(output, &dispatcher_shared))
            .map_err(RpcError::DispatcherStart)?;
        Ok(Self {
            input: Mutex::new(Some(input)),
            shared,
            next_request_id: AtomicU64::new(1),
            dispatcher: Mutex::new(Some(dispatcher)),
        })
    }

    /// Subscribes to uncorrelated Pi events and terminal protocol status.
    #[must_use]
    pub fn subscribe(&self) -> mpsc::Receiver<PiEvent> {
        let (sender, receiver) = mpsc::channel();
        self.shared
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(sender);
        receiver
    }

    /// Sends one command and waits for its matching response.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError`] for serialization, I/O, timeout, process closure,
    /// malformed output, correlation, or Pi command failures.
    pub fn request(&self, command: &PiCommand, timeout: Duration) -> Result<PiResponse, RpcError> {
        if let Some(message) = self
            .shared
            .terminal_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Err(RpcError::Closed(message));
        }

        let request_id = format!(
            "pix-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        );
        let mut value = serde_json::to_value(command).map_err(RpcError::Encode)?;
        let object = value.as_object_mut().ok_or(RpcError::InvalidCommandShape)?;
        object.insert("id".to_owned(), Value::String(request_id.clone()));
        let encoded = encode_jsonl(&value)?;

        let (sender, receiver) = mpsc::sync_channel(1);
        self.shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(request_id.clone(), sender);

        let write_result = self
            .input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .ok_or_else(|| RpcError::Closed("Pi RPC stdin is closed".to_owned()))
            .and_then(|input| input.write_all(&encoded).map_err(RpcError::Write));
        if let Err(error) = write_result {
            remove_pending(&self.shared, &request_id);
            return Err(error);
        }

        let raw = match receiver.recv_timeout(timeout) {
            Ok(result) => result?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                remove_pending(&self.shared, &request_id);
                return Err(RpcError::Timeout {
                    command: command.command_name(),
                    timeout,
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(RpcError::Closed("Pi RPC dispatcher stopped".to_owned()));
            }
        };
        parse_response(&raw, command.command_name())
    }

    /// Reads a fresh authoritative state and message snapshot from Pi.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError`] when either RPC command fails or Pi returns an
    /// incompatible response shape.
    pub fn snapshot(&self, timeout: Duration) -> Result<RpcSnapshot, RpcError> {
        let state = self.state(timeout)?;
        let messages = self.messages(timeout)?;
        Ok(RpcSnapshot { state, messages })
    }

    /// Reads only Pi's authoritative runtime state. History-capable Host
    /// clients use the native JSONL file for bounded pages so a huge
    /// `get_messages` response never has to cross the Pi RPC record limit.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError`] when Pi rejects the request, times out, or returns
    /// a response without state data.
    pub fn state(&self, timeout: Duration) -> Result<Value, RpcError> {
        self.request(&PiCommand::GetState, timeout)?
            .data
            .ok_or(RpcError::MissingResponseData("get_state"))
    }

    /// Reads Pi's complete in-memory message view for legacy clients that do
    /// not negotiate `session_history.v1`.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError`] when Pi rejects the request, times out, or returns
    /// a response without a `messages` array.
    pub fn messages(&self, timeout: Duration) -> Result<Vec<Value>, RpcError> {
        let messages_response = self
            .request(&PiCommand::GetMessages, timeout)?
            .data
            .ok_or(RpcError::MissingResponseData("get_messages"))?;
        messages_response
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(RpcError::InvalidResponseData("get_messages.messages"))
            .cloned()
    }

    /// Closes Pi stdin and waits for the output dispatcher to finish.
    pub fn close(&self) {
        self.input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(dispatcher) = self
            .dispatcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = dispatcher.join();
        }
    }
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        self.input
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(dispatcher) = self
            .dispatcher
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = dispatcher.join();
        }
    }
}

fn dispatch_output(output: ChildStdout, shared: &Shared) {
    let result = read_lf_records(output, |record| dispatch_record(record, shared));
    let terminal_message = match result {
        Ok(()) => "Pi RPC stdout closed".to_owned(),
        Err(error) => error.to_string(),
    };
    *shared
        .terminal_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(terminal_message.clone());
    fail_all_pending(shared, &terminal_message);
    let final_event = if terminal_message == "Pi RPC stdout closed" {
        PiEvent::Closed
    } else {
        PiEvent::ProtocolError {
            message: terminal_message,
        }
    };
    broadcast(shared, &final_event);
}

fn dispatch_record(record: &[u8], shared: &Shared) -> Result<(), RpcError> {
    let value: Value = serde_json::from_slice(record).map_err(RpcError::Decode)?;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or(RpcError::MissingType)?;
    if event_type == "response"
        && let Some(request_id) = value.get("id").and_then(Value::as_str)
        && let Some(sender) = remove_pending(shared, request_id)
    {
        let _ = sender.send(Ok(value));
        return Ok(());
    }
    broadcast(
        shared,
        &PiEvent::Event {
            sequence: None,
            event_type: event_type.to_owned(),
            payload: value,
        },
    );
    Ok(())
}

fn parse_response(value: &Value, expected_command: &'static str) -> Result<PiResponse, RpcError> {
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .ok_or(RpcError::MissingResponseCommand)?;
    if command != expected_command {
        return Err(RpcError::CommandMismatch {
            expected: expected_command,
            actual: command.to_owned(),
        });
    }
    let success = value
        .get("success")
        .and_then(Value::as_bool)
        .ok_or(RpcError::MissingSuccess)?;
    if !success {
        return Err(RpcError::Rejected {
            command: command.to_owned(),
            message: value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Pi rejected the command")
                .to_owned(),
        });
    }
    Ok(PiResponse {
        command: command.to_owned(),
        data: value.get("data").cloned(),
    })
}

fn encode_jsonl(value: &Value) -> Result<Vec<u8>, RpcError> {
    let mut encoded = serde_json::to_vec(value).map_err(RpcError::Encode)?;
    if encoded.len() > MAX_RPC_RECORD_BYTES {
        return Err(RpcError::RecordTooLarge(encoded.len()));
    }
    encoded.push(b'\n');
    Ok(encoded)
}

fn read_lf_records<R, F>(mut reader: R, mut on_record: F) -> Result<(), RpcError>
where
    R: Read,
    F: FnMut(&[u8]) -> Result<(), RpcError>,
{
    let mut input = [0_u8; 8192];
    let mut record = Vec::new();
    loop {
        let count = reader.read(&mut input).map_err(RpcError::Read)?;
        if count == 0 {
            if record.is_empty() {
                return Ok(());
            }
            return Err(RpcError::UnterminatedRecord);
        }
        let mut start = 0;
        for (index, byte) in input[..count].iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            extend_record(&mut record, &input[start..index])?;
            if record.last() == Some(&b'\r') {
                record.pop();
            }
            if record.is_empty() {
                return Err(RpcError::EmptyRecord);
            }
            on_record(&record)?;
            record.clear();
            start = index + 1;
        }
        extend_record(&mut record, &input[start..count])?;
    }
}

fn extend_record(record: &mut Vec<u8>, bytes: &[u8]) -> Result<(), RpcError> {
    if record.len().saturating_add(bytes.len()) > MAX_RPC_RECORD_BYTES {
        return Err(RpcError::RecordTooLarge(
            record.len().saturating_add(bytes.len()),
        ));
    }
    record.extend_from_slice(bytes);
    Ok(())
}

fn remove_pending(shared: &Shared, request_id: &str) -> Option<PendingSender> {
    shared
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(request_id)
}

fn fail_all_pending(shared: &Shared, message: &str) {
    let pending = std::mem::take(
        &mut *shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for sender in pending.into_values() {
        let _ = sender.send(Err(RpcError::Closed(message.to_owned())));
    }
}

fn broadcast(shared: &Shared, event: &PiEvent) {
    shared
        .subscribers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
}

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("failed to encode Pi RPC command: {0}")]
    Encode(serde_json::Error),
    #[error("failed to decode Pi RPC output: {0}")]
    Decode(serde_json::Error),
    #[error("failed to write Pi RPC input: {0}")]
    Write(io::Error),
    #[error("failed to read Pi RPC output: {0}")]
    Read(io::Error),
    #[error("failed to start Pi RPC dispatcher: {0}")]
    DispatcherStart(io::Error),
    #[error("Pi RPC record is {0} bytes, exceeding the limit")]
    RecordTooLarge(usize),
    #[error("Pi RPC command did not serialize to an object")]
    InvalidCommandShape,
    #[error("Pi RPC emitted an empty record")]
    EmptyRecord,
    #[error("Pi RPC stdout ended with an unterminated record")]
    UnterminatedRecord,
    #[error("Pi RPC record has no string type")]
    MissingType,
    #[error("Pi RPC response has no command")]
    MissingResponseCommand,
    #[error("Pi RPC response has no success flag")]
    MissingSuccess,
    #[error("Pi RPC response for {0} has no data")]
    MissingResponseData(&'static str),
    #[error("Pi RPC response has invalid data at {0}")]
    InvalidResponseData(&'static str),
    #[error("Pi RPC response command mismatch: expected {expected}, received {actual}")]
    CommandMismatch {
        expected: &'static str,
        actual: String,
    },
    #[error("Pi rejected {command}: {message}")]
    Rejected { command: String, message: String },
    #[error("timed out after {timeout:?} waiting for Pi {command}")]
    Timeout {
        command: &'static str,
        timeout: Duration,
    },
    #[error("Pi RPC connection closed: {0}")]
    Closed(String),
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::json;

    use super::{PiCommand, PiImage, RpcError, encode_jsonl, read_lf_records};

    #[test]
    fn splits_only_on_ascii_lf_and_preserves_unicode_separators() {
        let input = "{\"text\":\"before\u{2028}after\"}\r\n{\"text\":\"next\u{2029}value\"}\n";
        let mut records = Vec::new();
        read_lf_records(Cursor::new(input), |record| {
            records.push(String::from_utf8(record.to_vec()).expect("UTF-8 record"));
            Ok(())
        })
        .expect("read records");

        assert_eq!(records.len(), 2);
        assert!(records[0].contains('\u{2028}'));
        assert!(records[1].contains('\u{2029}'));
        assert!(!records[0].ends_with('\r'));
    }

    #[test]
    fn rejects_an_unterminated_final_record() {
        assert!(matches!(
            read_lf_records(Cursor::new(b"{}"), |_| Ok(())),
            Err(RpcError::UnterminatedRecord)
        ));
    }

    #[test]
    fn serialized_commands_end_in_exactly_one_lf() {
        let value = serde_json::to_value(PiCommand::GetState).expect("serialize command");
        let encoded = encode_jsonl(&value).expect("encode JSONL");
        assert_eq!(encoded.last(), Some(&b'\n'));
        assert_eq!(
            encoded.iter().position(|byte| *byte == b'\n'),
            Some(encoded.len() - 1)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encoded[..encoded.len() - 1])
                .expect("valid JSON"),
            json!({"type": "get_state"})
        );
    }

    #[test]
    fn images_serialize_as_pi_image_content_without_extra_fields() {
        let command = PiCommand::Prompt {
            message: "what is this".to_owned(),
            streaming_behavior: None,
            images: vec![PiImage::new("image/png", "aGk=")],
        };
        let value = serde_json::to_value(&command).expect("serialize prompt");
        assert_eq!(
            value,
            json!({
                "type": "prompt",
                "message": "what is this",
                "images": [{"type": "image", "data": "aGk=", "mimeType": "image/png"}]
            })
        );

        let bare = serde_json::to_value(PiCommand::Steer {
            message: "focus".to_owned(),
            images: Vec::new(),
        })
        .expect("serialize steer");
        assert_eq!(bare, json!({"type": "steer", "message": "focus"}));
    }
}
