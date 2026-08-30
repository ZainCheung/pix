use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use chrono::{DateTime, Utc};
use directories::BaseDirs;
use serde_json::Value;
use thiserror::Error;

use crate::pi_rpc::{RpcClient, RpcError};
use crate::session_lock::SessionId;

const MAX_SESSION_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
struct SessionScan {
    header: Option<Value>,
    name: Option<String>,
    message_count: usize,
    first_user_message: Option<String>,
    last_activity: Option<DateTime<Utc>>,
}

impl SessionScan {
    fn visit(&mut self, entry: Value) -> bool {
        if self.header.is_none() {
            if entry.get("type").and_then(Value::as_str) != Some("session") {
                return false;
            }
            self.header = Some(entry);
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
            Some("message") => self.visit_message(&entry),
            _ => {}
        }
        true
    }

    fn visit_message(&mut self, entry: &Value) {
        self.message_count = self.message_count.saturating_add(1);
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
        let state = snapshot.state;
        Ok(Self {
            session_id: required_string(&state, "sessionId")?.to_owned(),
            session_name: state
                .get("sessionName")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            model: state.get("model").filter(|value| !value.is_null()).cloned(),
            thinking_level: required_string(&state, "thinkingLevel")?.to_owned(),
            is_streaming: required_bool(&state, "isStreaming")?,
            is_compacting: required_bool(&state, "isCompacting")?,
            pending_message_count: required_usize(&state, "pendingMessageCount")?,
            messages: snapshot.messages,
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
}

#[derive(Debug, Clone)]
struct CachedSessionFile {
    modified: SystemTime,
    size: u64,
    session: Option<DiscoveredSession>,
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
        self.list()?
            .into_iter()
            .find(|session| session.summary.id == id)
            .ok_or(SessionError::NotFound(id))
    }
}

impl SessionMetadataIndex {
    /// Drops cached summaries under `directory` so the next list reparses.
    pub fn invalidate_directory(&mut self, directory: &Path) {
        self.files
            .retain(|path, _| path.parent() != Some(directory));
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
        self.files
            .retain(|path, _| path.parent() != Some(directory) || live.contains(path));
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
            self.files.remove(path);
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
        let session = parse_session_file(path, workspace)?;
        self.files.insert(
            path.to_path_buf(),
            CachedSessionFile {
                modified,
                size,
                session: session.clone(),
            },
        );
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

fn parse_session_file(
    path: &Path,
    canonical_workspace: &Path,
) -> Result<Option<DiscoveredSession>, SessionError> {
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
        if !scan.visit(entry) {
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
    Ok(Some(DiscoveredSession {
        summary: SessionSummary {
            id,
            name: scan.name,
            created_at,
            modified_at,
            message_count: scan.message_count,
            first_user_message: scan.first_user_message,
        },
        path: path.to_path_buf(),
    }))
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
    #[error("Pi snapshot field {0} is absent or incompatible")]
    InvalidSnapshot(&'static str),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;
    use uuid::Uuid;

    use super::PiSessionStore;

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
}
