use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use chrono::{DateTime, Utc};
use directories::BaseDirs;
use pix_wire::{HistoryAnchor, HistoryPresentation, TurnPresentationState};
use serde_json::Value;
use thiserror::Error;

use crate::image_assets::ImageAssetError;
use crate::pi_rpc::{RpcClient, RpcError};
use crate::session_history::{self, HistoryError, MessagePageBuilder};
use crate::session_history_reader::ReverseJsonlReader;
use crate::session_lock::SessionId;

const MAX_SESSION_LINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HISTORY_PREVIEW_BYTES: usize = 32 * 1024;
const HISTORY_CHECKPOINT_MESSAGE_STRIDE: usize = 256;
const HISTORY_CHECKPOINT_BYTE_STRIDE: u64 = 4 * 1024 * 1024;

#[derive(Default)]
struct SessionScan {
    header: Option<Value>,
    name: Option<String>,
    message_count: usize,
    first_user_message: Option<String>,
    last_activity: Option<DateTime<Utc>>,
    indexed_end_offset: u64,
    last_user: Option<HistoryMessageAnchor>,
    last_terminal_assistant: Option<HistoryMessageAnchor>,
    process_counts: HistoryProcessCounts,
    turn_was_aborted: bool,
    checkpoints: Vec<HistoryCheckpoint>,
    last_checkpoint_index: Option<usize>,
    last_checkpoint_offset: u64,
}

