//! Bounded, byte-aware windows over a Pi session's current message view.
//!
//! Pi's RPC `get_messages` command returns the complete in-memory message
//! array, so history-capable callers use the native JSONL source and feed this
//! selector one message at a time. Only a small recent window crosses the Pix
//! wire boundary. Cursors are opaque to clients and carry the authenticated
//! session identity, a fixed snapshot revision, and the exclusive message
//! index for the next page.

use std::collections::VecDeque;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pix_wire::{
    HistoryState, MAX_ENCRYPTED_FRAME_BYTES, MAX_HISTORY_PAGE_BYTES, MAX_HISTORY_PAGE_MESSAGES,
    SessionHistoryPage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Builds the latest bounded history page for a session snapshot.
///
/// # Errors
///
/// Returns [`HistoryError`] when a message cannot be encoded or cannot fit the
/// encrypted frame budget.
pub fn initial_page(
    session_id: &str,
    messages: &[Value],
) -> Result<SessionHistoryPage, HistoryError> {
    page_from_messages(session_id, messages, None, MAX_HISTORY_PAGE_MESSAGES)
}

/// Builds one page containing messages strictly before `cursor`.
///
/// # Errors
///
/// Returns [`HistoryError::InvalidCursor`] when the cursor is malformed or
/// belongs to another session, and [`HistoryError::MessageTooLarge`] when a
/// retained message cannot fit one encrypted frame.
pub fn page_from_cursor(
    session_id: &str,
    messages: &[Value],
    cursor: &str,
    limit: u32,
) -> Result<SessionHistoryPage, HistoryError> {
    let cursor = decode_cursor(session_id, cursor)?;
    page_from_messages_with_revision(
        session_id,
        messages,
        Some(cursor.before_index),
        limit,
        Some(cursor.revision),
    )
}

/// Builds a bounded page from an indexed stream of messages. `before_index`
/// is exclusive; `None` selects the latest messages. The selector keeps only
/// the candidate page in memory, so callers can feed it a native JSONL file
/// without materializing the complete session history.
///
/// # Errors
///
/// Returns [`HistoryError`] when the requested page size is invalid, a message
/// cannot be encoded, or one message exceeds the frame budget.
pub fn page_from_messages(
    session_id: &str,
    messages: &[Value],
    before_index: Option<usize>,
    limit: u32,
) -> Result<SessionHistoryPage, HistoryError> {
    page_from_messages_with_revision(session_id, messages, before_index, limit, None)
}

/// Builds a page against a fixed history revision. This keeps a cursor valid
/// while Pi appends newer live messages after the snapshot boundary.
///
/// # Errors
///
/// Returns [`HistoryError`] when the requested page size/cursor boundary is
/// invalid or a retained message cannot fit the frame budget.
pub fn page_from_messages_with_revision(
    session_id: &str,
    messages: &[Value],
    before_index: Option<usize>,
    limit: u32,
    revision: Option<usize>,
) -> Result<SessionHistoryPage, HistoryError> {
    let mut builder = MessagePageBuilder::with_revision(before_index, limit, revision)?;
    for (index, message) in messages.iter().enumerate() {
        builder.push(index, message.clone())?;
    }
    builder.finish(session_id)
}

/// Streaming selector used by [`PiSessionStore`](crate::session::PiSessionStore)
/// while it scans a Pi JSONL file. The message values retained here are at
/// most one page plus the byte-budgeted suffix.
#[derive(Debug)]
pub struct MessagePageBuilder {
    before_index: Option<usize>,
    revision: Option<usize>,
    limit: usize,
    messages: VecDeque<(usize, Value, usize)>,
    encoded_bytes: usize,
    seen_count: usize,
}

impl MessagePageBuilder {
    /// Creates a selector without a fixed revision boundary.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::InvalidLimit`] when `requested_limit` is zero
    /// or exceeds the protocol maximum.
    pub fn new(before_index: Option<usize>, requested_limit: u32) -> Result<Self, HistoryError> {
        Self::with_revision(before_index, requested_limit, None)
    }

    /// Creates a selector tied to a fixed history revision.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::InvalidLimit`] for a zero page size or
    /// [`HistoryError::InvalidCursor`] when `before_index` exceeds `revision`.
    pub fn with_revision(
        before_index: Option<usize>,
        requested_limit: u32,
        revision: Option<usize>,
    ) -> Result<Self, HistoryError> {
        let max_limit = usize::try_from(MAX_HISTORY_PAGE_MESSAGES).unwrap_or(usize::MAX);
        let limit = usize::try_from(requested_limit).unwrap_or(usize::MAX);
        if limit == 0 || limit > max_limit {
            return Err(HistoryError::InvalidLimit);
        }
        if let (Some(before_index), Some(revision)) = (before_index, revision)
            && before_index > revision
        {
            return Err(HistoryError::InvalidCursor);
        }
        Ok(Self {
            before_index,
            revision,
            limit,
            messages: VecDeque::new(),
            encoded_bytes: 0,
            seen_count: 0,
        })
    }

    /// Feeds one indexed message to the bounded suffix selector.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::Encode`] for an unencodable JSON value or
    /// [`HistoryError::MessageTooLarge`] when the first retained message is
    /// larger than one encrypted frame.
    pub fn push(&mut self, index: usize, message: Value) -> Result<(), HistoryError> {
        if self.revision.is_some_and(|revision| index >= revision) {
            return Ok(());
        }
        self.seen_count = self.seen_count.saturating_add(1);
        if self.before_index.is_some_and(|before| index >= before) {
            return Ok(());
        }

        let message_bytes = serde_json::to_vec(&message)
            .map_err(HistoryError::Encode)?
            .len();
        let separator = usize::from(!self.messages.is_empty());
        let target = MAX_HISTORY_PAGE_BYTES.saturating_sub(4096);
        if self.messages.is_empty()
            && message_bytes.saturating_add(4096) > MAX_ENCRYPTED_FRAME_BYTES
        {
            return Err(HistoryError::MessageTooLarge(message_bytes));
        }

        self.messages.push_back((index, message, message_bytes));
        self.encoded_bytes = self
            .encoded_bytes
            .saturating_add(separator)
            .saturating_add(message_bytes);
        while self.messages.len() > self.limit || self.encoded_bytes > target {
            let Some((_, _, bytes)) = self.messages.pop_front() else {
                break;
            };
            self.encoded_bytes = self.encoded_bytes.saturating_sub(bytes);
            if !self.messages.is_empty() {
                self.encoded_bytes = self.encoded_bytes.saturating_sub(1);
            }
        }
        Ok(())
    }

    /// Finishes the selector and creates the wire page plus its next cursor.
    ///
    /// # Errors
    ///
    /// This currently cannot fail, but remains fallible so the selector's
    /// public API can add protocol validation without changing callers.
    pub fn finish(self, session_id: &str) -> Result<SessionHistoryPage, HistoryError> {
        let start_index = self.messages.front().map_or_else(
            || self.before_index.unwrap_or(self.seen_count),
            |(index, _, _)| *index,
        );
        let has_more = start_index > 0;
        let revision = self.revision.unwrap_or(self.seen_count);
        let next_cursor = has_more.then(|| encode_cursor(session_id, start_index, revision));
        Ok(SessionHistoryPage {
            session_id: session_id.to_owned(),
            messages: self
                .messages
                .into_iter()
                .map(|(_, message, _)| message)
                .collect(),
            start_index,
            has_more,
            next_cursor,
            revision: u64::try_from(revision).unwrap_or(u64::MAX),
            history_items: Vec::new(),
        })
    }
}

/// Converts a page into the compact snapshot metadata sent to iOS.
#[must_use]
pub fn state(page: &SessionHistoryPage) -> HistoryState {
    HistoryState {
        start_index: page.start_index,
        has_more: page.has_more,
        cursor: page.next_cursor.clone(),
        revision: page.revision,
        presentation: None,
    }
}

/// Returns the exclusive message index encoded in a cursor after checking its
/// session binding. This is used by the JSONL streaming path.
///
/// # Errors
///
/// Returns [`HistoryError::InvalidCursor`] for malformed or cross-session
/// cursors.
pub fn before_index(session_id: &str, value: &str) -> Result<usize, HistoryError> {
    Ok(decode_cursor(session_id, value)?.before_index)
}

/// Returns the fixed history revision encoded in a cursor.
///
/// # Errors
///
/// Returns [`HistoryError::InvalidCursor`] for malformed or cross-session
/// cursors.
pub fn cursor_revision(session_id: &str, value: &str) -> Result<usize, HistoryError> {
    Ok(decode_cursor(session_id, value)?.revision)
}

/// Cursor fields used by the indexed reverse-reader path. The cursor remains
/// opaque to clients; these values are validated only by the Host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedCursor {
    pub before_index: usize,
    pub revision: usize,
    pub history_epoch: String,
    pub snapshot_end_offset: u64,
    pub before_offset: u64,
    pub boundary_fingerprint: u64,
}