impl SessionScan {
    fn visit(&mut self, entry: Value, start_offset: u64, end_offset: u64) -> bool {
        if self.header.is_none() {
            if entry.get("type").and_then(Value::as_str) != Some("session") {
                return false;
            }
            self.header = Some(entry);
            self.indexed_end_offset = end_offset;
            return true;
        }
        match entry.get("type").and_then(Value::as_str) {
            Some("session_info") => {
                self.name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned);
            }
            Some("message") => self.visit_message(&entry, start_offset, end_offset),
            Some("error") => {
                self.process_counts.error_count = self.process_counts.error_count.saturating_add(1);
            }
            _ => {}
        }
        self.indexed_end_offset = end_offset;
        true
    }

    fn visit_message(&mut self, entry: &Value, start_offset: u64, end_offset: u64) {
        let index = self.message_count;
        self.message_count = self.message_count.saturating_add(1);
        let anchor = HistoryMessageAnchor {
            index,
            start_offset,
            end_offset,
        };
        if self.last_checkpoint_index.is_none()
            || index.saturating_sub(self.last_checkpoint_index.unwrap_or(index))
                >= HISTORY_CHECKPOINT_MESSAGE_STRIDE
            || end_offset.saturating_sub(self.last_checkpoint_offset)
                >= HISTORY_CHECKPOINT_BYTE_STRIDE
        {
            self.checkpoints.push(HistoryCheckpoint {
                index,
                offset: start_offset,
            });
            self.last_checkpoint_index = Some(index);
            self.last_checkpoint_offset = start_offset;
        }
        let message = entry.get("message").unwrap_or(&Value::Null);
        let role = message.get("role").and_then(Value::as_str);
        if matches!(role, Some("user" | "assistant")) {
            self.last_activity = message_timestamp(message)
                .or_else(|| entry_timestamp(entry))
                .or(self.last_activity);
        }
        if self.first_user_message.is_none() && role == Some("user") {
            self.first_user_message = extract_message_text(message);
        }
        match role {
            Some("user") => {
                self.last_user = Some(anchor);
                self.last_terminal_assistant = None;
                self.process_counts = HistoryProcessCounts::default();
                self.turn_was_aborted = false;
            }
            Some("assistant") => {
                if message_stop_reason(message).is_some_and(is_abort_reason) {
                    self.turn_was_aborted = true;
                }
                if self.last_user.is_some() && is_terminal_assistant(message) {
                    self.last_terminal_assistant = Some(anchor);
                }
                let counts = process_counts(message);
                self.process_counts.thought_count = self
                    .process_counts
                    .thought_count
                    .saturating_add(counts.thought_count);
                self.process_counts.tool_count = self
                    .process_counts
                    .tool_count
                    .saturating_add(counts.tool_count);
                self.process_counts.error_count = self
                    .process_counts
                    .error_count
                    .saturating_add(counts.error_count);
            }
            Some("tool" | "toolResult" | "tool_result") => {
                self.process_counts.tool_count = self.process_counts.tool_count.saturating_add(1);
            }
            _ => {}
        }
        if message.get("isError").and_then(Value::as_bool) == Some(true)
            || message.get("is_error").and_then(Value::as_bool) == Some(true)
        {
            self.process_counts.error_count = self.process_counts.error_count.saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub message_count: usize,
    pub first_user_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSession {
    pub summary: SessionSummary,
    pub path: PathBuf,
}

/// Byte anchor for one logical history message. The index is derived from
/// Pi's JSONL and contains offsets only; it never stores the message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryMessageAnchor {
    pub index: usize,
    pub start_offset: u64,
    pub end_offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryCheckpoint {
    pub index: usize,
    pub offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryProcessCounts {
    pub thought_count: u32,
    pub tool_count: u32,
    pub error_count: u32,
}

/// Derived, in-memory acceleration data for one native Pi session file.
///
/// Pi JSONL remains authoritative. This index may be dropped and rebuilt at
/// any time and intentionally contains no message content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHistoryIndex {
    pub file_size: u64,
    pub indexed_end_offset: u64,
    pub message_count: usize,
    pub history_epoch: String,
    pub last_user: Option<HistoryMessageAnchor>,
    pub last_terminal_assistant: Option<HistoryMessageAnchor>,
    pub process_counts: HistoryProcessCounts,
    pub turn_was_aborted: bool,
    pub checkpoints: Vec<HistoryCheckpoint>,
    pub boundary_fingerprint: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub session_name: Option<String>,
    pub model: Option<Value>,
    pub thinking_level: String,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub pending_message_count: usize,
    pub messages: Vec<Value>,
    /// Ephemeral tool rows captured by a TUI bridge snapshot. Native RPC
    /// snapshots leave this empty because Pi exposes tools through events.
    pub active_tools: Vec<Value>,
    /// The currently streamed assistant message, when a TUI snapshot catches
    /// the runtime between `message_update` and `message_end`.
    pub inflight_assistant: Option<Value>,
    /// TUI stream cursor included for snapshot-before-stream handoff. Native
    /// RPC snapshots have no compatible cursor and therefore use `None`.
    pub through_sequence: Option<u64>,
}

impl SessionSnapshot {
    /// Requests an authoritative snapshot from a live Pi runtime.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when RPC fails or Pi returns an incompatible
    /// state shape.
    pub fn read(rpc: &RpcClient, timeout: std::time::Duration) -> Result<Self, SessionError> {
        let snapshot = rpc.snapshot(timeout)?;
        Self::from_state_and_messages(&snapshot.state, snapshot.messages)
    }

    /// Reads only Pi's runtime state. History-capable clients read their
    /// bounded message page from the native JSONL session file instead of
    /// asking Pi to serialize the complete in-memory history.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when Pi does not return a valid runtime state.
    pub fn read_state(rpc: &RpcClient, timeout: std::time::Duration) -> Result<Self, SessionError> {
        let state = rpc.state(timeout)?;
        Self::from_state_and_messages(&state, Vec::new())
    }

    fn from_state_and_messages(state: &Value, messages: Vec<Value>) -> Result<Self, SessionError> {
        Ok(Self {
            session_id: required_string(state, "sessionId")?.to_owned(),
            session_name: state
                .get("sessionName")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            model: state.get("model").filter(|value| !value.is_null()).cloned(),
            thinking_level: required_string(state, "thinkingLevel")?.to_owned(),
            is_streaming: required_bool(state, "isStreaming")?,
            is_compacting: required_bool(state, "isCompacting")?,
            pending_message_count: required_usize(state, "pendingMessageCount")?,
            messages,
            active_tools: Vec::new(),
            inflight_assistant: None,
            through_sequence: None,
        })
    }
}

/// Payload-free timings for one `session.list` scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionListTiming {
    pub enumerate_ms: u64,
    pub scan_ms: u64,
    pub file_count: u64,
    pub session_count: u64,
    pub parsed_count: u64,
    pub reused_count: u64,
}

/// Process-memory index of session JSONL summaries.
///
/// Entries are keyed by file path and reused when mtime and size are
/// unchanged. The index is never written to disk and is not a conversation
/// source of truth.
#[derive(Debug, Default)]
pub struct SessionMetadataIndex {
    files: HashMap<PathBuf, CachedSessionFile>,
    by_id: HashMap<SessionId, PathBuf>,
}

#[derive(Debug, Clone)]
struct CachedSessionFile {
    modified: SystemTime,
    size: u64,
    session: Option<DiscoveredSession>,
    history: Option<SessionHistoryIndex>,
}

#[derive(Debug, Clone)]
pub struct PiSessionStore {
    session_directory: PathBuf,
    workspace: PathBuf,
}

impl PiSessionStore {
    /// Locates Pi's configured native session directory for a workspace.
    ///
    /// Honors `PI_CODING_AGENT_SESSION_DIR`, then `PI_CODING_AGENT_DIR`, and
    /// otherwise uses Pi's `~/.pi/agent/sessions/--encoded-cwd--` convention.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if the workspace cannot be canonicalized or no
    /// user home directory is available.
    pub fn for_workspace(workspace: impl AsRef<Path>) -> Result<Self, SessionError> {
        let canonical_workspace =
            fs::canonicalize(workspace.as_ref()).map_err(|source| SessionError::Canonicalize {
                path: workspace.as_ref().to_path_buf(),
                source,
            })?;
        let session_directory = if let Some(directory) =
            std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
        {
            PathBuf::from(directory)
        } else {
            let agent_directory = if let Some(directory) = std::env::var_os("PI_CODING_AGENT_DIR") {
                PathBuf::from(directory)
            } else {
                BaseDirs::new()
                    .map(|directories| directories.home_dir().join(".pi/agent"))
                    .ok_or(SessionError::NoHomeDirectory)?
            };
            agent_directory
                .join("sessions")
                .join(encoded_workspace_directory(&canonical_workspace))
        };
        Self::new(session_directory, canonical_workspace)
    }

    /// Creates a session view scoped to one canonical authorized workspace.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if the workspace cannot be canonicalized or is
    /// not a directory.
    pub fn new(
        session_directory: impl Into<PathBuf>,
        workspace: impl AsRef<Path>,
    ) -> Result<Self, SessionError> {
        let workspace =
            fs::canonicalize(workspace.as_ref()).map_err(|source| SessionError::Canonicalize {
                path: workspace.as_ref().to_path_buf(),
                source,
            })?;
        if !workspace.is_dir() {
            return Err(SessionError::WorkspaceNotDirectory(workspace));
        }
        Ok(Self {
            session_directory: session_directory.into(),
            workspace,
        })
    }

    #[must_use]
    pub fn session_directory(&self) -> &Path {
        &self.session_directory
    }

    /// Lists valid native Pi sessions for this workspace, newest first.
    ///
    /// Unreadable or malformed files are ignored so one damaged historical
    /// session does not hide healthy sessions.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] only when the session directory itself cannot
    /// be read. A missing directory represents an empty history.
    pub fn list(&self) -> Result<Vec<DiscoveredSession>, SessionError> {
        Ok(self.list_timed()?.0)
    }

    /// Lists sessions and reports directory-enumeration vs JSONL-scan cost.
    ///
    /// # Errors
    ///
    /// Same as [`Self::list`].
    pub fn list_timed(&self) -> Result<(Vec<DiscoveredSession>, SessionListTiming), SessionError> {
        let mut index = SessionMetadataIndex::default();
        self.list_cached(&mut index, None)
    }

    /// Lists sessions using `index` so unchanged JSONL files are not reparsed.
    ///
    /// # Errors
    ///
    /// Same as [`Self::list`].
    pub fn list_cached(
        &self,
        index: &mut SessionMetadataIndex,
        limit: Option<usize>,
    ) -> Result<(Vec<DiscoveredSession>, SessionListTiming), SessionError> {
        index.list(&self.session_directory, &self.workspace, limit)
    }

    /// Finds a session by native Pi session ID.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if discovery fails or the session is absent.
    pub fn find(&self, id: SessionId) -> Result<DiscoveredSession, SessionError> {
        let mut index = SessionMetadataIndex::default();
        self.find_cached(&mut index, id)
    }

    /// Finds a session using a caller-owned `HostCatalog` index. This avoids a
    /// second directory scan when Home already discovered the session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the catalog cannot read the session
    /// directory or the requested session is not authorized/found.
    pub fn find_cached(
        &self,
        index: &mut SessionMetadataIndex,
        id: SessionId,
    ) -> Result<DiscoveredSession, SessionError> {
        index.find_cached(&self.session_directory, &self.workspace, id)
    }

    /// Reads one bounded history page directly from the authoritative Pi
    /// JSONL file. Only the candidate page is retained; the full message list
    /// never crosses the Pi RPC or Pix wire boundaries.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the native session is unavailable,
    /// malformed, or contains an invalid cursor or oversized message.
    pub fn history_page(
        &self,
        id: SessionId,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<pix_wire::SessionHistoryPage, SessionError> {
        self.history_page_with_transform(id, cursor, limit, Ok)
    }

    /// Reads one history page while transforming each message before it is
    /// admitted to the byte budget. Hosts use this for image externalization:
    /// a large inline image must become a small reference before the selector
    /// decides whether the page fits the wire frame.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the native session cannot be read, the
    /// transform fails, or the cursor/page violates the history limits.
    pub fn history_page_with_transform<F>(
        &self,
        id: SessionId,
        cursor: Option<&str>,
        limit: u32,
        transform: F,
    ) -> Result<pix_wire::SessionHistoryPage, SessionError>
    where
        F: FnMut(Value) -> Result<Value, SessionError>,
    {
        let mut index = SessionMetadataIndex::default();
        self.history_page_with_transform_cached_internal(
            &mut index, id, cursor, limit, transform, false, false,
        )
    }

    /// Indexed history path used by Host connections. The index supplies the
    /// fixed message count, committed fence, anchors, and epoch so the reader
    /// can seek from the tail instead of reparsing the complete file.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session, cursor, source file, or
    /// transformed page is invalid.
    pub fn history_page_with_transform_cached<F>(
        &self,
        index: &mut SessionMetadataIndex,
        id: SessionId,
        cursor: Option<&str>,
        limit: u32,
        transform: F,
    ) -> Result<pix_wire::SessionHistoryPage, SessionError>
    where
        F: FnMut(Value) -> Result<Value, SessionError>,
    {
        self.history_page_with_transform_cached_options(index, id, cursor, limit, false, transform)
    }

    /// Indexed history path with structured page representations. When
    /// `include_items` is true, oversized messages become bounded placeholders
    /// instead of failing the entire page.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session, cursor, source file, or
    /// transformed page is invalid.
    pub fn history_page_with_transform_cached_options<F>(
        &self,
        index: &mut SessionMetadataIndex,
        id: SessionId,
        cursor: Option<&str>,
        limit: u32,
        include_items: bool,
        transform: F,
    ) -> Result<pix_wire::SessionHistoryPage, SessionError>
    where
        F: FnMut(Value) -> Result<Value, SessionError>,
    {
        self.history_page_with_transform_cached_internal(
            index,
            id,
            cursor,
            limit,
            transform,
            true,
            include_items,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn history_page_with_transform_cached_internal<F>(
        &self,
        index: &mut SessionMetadataIndex,
        id: SessionId,
        cursor: Option<&str>,
        limit: u32,
        mut transform: F,
        strict_epoch: bool,
        include_items: bool,
    ) -> Result<pix_wire::SessionHistoryPage, SessionError>
    where
        F: FnMut(Value) -> Result<Value, SessionError>,
    {
        let discovered = self.find_cached(index, id)?;
        let history = index
            .history_for(&discovered.path)
            .ok_or(SessionError::NotFound(id))?;
        Self::history_page_from_index(
            &discovered,
            &history,
            cursor,
            limit,
            &mut transform,
            strict_epoch,
            include_items,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn history_page_from_index<F>(
        discovered: &DiscoveredSession,
        history: &SessionHistoryIndex,
        cursor: Option<&str>,
        limit: u32,
        transform: &mut F,
        strict_epoch: bool,
        include_items: bool,
    ) -> Result<pix_wire::SessionHistoryPage, SessionError>
    where
        F: FnMut(Value) -> Result<Value, SessionError>,
    {
        let session_key = discovered.summary.id.to_string();
        let (before_index, revision, upper_bound, snapshot_end_offset, snapshot_fingerprint) =
            match cursor {
                Some(value) => match session_history::indexed_cursor(&session_key, value) {
                    Ok(cursor) => {
                        if (strict_epoch && cursor.history_epoch != history.history_epoch)
                            || cursor.snapshot_end_offset > history.indexed_end_offset
                            || cursor.before_offset > cursor.snapshot_end_offset
                            || cursor.before_index > cursor.revision
                            || cursor.revision > history.message_count
                            || file_boundary_fingerprint(
                                &discovered.path,
                                cursor.snapshot_end_offset,
                            )? != cursor.boundary_fingerprint
                        {
                            return Err(SessionError::History(HistoryError::InvalidCursor));
                        }
                        (
                            Some(cursor.before_index),
                            cursor.revision,
                            cursor.before_offset,
                            cursor.snapshot_end_offset,
                            cursor.boundary_fingerprint,
                        )
                    }
                    Err(HistoryError::LegacyCursor) => {
                        return Self::history_page_forward_legacy(
                            discovered, value, limit, transform,
                        );
                    }
                    Err(error) => return Err(SessionError::History(error)),
                },
                None => (
                    None,
                    history.message_count,
                    history.indexed_end_offset,
                    history.indexed_end_offset,
                    history.boundary_fingerprint,
                ),
            };
        let mut selected = Vec::new();
        let mut current_index = before_index.unwrap_or(revision);
        let target = pix_wire::MAX_HISTORY_PAGE_BYTES.saturating_sub(8192);
        let mut encoded_bytes = 0_usize;
        let file = File::open(&discovered.path).map_err(|source| SessionError::ReadFile {
            path: discovered.path.clone(),
            source,
        })?;
        let mut reader = ReverseJsonlReader::new(file, upper_bound, MAX_SESSION_LINE_BYTES);
        while selected.len() < usize::try_from(limit).unwrap_or(usize::MAX) && current_index > 0 {
            let Some(record) = reader
                .next_record()
                .map_err(|source| SessionError::ReadFile {
                    path: discovered.path.clone(),
                    source,
                })?
            else {
                break;
            };
            let mut bytes = record.bytes;
            trim_record_ending(&mut bytes);
            let Ok(entry) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if entry.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            current_index = current_index.saturating_sub(1);
            let message = entry.get("message").cloned().unwrap_or(Value::Null);
            let transformed = transform(message.clone())?;
            let serialized = serde_json::to_vec(&transformed)
                .map_err(|error| SessionError::History(HistoryError::Encode(error)))?;
            let separator = usize::from(!selected.is_empty());
            let mut representation = if include_items && is_renderable_history_message(&transformed)
            {
                Some(pix_wire::HistoryPageItem::Message {
                    index: current_index,
                    message: transformed.clone(),
                })
            } else {
                None
            };
            let mut candidate_bytes = serialized.len();
            if !is_renderable_history_message(&transformed)
                || serialized.len().saturating_add(8192) > pix_wire::MAX_HISTORY_PAGE_BYTES
                || serialized.len().saturating_add(8192) > pix_wire::MAX_ENCRYPTED_FRAME_BYTES
            {
                if !include_items {
                    return Err(SessionError::History(HistoryError::MessageTooLarge(
                        serialized.len(),
                    )));
                }
                let preview = message_preview(&transformed);
                let placeholder = pix_wire::HistoryPageItem::Placeholder {
                    index: current_index,
                    role: transformed
                        .get("role")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    preview: preview.0,
                    original_bytes: serialized.len(),
                    truncated: preview.1,
                    content_ref: None,
                };
                candidate_bytes = serde_json::to_vec(&placeholder)
                    .map_err(HistoryError::Encode)?
                    .len();
                representation = Some(placeholder);
            }
            if !selected.is_empty()
                && encoded_bytes
                    .saturating_add(separator)
                    .saturating_add(candidate_bytes)
                    > target
            {
                break;
            }
            encoded_bytes = encoded_bytes
                .saturating_add(separator)
                .saturating_add(candidate_bytes);
            selected.push((
                current_index,
                transformed,
                record.start_offset,
                representation,
            ));
        }
        selected.reverse();
        loop {
            let start_index = selected
                .first()
                .map_or(current_index, |(index, _, _, _)| *index);
            let has_more = start_index > 0;
            let next_cursor = has_more.then(|| {
                session_history::encode_indexed_cursor(
                    &session_key,
                    start_index,
                    revision,
                    &history.history_epoch,
                    snapshot_end_offset,
                    selected
                        .first()
                        .map_or(upper_bound, |(_, _, offset, _)| *offset),
                    snapshot_fingerprint,
                )
            });
            let page = pix_wire::SessionHistoryPage {
                session_id: session_key.clone(),
                messages: if include_items {
                    Vec::new()
                } else {
                    selected
                        .iter()
                        .map(|(_, message, _, _)| message.clone())
                        .collect()
                },
                start_index,
                has_more,
                next_cursor,
                revision: u64::try_from(revision).unwrap_or(u64::MAX),
                history_items: if include_items {
                    selected
                        .iter()
                        .filter_map(|(_, _, _, item)| item.clone())
                        .collect()
                } else {
                    Vec::new()
                },
            };
            let encoded_size = serde_json::to_vec(&page)
                .map_err(HistoryError::Encode)?
                .len();
            if encoded_size <= pix_wire::MAX_HISTORY_PAGE_BYTES {
                crate::diagnostics::record(
                    "session.history.read",
                    &[
                        ("bytes_read", reader.bytes_read()),
                        ("records_read", reader.records_read()),
                        (
                            "message_count",
                            u64::try_from(selected.len()).unwrap_or(u64::MAX),
                        ),
                        ("indexed", 1),
                    ],
                );
                return Ok(page);
            }
            if selected.len() <= 1 {
                return Err(SessionError::History(HistoryError::PageTooLarge(
                    encoded_size,
                )));
            }
            // Drop the oldest selected representation. Its index remains the
            // first item of the next page because the cursor is rebuilt from
            // the new first item's line-start offset on the next iteration.
            selected.remove(0);
        }
    }

    fn history_page_forward_legacy<F>(
        discovered: &DiscoveredSession,
        cursor: &str,
        limit: u32,
        transform: &mut F,
    ) -> Result<pix_wire::SessionHistoryPage, SessionError>
    where
        F: FnMut(Value) -> Result<Value, SessionError>,
    {
        let session_key = discovered.summary.id.to_string();
        let before_index = session_history::before_index(&session_key, cursor)?;
        let revision = session_history::cursor_revision(&session_key, cursor)?;
        let candidate_floor =
            before_index.saturating_sub(usize::try_from(limit).unwrap_or(usize::MAX));
        let mut builder =
            MessagePageBuilder::with_revision(Some(before_index), limit, Some(revision))?;
        let file = File::open(&discovered.path).map_err(|source| SessionError::ReadFile {
            path: discovered.path.clone(),
            source,
        })?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut message_index = 0_usize;
        loop {
            line.clear();
            let count = read_bounded_record(&mut reader, &mut line).map_err(|source| {
                SessionError::ReadFile {
                    path: discovered.path.clone(),
                    source,
                }
            })?;
            if count == 0 {
                break;
            }
            if line.len() > MAX_SESSION_LINE_BYTES
                || (line.len() == MAX_SESSION_LINE_BYTES && line.last() != Some(&b'\n'))
            {
                return Err(SessionError::ReadFile {
                    path: discovered.path.clone(),
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Pi session entry exceeds the supported line limit",
                    ),
                });
            }
            trim_record_ending(&mut line);
            let Ok(entry) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if entry.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let index = message_index;
            message_index = message_index.saturating_add(1);
            if index >= revision {
                break;
            }
            if index < candidate_floor || index >= before_index {
                continue;
            }
            let message = entry.get("message").cloned().unwrap_or(Value::Null);
            builder.push(index, transform(message)?)?;
        }
        let mut page = builder.finish(&session_key)?;
        page.history_items = Vec::new();
        Ok(page)
    }

    /// Builds the small semantic tail envelope from the derived index and
    /// canonical JSONL anchors. Message bodies are read only for bounded
    /// previews and are never retained in the index.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session cannot be found or an anchor
    /// preview cannot be read safely.
    pub fn history_presentation_cached(
        &self,
        index: &mut SessionMetadataIndex,
        id: SessionId,
        state: pix_wire::SessionState,
    ) -> Result<pix_wire::HistoryPresentation, SessionError> {
        let discovered = self.find_cached(index, id)?;
        let history = index
            .history_for(&discovered.path)
            .ok_or(SessionError::NotFound(id))?;
        let user_anchor = history.last_user.map(|anchor| HistoryAnchor {
            source_index: anchor.index,
            preview: read_anchor_preview(&discovered.path, anchor).ok().flatten(),
        });
        let terminal_anchor = history.last_terminal_assistant.map(|anchor| HistoryAnchor {
            source_index: anchor.index,
            preview: read_anchor_preview(&discovered.path, anchor).ok().flatten(),
        });
        let turn_state = match state {
            pix_wire::SessionState::Compacting => TurnPresentationState::Compacted,
            pix_wire::SessionState::Running | pix_wire::SessionState::Starting => {
                TurnPresentationState::Active
            }
            pix_wire::SessionState::Unavailable => TurnPresentationState::Failed,
            pix_wire::SessionState::Sleeping | pix_wire::SessionState::Idle => {
                if history.turn_was_aborted {
                    TurnPresentationState::Aborted
                } else if terminal_anchor.is_some() {
                    TurnPresentationState::Completed
                } else if history.process_counts.error_count > 0 {
                    TurnPresentationState::Failed
                } else if user_anchor.is_some() {
                    TurnPresentationState::Active
                } else {
                    TurnPresentationState::Completed
                }
            }
        };
        Ok(HistoryPresentation {
            turn_state,
            user_anchor,
            terminal_anchor,
            process: pix_wire::HistoryProcessSummary {
                thought_count: history.process_counts.thought_count,
                tool_count: history.process_counts.tool_count,
                error_count: history.process_counts.error_count,
                omitted: history.process_counts.thought_count > 0
                    || history.process_counts.tool_count > 0
                    || history.process_counts.error_count > 0,
            },
            error_summary: None,
        })
    }
}

impl SessionMetadataIndex {
    /// Drops cached summaries under `directory` so the next list reparses.
    pub fn invalidate_directory(&mut self, directory: &Path) {
        let paths = self
            .files
            .keys()
            .filter(|path| path.parent() == Some(directory))
            .cloned()
            .collect::<Vec<_>>();
        for path in paths {
            self.remove_path(&path);
        }
    }

    fn remove_path(&mut self, path: &Path) {
        if let Some(cached) = self.files.remove(path)
            && let Some(session) = cached.session
        {
            self.by_id.remove(&session.summary.id);
        }
    }

    fn find_cached(
        &mut self,
        directory: &Path,
        workspace: &Path,
        id: SessionId,
    ) -> Result<DiscoveredSession, SessionError> {
        if let Some(path) = self.by_id.get(&id).cloned()
            && path.parent() == Some(directory)
        {
            match self.lookup_or_parse(&path, workspace)? {
                CacheHit::Reused(Some(session)) | CacheHit::Parsed(Some(session)) => {
                    return Ok(session);
                }
                CacheHit::Reused(None) | CacheHit::Parsed(None) => {}
            }
        }
        let (sessions, _) = self.list(directory, workspace, None)?;
        sessions
            .into_iter()
            .find(|session| session.summary.id == id)
            .ok_or(SessionError::NotFound(id))
    }

    fn history_for(&self, path: &Path) -> Option<SessionHistoryIndex> {
        self.files
            .get(path)
            .and_then(|cached| cached.history.clone())
    }

    fn list(
        &mut self,
        directory: &Path,
        workspace: &Path,
        limit: Option<usize>,
    ) -> Result<(Vec<DiscoveredSession>, SessionListTiming), SessionError> {
        let enumerate_started = Instant::now();
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.invalidate_directory(directory);
                return Ok((Vec::new(), empty_timing(enumerate_started)));
            }
            Err(source) => {
                return Err(SessionError::ReadDirectory {
                    path: directory.to_path_buf(),
                    source,
                });
            }
        };
        let mut jsonl_paths = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                jsonl_paths.push(path);
            }
        }
        let enumerate_ms = crate::diagnostics::elapsed_ms(enumerate_started);

        let scan_started = Instant::now();
        let mut sessions = Vec::new();
        let mut parsed_count = 0_u64;
        let mut reused_count = 0_u64;
        let live: HashSet<PathBuf> = jsonl_paths.iter().cloned().collect();
        for path in &jsonl_paths {
            match self.lookup_or_parse(path, workspace)? {
                CacheHit::Reused(session) => {
                    reused_count = reused_count.saturating_add(1);
                    if let Some(session) = session {
                        sessions.push(session);
                    }
                }
                CacheHit::Parsed(session) => {
                    parsed_count = parsed_count.saturating_add(1);
                    if let Some(session) = session {
                        sessions.push(session);
                    }
                }
            }
        }
        let stale = self
            .files
            .keys()
            .filter(|path| path.parent() == Some(directory) && !live.contains(*path))
            .cloned()
            .collect::<Vec<_>>();
        for path in stale {
            self.remove_path(&path);
        }
        sessions.sort_by_key(|session| Reverse(session.summary.modified_at));
        if let Some(limit) = limit {
            sessions.truncate(limit);
        }
        let timing = SessionListTiming {
            enumerate_ms,
            scan_ms: crate::diagnostics::elapsed_ms(scan_started),
            file_count: u64::try_from(jsonl_paths.len()).unwrap_or(u64::MAX),
            session_count: u64::try_from(sessions.len()).unwrap_or(u64::MAX),
            parsed_count,
            reused_count,
        };
        Ok((sessions, timing))
    }

    fn lookup_or_parse(&mut self, path: &Path, workspace: &Path) -> Result<CacheHit, SessionError> {
        let Ok(metadata) = fs::metadata(path) else {
            self.remove_path(path);
            return Ok(CacheHit::Parsed(None));
        };
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let size = metadata.len();
        if let Some(cached) = self.files.get(path)
            && cached.modified == modified
            && cached.size == size
        {
            return Ok(CacheHit::Reused(cached.session.clone()));
        }
        let previous = self.files.get(path).cloned();
        let parsed = parse_session_file(path, workspace)?;
        self.remove_path(path);
        let (session, mut history) = match parsed {
            Some(parsed) => (Some(parsed.session), Some(parsed.history)),
            None => (None, None),
        };
        if let (Some(previous), Some(history)) = (previous.as_ref(), history.as_mut())
            && size >= previous.size
            && previous.history.as_ref().is_some_and(|old| {
                file_boundary_fingerprint(path, old.indexed_end_offset)
                    .ok()
                    .is_some_and(|fingerprint| fingerprint == old.boundary_fingerprint)
            })
            && let Some(old) = previous.history.as_ref()
        {
            history.history_epoch.clone_from(&old.history_epoch);
        }
        self.files.insert(
            path.to_path_buf(),
            CachedSessionFile {
                modified,
                size,
                session: session.clone(),
                history,
            },
        );
        if let Some(session) = session.as_ref() {
            self.by_id.insert(session.summary.id, path.to_path_buf());
        }
        Ok(CacheHit::Parsed(session))
    }
}

fn empty_timing(enumerate_started: Instant) -> SessionListTiming {
    SessionListTiming {
        enumerate_ms: crate::diagnostics::elapsed_ms(enumerate_started),
        scan_ms: 0,
        file_count: 0,
        session_count: 0,
        parsed_count: 0,
        reused_count: 0,
    }
}

enum CacheHit {
    Reused(Option<DiscoveredSession>),
    Parsed(Option<DiscoveredSession>),
}

struct ParsedSession {
    session: DiscoveredSession,
    history: SessionHistoryIndex,
}

fn parse_session_file(
    path: &Path,
    canonical_workspace: &Path,
) -> Result<Option<ParsedSession>, SessionError> {
    let file = File::open(path).map_err(|source| SessionError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| SessionError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut scan = SessionScan::default();
    let mut offset = 0_u64;

    loop {
        line.clear();
        let count = read_bounded_record(&mut reader, &mut line).map_err(|source| {
            SessionError::ReadFile {
                path: path.to_path_buf(),
                source,
            }
        })?;
        if count == 0 {
            break;
        }
        let start_offset = offset;
        offset = offset.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        let end_offset = offset;
        if line.len() > MAX_SESSION_LINE_BYTES
            || (line.len() == MAX_SESSION_LINE_BYTES && line.last() != Some(&b'\n'))
        {
            return Ok(None);
        }
        trim_record_ending(&mut line);
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if !scan.visit(entry, start_offset, end_offset) {
            return Ok(None);
        }
    }

    let Some(header) = scan.header else {
        return Ok(None);
    };
    let Some(id) = header
        .get("id")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(SessionId::from_uuid)
    else {
        return Ok(None);
    };
    let Some(cwd) = header.get("cwd").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Ok(canonical_cwd) = fs::canonicalize(cwd) else {
        return Ok(None);
    };
    if canonical_cwd != canonical_workspace {
        return Ok(None);
    }
    let created_at = header
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .or_else(|| metadata.created().ok().map(system_time_to_utc))
        .unwrap_or_else(Utc::now);
    let modified_at = scan
        .last_activity
        .or_else(|| metadata.modified().ok().map(system_time_to_utc))
        .unwrap_or(created_at);
    let session = DiscoveredSession {
        summary: SessionSummary {
            id,
            name: scan.name,
            created_at,
            modified_at,
            message_count: scan.message_count,
            first_user_message: scan.first_user_message,
        },
        path: path.to_path_buf(),
    };
    let history = SessionHistoryIndex {
        file_size: metadata.len(),
        indexed_end_offset: scan.indexed_end_offset,
        message_count: scan.message_count,
        history_epoch: new_history_epoch(),
        last_user: scan.last_user,
        last_terminal_assistant: scan.last_terminal_assistant,
        process_counts: scan.process_counts,
        turn_was_aborted: scan.turn_was_aborted,
        checkpoints: scan.checkpoints,
        boundary_fingerprint: file_boundary_fingerprint(path, scan.indexed_end_offset)?,
    };
    Ok(Some(ParsedSession { session, history }))
}

fn read_bounded_record<R: BufRead>(reader: &mut R, output: &mut Vec<u8>) -> io::Result<usize> {
    let limit = u64::try_from(MAX_SESSION_LINE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut limited = std::io::Read::take(&mut *reader, limit);
    limited.read_until(b'\n', output)
}

fn trim_record_ending(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

fn extract_message_text(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return nonempty(text);
    }
    let text = content
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    nonempty(&text)
}

fn visit_process_value(value: &Value, counts: &mut HistoryProcessCounts) {
    match value {
        Value::Array(values) => values
            .iter()
            .for_each(|value| visit_process_value(value, counts)),
        Value::Object(object) => {
            if let Some(kind) = object.get("type").and_then(Value::as_str) {
                let normalized = kind.to_ascii_lowercase();
                if normalized.contains("think") || normalized.contains("reason") {
                    counts.thought_count = counts.thought_count.saturating_add(1);
                }
                if normalized.contains("toolcall") || normalized.contains("tool_call") {
                    counts.tool_count = counts.tool_count.saturating_add(1);
                }
            }
            if message_stop_reason(value).is_some_and(|reason| reason.eq_ignore_ascii_case("error"))
            {
                counts.error_count = counts.error_count.saturating_add(1);
            }
            if value.get("isError").and_then(Value::as_bool) == Some(true)
                || value.get("is_error").and_then(Value::as_bool) == Some(true)
            {
                counts.error_count = counts.error_count.saturating_add(1);
            }
            object
                .values()
                .for_each(|value| visit_process_value(value, counts));
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn process_counts(message: &Value) -> HistoryProcessCounts {
    let mut counts = HistoryProcessCounts::default();
    if matches!(
        message.get("role").and_then(Value::as_str),
        Some("tool" | "toolResult" | "tool_result")
    ) {
        counts.tool_count = 1;
    }
    visit_process_value(message, &mut counts);
    counts
}

fn contains_tool_call(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_tool_call),
        Value::Object(object) => {
            object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    let normalized = kind.to_ascii_lowercase();
                    normalized.contains("toolcall") || normalized.contains("tool_call")
                })
                || object.values().any(contains_tool_call)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn is_terminal_assistant(message: &Value) -> bool {
    if message
        .get("stopReason")
        .or_else(|| message.get("stop_reason"))
        .and_then(Value::as_str)
        .is_some_and(|reason| reason.eq_ignore_ascii_case("error"))
        || message
            .get("isError")
            .or_else(|| message.get("is_error"))
            .and_then(Value::as_bool)
            == Some(true)
    {
        return false;
    }
    let Some(content) = message.get("content") else {
        return true;
    };
    !contains_tool_call(content)
}

fn message_stop_reason(message: &Value) -> Option<&str> {
    message
        .get("stopReason")
        .or_else(|| message.get("stop_reason"))
        .and_then(Value::as_str)
}

fn is_abort_reason(reason: &str) -> bool {
    matches!(
        reason.to_ascii_lowercase().as_str(),
        "abort" | "aborted" | "cancel" | "cancelled" | "canceled"
    )
}

fn message_preview(message: &Value) -> (String, bool) {
    let text = extract_message_text(message)
        .or_else(|| {
            message
                .get("role")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "Large history message".to_owned());
    if text.len() <= MAX_HISTORY_PREVIEW_BYTES {
        return (text, false);
    }
    let mut end = MAX_HISTORY_PREVIEW_BYTES.saturating_sub("…".len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &text[..end]), true)
}

/// Returns whether the iOS snapshot builder has a stable row representation
/// for a canonical Pi message. Unknown roles are retained as placeholders so
/// an unsupported record cannot silently create a source-index gap.
fn is_renderable_history_message(message: &Value) -> bool {
    matches!(
        message.get("role").and_then(Value::as_str),
        Some("user" | "assistant" | "toolResult" | "tool_result" | "bashExecution" | "custom")
    )
}

fn read_anchor_preview(
    path: &Path,
    anchor: HistoryMessageAnchor,
) -> Result<Option<pix_wire::HistoryPreview>, SessionError> {
    let mut file = File::open(path).map_err(|source| SessionError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let length = anchor.end_offset.saturating_sub(anchor.start_offset);
    let length = usize::try_from(length).map_err(|_| SessionError::ReadFile {
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::InvalidData,
            "history anchor range is too large",
        ),
    })?;
    let mut bytes = vec![0_u8; length];
    file.seek(std::io::SeekFrom::Start(anchor.start_offset))
        .map_err(|source| SessionError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    file.read_exact(&mut bytes)
        .map_err(|source| SessionError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    trim_record_ending(&mut bytes);
    let entry =
        serde_json::from_slice::<Value>(&bytes).map_err(|error| SessionError::ReadFile {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
        })?;
    let message = entry.get("message").unwrap_or(&Value::Null);
    let Some(role) = message.get("role").and_then(Value::as_str) else {
        return Ok(None);
    };
    let serialized = serde_json::to_vec(message).map_err(HistoryError::Encode)?;
    let (text, truncated) = message_preview(message);
    Ok(Some(pix_wire::HistoryPreview {
        role: role.to_owned(),
        text,
        original_bytes: serialized.len(),
        truncated,
    }))
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn message_timestamp(message: &Value) -> Option<DateTime<Utc>> {
    let milliseconds = message.get("timestamp")?.as_i64()?;
    DateTime::from_timestamp_millis(milliseconds)
}

fn entry_timestamp(entry: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(entry.get("timestamp")?.as_str()?)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn system_time_to_utc(value: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(value)
}

fn new_history_epoch() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Computes a small prefix/end fingerprint for the committed boundary. It is
/// deliberately bounded: it catches the supported rewrite/truncate patterns
/// without hashing a complete multi-megabyte session file.
fn file_boundary_fingerprint(path: &Path, end_offset: u64) -> Result<u64, SessionError> {
    let mut file = File::open(path).map_err(|source| SessionError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    let prefix_len = end_offset.min(128);
    if prefix_len > 0 {
        let mut prefix = vec![0_u8; usize::try_from(prefix_len).unwrap_or(128)];
        file.read_exact(&mut prefix)
            .map_err(|source| SessionError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        bytes.extend_from_slice(&prefix);
    }
    let tail_start = end_offset.saturating_sub(128);
    if tail_start >= prefix_len {
        file.seek(std::io::SeekFrom::Start(tail_start))
            .map_err(|source| SessionError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        let tail_len = end_offset.saturating_sub(tail_start);
        let mut tail = vec![0_u8; usize::try_from(tail_len).unwrap_or(128)];
        file.read_exact(&mut tail)
            .map_err(|source| SessionError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        bytes.extend_from_slice(&tail);
    }
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    Ok(hash)
}

fn encoded_workspace_directory(workspace: &Path) -> String {
    let path = workspace.to_string_lossy();
    let without_root = path.trim_start_matches(['/', '\\']);
    let encoded: String = without_root
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    format!("--{encoded}--")
}

fn required_string<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, SessionError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(SessionError::InvalidSnapshot(field))
}

fn required_bool(value: &Value, field: &'static str) -> Result<bool, SessionError> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or(SessionError::InvalidSnapshot(field))
}

fn required_usize(value: &Value, field: &'static str) -> Result<usize, SessionError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|number| usize::try_from(number).ok())
        .ok_or(SessionError::InvalidSnapshot(field))
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("no user home directory is available for Pi session discovery")]
    NoHomeDirectory,
    #[error("failed to canonicalize workspace {path}: {source}")]
    Canonicalize { path: PathBuf, source: io::Error },
    #[error("workspace is not a directory: {0}")]
    WorkspaceNotDirectory(PathBuf),
    #[error("failed to read Pi session directory {path}: {source}")]
    ReadDirectory { path: PathBuf, source: io::Error },
    #[error("failed to read Pi session file {path}: {source}")]
    ReadFile { path: PathBuf, source: io::Error },
    #[error("Pi session was not found: {0}")]
    NotFound(SessionId),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    History(#[from] crate::session_history::HistoryError),
    #[error(transparent)]
    ImageAsset(#[from] ImageAssetError),
    #[error("Pi snapshot field {0} is absent or incompatible")]
    InvalidSnapshot(&'static str),
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Write;

    use serde_json::Value;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{PiSessionStore, SessionError};
    use crate::session_lock::SessionId;
    use pix_wire::{HistoryPageItem, SessionState, TurnPresentationState};

    #[test]
    fn lists_only_sessions_for_the_authorized_workspace() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let other = directory.path().join("other");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&other).expect("create other workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let header = format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{}}}\n",
            serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
                .expect("encode path")
        );
        fs::write(
            sessions.join("good.jsonl"),
            format!(
                "{header}{{\"type\":\"session_info\",\"name\":\"Feature\"}}\n{{\"type\":\"message\",\"timestamp\":\"2026-08-12T00:00:01Z\",\"message\":{{\"role\":\"user\",\"content\":\"Hello\"}}}}\n"
            ),
        )
        .expect("write good session");
        fs::write(
            sessions.join("other.jsonl"),
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{}}}\n",
                Uuid::new_v4(),
                serde_json::to_string(other.to_str().expect("other UTF-8")).expect("encode path")
            ),
        )
        .expect("write other session");
        fs::write(sessions.join("broken.jsonl"), "not JSON\n").expect("write corrupt session");

        let (discovered, timing) = PiSessionStore::new(&sessions, &workspace)
            .expect("session store")
            .list_timed()
            .expect("list sessions");
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].summary.id.to_string(), id.to_string());
        assert_eq!(discovered[0].summary.name.as_deref(), Some("Feature"));
        assert_eq!(discovered[0].summary.message_count, 1);
        assert_eq!(
            discovered[0].summary.first_user_message.as_deref(),
            Some("Hello")
        );
        assert_eq!(timing.file_count, 3);
        assert_eq!(timing.session_count, 1);
        assert_eq!(timing.parsed_count, 3);
        assert_eq!(timing.reused_count, 0);
    }

    #[test]
    fn warm_list_reuses_unchanged_jsonl_and_reparses_after_edit() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let path = sessions.join("good.jsonl");
        fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{}}}\n{{\"type\":\"session_info\",\"name\":\"One\"}}\n",
                serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
                    .expect("encode path")
            ),
        )
        .expect("write session");

        let store = PiSessionStore::new(&sessions, &workspace).expect("session store");
        let mut index = super::SessionMetadataIndex::default();
        let (_, first) = store.list_cached(&mut index, None).expect("cold list");
        assert_eq!(first.parsed_count, 1);
        assert_eq!(first.reused_count, 0);

        let (warm_sessions, second) = store.list_cached(&mut index, None).expect("warm list");
        assert_eq!(second.parsed_count, 0);
        assert_eq!(second.reused_count, 1);
        assert_eq!(warm_sessions[0].summary.name.as_deref(), Some("One"));

        // Some Linux filesystems can retain the same mtime for a same-sized
        // rewrite performed within one timestamp tick. Cross that boundary
        // so this test exercises the documented mtime+size cache key rather
        // than depending on the host filesystem's clock granularity.
        std::thread::sleep(std::time::Duration::from_millis(25));
        fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{}}}\n{{\"type\":\"session_info\",\"name\":\"Two\"}}\n",
                serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
                    .expect("encode path")
            ),
        )
        .expect("rewrite session");
        let (edited, third) = store.list_cached(&mut index, None).expect("edited list");
        assert_eq!(third.parsed_count, 1);
        assert_eq!(edited[0].summary.name.as_deref(), Some("Two"));

        fs::remove_file(&path).expect("delete session");
        let (gone, fourth) = store.list_cached(&mut index, None).expect("deleted list");
        assert!(gone.is_empty());
        assert_eq!(fourth.file_count, 0);
    }

    #[test]
    fn list_limit_keeps_the_newest_sessions() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let cwd = serde_json::to_string(workspace.to_str().expect("workspace UTF-8")).expect("cwd");
        for (name, stamp) in [
            ("old", "2026-08-12T00:00:00Z"),
            ("new", "2026-08-12T01:00:00Z"),
        ] {
            let id = Uuid::new_v4();
            fs::write(
                sessions.join(format!("{name}.jsonl")),
                format!(
                    "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"{stamp}\",\"cwd\":{cwd}}}\n{{\"type\":\"message\",\"timestamp\":\"{stamp}\",\"message\":{{\"role\":\"user\",\"content\":\"{name}\"}}}}\n"
                ),
            )
            .expect("write session");
        }
        let store = PiSessionStore::new(&sessions, &workspace).expect("session store");
        let mut index = super::SessionMetadataIndex::default();
        let (listed, timing) = store
            .list_cached(&mut index, Some(1))
            .expect("limited list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].summary.first_user_message.as_deref(), Some("new"));
        assert_eq!(timing.file_count, 2);
        assert_eq!(timing.session_count, 1);
    }

    #[test]
    fn history_page_streams_native_jsonl_and_keeps_cursor_revision() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let cwd = serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
            .expect("encode cwd");
        let mut contents = format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n"
        );
        for index in 0..75 {
            contents.push_str(
                &serde_json::json!({
                    "type": "message",
                    "message": {"role": "user", "content": format!("message-{index}")}
                })
                .to_string(),
            );
            contents.push('\n');
        }
        let path = sessions.join("history.jsonl");
        fs::write(&path, contents).expect("write session");
        let store = PiSessionStore::new(&sessions, &workspace).expect("session store");

        let first = store
            .history_page(SessionId::from_uuid(id), None, 50)
            .expect("latest history page");
        assert_eq!(first.start_index, 25);
        assert_eq!(first.messages.len(), 50);
        assert_eq!(first.revision, 75);
        let cursor = first.next_cursor.clone().expect("older cursor");

        // Appending live history does not move the cursor's boundary. The
        // older page still describes the same 75-message snapshot.
        let mut appended = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open session for append");
        writeln!(
            appended,
            "{}",
            serde_json::json!({
                "type": "message",
                "message": {"role": "user", "content": "message-75"}
            })
        )
        .expect("append live message");

        let older = store
            .history_page(SessionId::from_uuid(id), Some(&cursor), 50)
            .expect("older history page");
        assert_eq!(older.start_index, 0);
        assert_eq!(older.messages.len(), 25);
        assert_eq!(older.revision, 75);
        assert!(!older.has_more);
    }

    #[test]
    fn indexed_history_keeps_source_indexes_and_replaces_large_messages_with_placeholders() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let giant = "x".repeat(pix_wire::MAX_HISTORY_PAGE_BYTES + 64 * 1024);
        let entries = vec![
            serde_json::json!({"type":"session","version":3,"id":id,"timestamp":"2026-08-12T00:00:00Z","cwd":workspace}),
            serde_json::json!({"type":"message","message":{"role":"user","content":"first"}}),
            serde_json::json!({"type":"message","message":{"role":"assistant","content":giant}}),
            serde_json::json!({"type":"message","message":{"role":"user","content":"latest question"}}),
            serde_json::json!({"type":"message","message":{"role":"assistant","content":"latest answer"}}),
        ];
        let mut text = String::new();
        for entry in entries {
            text.push_str(&entry.to_string());
            text.push('\n');
        }
        let path = sessions.join("history.jsonl");
        fs::write(&path, text).expect("write history");
        let store = PiSessionStore::new(&sessions, &workspace).expect("store");
        let mut index = super::SessionMetadataIndex::default();
        store.list_cached(&mut index, None).expect("index history");

        let page = store
            .history_page_with_transform_cached_options(
                &mut index,
                SessionId::from_uuid(id),
                None,
                50,
                true,
                Ok,
            )
            .expect("history page");
        assert!(page.messages.is_empty());
        assert_eq!(page.start_index, 0);
        assert_eq!(page.history_items.len(), 4);
        assert_eq!(
            page.history_items
                .iter()
                .map(|item| match item {
                    HistoryPageItem::Message { index, .. }
                    | HistoryPageItem::Placeholder { index, .. } => *index,
                })
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert!(matches!(
            page.history_items[1],
            HistoryPageItem::Placeholder {
                index: 1,
                truncated: true,
                ..
            }
        ));
    }

    #[test]
    fn oversized_final_assistant_does_not_hide_the_final_user_message() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let giant = "answer ".repeat(110_000);
        let entries = [
            serde_json::json!({
                "type":"session",
                "version":3,
                "id":id,
                "timestamp":"2026-08-12T00:00:00Z",
                "cwd":workspace
            }),
            serde_json::json!({"type":"message","message":{"role":"user","content":"final question"}}),
            serde_json::json!({"type":"message","message":{"role":"assistant","content":giant}}),
        ];
        let mut text = String::new();
        for entry in entries {
            text.push_str(&entry.to_string());
            text.push('\n');
        }
        let path = sessions.join("final-large.jsonl");
        fs::write(&path, text).expect("write history");
        let store = PiSessionStore::new(&sessions, &workspace).expect("store");
        let mut index = super::SessionMetadataIndex::default();
        store.list_cached(&mut index, None).expect("index history");
        let page = store
            .history_page_with_transform_cached_options(
                &mut index,
                SessionId::from_uuid(id),
                None,
                50,
                true,
                Ok,
            )
            .expect("history page");
        assert_eq!(page.history_items.len(), 2);
        assert!(matches!(
            page.history_items[0],
            HistoryPageItem::Message { index: 0, .. }
        ));
        assert!(matches!(
            page.history_items[1],
            HistoryPageItem::Placeholder {
                index: 1,
                truncated: true,
                ..
            }
        ));
    }

    #[test]
    fn unknown_history_roles_are_retained_as_bounded_placeholders() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let cwd = serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
            .expect("encode cwd");
        let path = sessions.join("unknown-role.jsonl");
        fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"futurePiRole\",\"content\":\"opaque\"}}}}\n"
            ),
        )
        .expect("write history");
        let store = PiSessionStore::new(&sessions, &workspace).expect("store");
        let mut index = super::SessionMetadataIndex::default();
        store.list_cached(&mut index, None).expect("index history");
        let page = store
            .history_page_with_transform_cached_options(
                &mut index,
                SessionId::from_uuid(id),
                None,
                50,
                true,
                Ok,
            )
            .expect("history page");
        assert!(matches!(
            page.history_items.first(),
            Some(HistoryPageItem::Placeholder {
                role: Some(role),
                preview,
                ..
            }) if role == "futurePiRole" && preview == "opaque"
        ));
    }

    #[test]
    fn committed_fence_ignores_partial_trailing_jsonl_record() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let cwd = serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
            .expect("encode cwd");
        let header = format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n"
        );
        let path = sessions.join("partial.jsonl");
        fs::write(
            &path,
            format!(
                "{header}{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"complete\"}}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"content\":\"partial"
            ),
        )
        .expect("write partial history");
        let store = PiSessionStore::new(&sessions, &workspace).expect("store");
        let page = store
            .history_page(SessionId::from_uuid(id), None, 50)
            .expect("history page");
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.start_index, 0);
    }

    #[test]
    fn presentation_anchors_stay_with_the_final_turn() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let cwd = serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
            .expect("encode cwd");
        let header = format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n"
        );
        let messages = [
            serde_json::json!({"type":"message","message":{"role":"user","content":"inspect"}}),
            serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"read"}]}}),
            serde_json::json!({"type":"message","message":{"role":"toolResult","content":[{"type":"text","text":"body"}]}}),
            serde_json::json!({"type":"message","message":{"role":"assistant","content":"done"}}),
        ];
        let mut text = header;
        for message in messages {
            text.push_str(&message.to_string());
            text.push('\n');
        }
        fs::write(sessions.join("presentation.jsonl"), text).expect("write presentation");
        let store = PiSessionStore::new(&sessions, &workspace).expect("store");
        let mut index = super::SessionMetadataIndex::default();
        store.list_cached(&mut index, None).expect("index history");
        let presentation = store
            .history_presentation_cached(&mut index, SessionId::from_uuid(id), SessionState::Idle)
            .expect("presentation");
        assert_eq!(presentation.turn_state, TurnPresentationState::Completed);
        assert_eq!(
            presentation.user_anchor.map(|anchor| anchor.source_index),
            Some(0)
        );
        assert_eq!(
            presentation
                .terminal_anchor
                .map(|anchor| anchor.source_index),
            Some(3)
        );
        assert!(presentation.process.tool_count > 0);

        let active_path = sessions.join("active.jsonl");
        let active_id = Uuid::new_v4();
        let active_header = format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{active_id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n"
        );
        fs::write(
            &active_path,
            format!(
                "{active_header}{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"run\"}}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"toolCall\",\"id\":\"call-2\"}}]}}}}\n"
            ),
        )
        .expect("write active history");
        let active_presentation = store
            .history_presentation_cached(
                &mut index,
                SessionId::from_uuid(active_id),
                SessionState::Running,
            )
            .expect("active presentation");
        assert_eq!(
            active_presentation.turn_state,
            TurnPresentationState::Active
        );
        assert!(active_presentation.terminal_anchor.is_none());

        let aborted_id = Uuid::new_v4();
        let aborted_path = sessions.join("aborted.jsonl");
        let aborted_header = format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{aborted_id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n"
        );
        fs::write(
            &aborted_path,
            format!(
                "{aborted_header}{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"stop\"}}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"stopReason\":\"aborted\",\"content\":\"partial\"}}}}\n"
            ),
        )
        .expect("write aborted history");
        let aborted_presentation = store
            .history_presentation_cached(
                &mut index,
                SessionId::from_uuid(aborted_id),
                SessionState::Idle,
            )
            .expect("aborted presentation");
        assert_eq!(
            aborted_presentation.turn_state,
            TurnPresentationState::Aborted
        );
    }

    #[test]
    fn indexed_cursor_rejects_a_rewritten_boundary() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&sessions).expect("create sessions");
        let id = Uuid::new_v4();
        let cwd = serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
            .expect("encode cwd");
        let path = sessions.join("rewrite.jsonl");
        let mut text = format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n"
        );
        for index in 0..3 {
            text.push_str(
                &serde_json::json!({"type":"message","message":{"role":"user","content":format!("m{index}")}}).to_string(),
            );
            text.push('\n');
        }
        fs::write(&path, text).expect("write history");
        let store = PiSessionStore::new(&sessions, &workspace).expect("store");
        let mut index = super::SessionMetadataIndex::default();
        store.list_cached(&mut index, None).expect("index history");
        let page = store
            .history_page_with_transform_cached_options(
                &mut index,
                SessionId::from_uuid(id),
                None,
                1,
                true,
                Ok,
            )
            .expect("page");
        let cursor = page.next_cursor.expect("cursor");
        fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"rewritten\"}}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"m1\"}}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"m2\"}}}}\n"
            ),
        )
        .expect("rewrite history");
        let error = store
            .history_page_with_transform_cached_options(
                &mut index,
                SessionId::from_uuid(id),
                Some(&cursor),
                1,
                true,
                Ok,
            )
            .expect_err("rewritten cursor must fail");
        assert!(matches!(
            error,
            SessionError::History(crate::session_history::HistoryError::InvalidCursor)
        ));
    }

    #[test]
    fn scales_cold_and_warm_scans_across_one_hundred_and_one_thousand_files() {
        for count in [100_usize, 1000_usize] {
            let directory = tempdir().expect("temporary directory");
            let workspace = directory.path().join("workspace");
            let sessions = directory.path().join("sessions");
            fs::create_dir_all(&workspace).expect("create workspace");
            fs::create_dir_all(&sessions).expect("create sessions");
            let cwd = serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
                .expect("encode path");
            for index in 0..count {
                let id = Uuid::new_v4();
                fs::write(
                    sessions.join(format!("{index:04}.jsonl")),
                    format!(
                        "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n{{\"type\":\"message\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":\"n{index}\"}}}}\n"
                    ),
                )
                .expect("write session");
            }
            let store = PiSessionStore::new(&sessions, &workspace).expect("session store");
            let mut index = super::SessionMetadataIndex::default();
            let (cold, first) = store.list_cached(&mut index, None).expect("cold scan");
            assert_eq!(cold.len(), count);
            assert_eq!(first.parsed_count, u64::try_from(count).expect("count"));
            assert_eq!(first.reused_count, 0);

            let (warm, second) = store.list_cached(&mut index, None).expect("warm scan");
            assert_eq!(warm.len(), count);
            assert_eq!(second.parsed_count, 0);
            assert_eq!(second.reused_count, u64::try_from(count).expect("count"));
            assert!(
                second.scan_ms <= first.scan_ms.saturating_add(50),
                "warm scan should not be slower than cold+slack: cold={} warm={}",
                first.scan_ms,
                second.scan_ms
            );
        }
    }

    #[test]
    #[ignore = "generated 50/100 MiB history benchmark"]
    fn generated_large_histories_read_only_a_bounded_tail_window() {
        for target_bytes in [50_usize * 1024 * 1024, 100_usize * 1024 * 1024] {
            let directory = tempdir().expect("temporary directory");
            let workspace = directory.path().join("workspace");
            let sessions = directory.path().join("sessions");
            fs::create_dir_all(&workspace).expect("create workspace");
            fs::create_dir_all(&sessions).expect("create sessions");
            let id = Uuid::new_v4();
            let cwd = serde_json::to_string(workspace.to_str().expect("workspace UTF-8"))
                .expect("encode cwd");
            let path = sessions.join("large.jsonl");
            let mut file = File::create(&path).expect("create history");
            writeln!(
                file,
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}"
            )
            .expect("write header");
            let filler = "x".repeat(900);
            let mut index = 0_usize;
            let mut written = 0_usize;
            while written < target_bytes {
                let line = serde_json::json!({
                    "type": "message",
                    "message": {
                        "role": if index.is_multiple_of(2) { "user" } else { "assistant" },
                        "content": format!("{index:08} {filler}")
                    }
                });
                let line = format!("{line}\n");
                written = written.saturating_add(line.len());
                file.write_all(line.as_bytes()).expect("write message");
                index = index.saturating_add(1);
            }
            file.flush().expect("flush history");

            let store = PiSessionStore::new(&sessions, &workspace).expect("store");
            let mut catalog = super::SessionMetadataIndex::default();
            store
                .list_cached(&mut catalog, None)
                .expect("index history");
            let _ = crate::diagnostics::take_thread_records();
            let page = store
                .history_page_with_transform_cached_options(
                    &mut catalog,
                    SessionId::from_uuid(id),
                    None,
                    50,
                    true,
                    Ok,
                )
                .expect("tail page");
            assert_eq!(page.history_items.len(), 50);
            let metrics = crate::diagnostics::take_thread_records()
                .into_iter()
                .find(|(event, _)| event == "session.history.read")
                .map(|(_, body)| body)
                .expect("history read metrics");
            let bytes_read = metrics
                .get("bytes_read")
                .and_then(Value::as_u64)
                .expect("bytes_read metric");
            assert!(
                bytes_read < u64::try_from(target_bytes / 2).expect("target fits"),
                "tail reader should not scan the complete {target_bytes} byte file: {bytes_read}"
            );
        }
    }
}