/// Encodes a v2 cursor tied to one in-memory history epoch and committed
/// record boundary. The older v1 encoding remains available for compatibility.
#[must_use]
///
/// # Panics
///
/// Panics only if the internal cursor payload cannot be serialized, which
/// cannot occur for the fixed serializable fields in this function.
pub fn encode_indexed_cursor(
    session_id: &str,
    before_index: usize,
    revision: usize,
    history_epoch: &str,
    snapshot_end_offset: u64,
    before_offset: u64,
    boundary_fingerprint: u64,
) -> String {
    let payload = CursorPayload {
        version: 2,
        session_id: session_id.to_owned(),
        before_index,
        revision,
        history_epoch: Some(history_epoch.to_owned()),
        snapshot_end_offset: Some(snapshot_end_offset),
        before_offset: Some(before_offset),
        boundary_fingerprint: Some(boundary_fingerprint),
    };
    let encoded = serde_json::to_vec(&payload).expect("history cursor is serializable");
    URL_SAFE_NO_PAD.encode(encoded)
}

/// Decodes a v1 or indexed v2 cursor. Callers that need reverse reads must
/// reject v1 cursors when no offset fallback is available.
///
/// # Errors
///
/// Returns [`HistoryError::InvalidCursor`] for malformed, cross-session, or
/// incomplete v2 payloads. Returns [`HistoryError::LegacyCursor`] for a valid
/// v1 cursor that has no indexed byte offsets.
pub fn indexed_cursor(session_id: &str, value: &str) -> Result<IndexedCursor, HistoryError> {
    let payload = decode_cursor(session_id, value)?;
    let Some(history_epoch) = payload.history_epoch else {
        return Err(HistoryError::LegacyCursor);
    };
    let Some(snapshot_end_offset) = payload.snapshot_end_offset else {
        return Err(HistoryError::LegacyCursor);
    };
    let Some(before_offset) = payload.before_offset else {
        return Err(HistoryError::LegacyCursor);
    };
    let Some(boundary_fingerprint) = payload.boundary_fingerprint else {
        return Err(HistoryError::LegacyCursor);
    };
    Ok(IndexedCursor {
        before_index: payload.before_index,
        revision: payload.revision,
        history_epoch,
        snapshot_end_offset,
        before_offset,
        boundary_fingerprint,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct CursorPayload {
    version: u8,
    session_id: String,
    before_index: usize,
    revision: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history_epoch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snapshot_end_offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before_offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    boundary_fingerprint: Option<u64>,
}

fn encode_cursor(session_id: &str, before_index: usize, revision: usize) -> String {
    let payload = CursorPayload {
        version: 1,
        session_id: session_id.to_owned(),
        before_index,
        revision,
        history_epoch: None,
        snapshot_end_offset: None,
        before_offset: None,
        boundary_fingerprint: None,
    };
    let encoded = serde_json::to_vec(&payload).expect("history cursor is serializable");
    URL_SAFE_NO_PAD.encode(encoded)
}

fn decode_cursor(session_id: &str, value: &str) -> Result<CursorPayload, HistoryError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| HistoryError::InvalidCursor)?;
    let payload: CursorPayload =
        serde_json::from_slice(&bytes).map_err(|_| HistoryError::InvalidCursor)?;
    if !matches!(payload.version, 1 | 2) || payload.session_id != session_id {
        return Err(HistoryError::InvalidCursor);
    }
    if payload.version == 2
        && (payload.history_epoch.is_none()
            || payload.snapshot_end_offset.is_none()
            || payload.before_offset.is_none()
            || payload.boundary_fingerprint.is_none())
    {
        return Err(HistoryError::InvalidCursor);
    }
    Ok(payload)
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("history page limit is invalid")]
    InvalidLimit,
    #[error("history cursor is invalid or belongs to another session")]
    InvalidCursor,
    #[error("history cursor uses the legacy format without indexed offsets")]
    LegacyCursor,
    #[error("history message is {0} bytes, exceeding the wire frame budget")]
    MessageTooLarge(usize),
    #[error("encoded history page is {0} bytes, exceeding the history target")]
    PageTooLarge(usize),
    #[error("failed to encode a history message: {0}")]
    Encode(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::{HistoryError, initial_page, page_from_cursor, page_from_messages_with_revision};
    use pix_wire::{MAX_HISTORY_PAGE_BYTES, MAX_HISTORY_PAGE_MESSAGES};
    use serde_json::json;

    #[test]
    fn initial_page_keeps_latest_messages_and_returns_opaque_cursor() {
        let messages = (0..75)
            .map(|index| json!({"role":"user","content":index}))
            .collect::<Vec<_>>();
        let page = initial_page("session-1", &messages).expect("initial history page");
        assert_eq!(
            page.messages.len(),
            usize::try_from(MAX_HISTORY_PAGE_MESSAGES).unwrap()
        );
        assert_eq!(page.start_index, 25);
        assert!(page.has_more);
        assert!(
            page.next_cursor
                .as_deref()
                .is_some_and(|value| !value.contains("session-1"))
        );
        assert!(page.next_cursor.as_deref().unwrap().len() < 256);
    }

    #[test]
    fn history_page_is_byte_bounded_before_message_count() {
        let large = "x".repeat(MAX_HISTORY_PAGE_BYTES - 3_000);
        let messages = vec![
            json!({"role":"user","content":"old"}),
            json!({"role":"user","content":large}),
            json!({"role":"user","content":"new"}),
        ];
        let page = initial_page("session-1", &messages).expect("initial history page");
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.start_index, 2);
    }

    #[test]
    fn cursor_pages_older_messages_and_rejects_cross_session() {
        let messages = (0..75)
            .map(|index| json!({"role":"user","content":index}))
            .collect::<Vec<_>>();
        let first = initial_page("session-1", &messages).expect("initial page");
        let older = page_from_cursor(
            "session-1",
            &messages,
            first.next_cursor.as_deref().unwrap_or(""),
            50,
        )
        .expect("older page");
        assert_eq!(older.start_index, 0);
        assert_eq!(older.messages.len(), 25);
        assert!(!older.has_more);
        assert!(matches!(
            page_from_cursor(
                "session-2",
                &messages,
                first.next_cursor.as_deref().unwrap(),
                2
            ),
            Err(HistoryError::InvalidCursor)
        ));
    }

    #[test]
    fn cursor_revision_excludes_messages_appended_after_snapshot_boundary() {
        let mut messages = (0..75)
            .map(|index| json!({"role":"user","content":index}))
            .collect::<Vec<_>>();
        let first = initial_page("session-1", &messages).expect("initial page");
        messages.extend((75..80).map(|index| json!({"role":"user","content":index})));
        let older =
            page_from_messages_with_revision("session-1", &messages, Some(25), 50, Some(75))
                .expect("older page at boundary");
        assert_eq!(older.revision, 75);
        assert_eq!(older.messages.len(), 25);
        assert_eq!(older.start_index, 0);
        assert_eq!(older.next_cursor, None);
        assert_eq!(first.revision, 75);
    }

    #[test]
    fn page_limit_is_bounded_by_protocol_maximum() {
        let messages = vec![json!({"role":"user","content":"hello"})];
        assert!(matches!(
            super::page_from_messages("session-1", &messages, None, 0),
            Err(HistoryError::InvalidLimit)
        ));
        assert!(matches!(
            super::page_from_messages("session-1", &messages, None, MAX_HISTORY_PAGE_MESSAGES + 1),
            Err(HistoryError::InvalidLimit)
        ));
    }
}
