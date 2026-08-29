//! Host-local ownership and protocol harness for the optional Pi TUI bridge.
//!
//! This module deliberately stops at the host compatibility boundary.  It
//! does not start a Pi process, persist conversation content, or implement the
//! Pix wire protocol.  A transport adapter is expected to obtain the peer UID
//! and process identity from the Unix socket before calling [`TuiBridgeRegistry
//!::register`]; the REGISTER payload is never trusted for either value.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::session::{PiSessionStore, SessionError};
use crate::session_lock::{
    ProcessIdentity, SessionId, SessionLease, SessionLockError, SessionOwnerKind,
    workspace_fingerprint,
};
use pix_wire::SessionState;

/// Version of the host-local TUI bridge protocol.
pub const TUI_BRIDGE_PROTOCOL_VERSION: u32 = 1;
/// Maximum size of one newline-delimited bridge frame.
pub const TUI_BRIDGE_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const REGISTER_MESSAGE_TYPE: &str = "register";

/// REGISTER payload sent by the optional Pi extension.
///
/// `cwd` and `session_file` are hints only.  The host canonicalizes and
/// re-checks them against its own workspace/session view before claiming the
/// ownership gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiBridgeRegister {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub bridge_instance_id: Uuid,
    pub extension_version: u32,
    pub session_id: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub session_file: Option<PathBuf>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl TuiBridgeRegister {
    /// Creates a minimal REGISTER frame for a harness or transport adapter.
    #[must_use]
    pub fn new(session_id: SessionId, cwd: impl Into<PathBuf>, bridge_instance_id: Uuid) -> Self {
        Self {
            version: TUI_BRIDGE_PROTOCOL_VERSION,
            message_type: REGISTER_MESSAGE_TYPE.to_owned(),
            bridge_instance_id,
            extension_version: 1,
            session_id: session_id.to_string(),
            cwd: cwd.into(),
            session_file: None,
            reason: Some("startup".to_owned()),
            capabilities: vec!["events.v1".to_owned(), "snapshot.v1".to_owned()],
        }
    }
}

/// Peer credentials obtained by the Unix transport adapter.
///
/// The extension cannot provide this structure.  Keeping it separate from
/// [`TuiBridgeRegister`] makes the trust boundary explicit in the API and in
/// tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiBridgePeer {
    pub uid: u32,
    pub process: ProcessIdentity,
}

impl TuiBridgePeer {
    #[must_use]
    pub const fn new(uid: u32, process: ProcessIdentity) -> Self {
        Self { uid, process }
    }
}

/// Whether the bridge connection currently reaches the TUI process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuiBridgeConnectionState {
    Attached,
    Unreachable,
}

/// A release/reconnect token.  Generation and nonce are intentionally
/// returned to the caller so stale connections cannot release replacements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiBridgeToken {
    pub session_id: SessionId,
    pub bridge_instance_id: Uuid,
    pub owner: ProcessIdentity,
    pub generation: u64,
    pub claim_nonce: Uuid,
}

/// Result of a successful register or host-restart restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiBridgeRegistration {
    pub token: TuiBridgeToken,
    pub state: TuiBridgeConnectionState,
    /// True when no durable JSONL session was found during REGISTER.  The
    /// ownership claim remains in memory/lock metadata only until Pi creates
    /// its first session file.
    pub provisional: bool,
}

/// Host response to a REGISTER request.  This is local bridge protocol data;
/// it is never forwarded as a `pix-wire` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiBridgeRegisterResponse {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub session_id: String,
    pub granted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_instance_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_nonce: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<TuiBridgeConnectionState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisional: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TuiBridgeRegisterResponse {
    #[must_use]
    pub fn granted(registration: &TuiBridgeRegistration) -> Self {
        Self {
            version: TUI_BRIDGE_PROTOCOL_VERSION,
            message_type: "register_result".to_owned(),
            session_id: registration.token.session_id.to_string(),
            granted: true,
            bridge_instance_id: Some(registration.token.bridge_instance_id),
            generation: Some(registration.token.generation),
            claim_nonce: Some(registration.token.claim_nonce),
            state: Some(registration.state),
            provisional: Some(registration.provisional),
            error: None,
        }
    }

    #[must_use]
    pub fn denied(session_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            version: TUI_BRIDGE_PROTOCOL_VERSION,
            message_type: "register_result".to_owned(),
            session_id: session_id.into(),
            granted: false,
            bridge_instance_id: None,
            generation: None,
            claim_nonce: None,
            state: None,
            provisional: None,
            error: Some(error.into()),
        }
    }
}

/// Encodes a REGISTER response as one JSONL frame.
///
/// # Errors
///
/// Returns [`serde_json::Error`] only if the response schema cannot be
/// serialized, which should not occur for the built-in fields.
pub fn encode_register_response(
    response: &TuiBridgeRegisterResponse,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut frame = serde_json::to_vec(response)?;
    frame.push(b'\n');
    Ok(frame)
}

/// Payload-free view used by `RuntimeManager` and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiBridgeOwnerSnapshot {
    pub token: TuiBridgeToken,
    pub workspace: PathBuf,
    pub state: TuiBridgeConnectionState,
    pub session_state: SessionState,
    pub client_count: usize,
    pub provisional: bool,
}

struct TuiOwner {
    lease: SessionLease,
    workspace: PathBuf,
    state: TuiBridgeConnectionState,
    session_state: SessionState,
    client_count: usize,
    provisional: bool,
}

/// Host-local registry for external TUI ownership.
///
/// The registry owns the `SessionLease` for every known TUI owner.  Dropping
/// the registry unlocks the advisory files but deliberately leaves `PiTui`
/// records on disk, matching the host-restart safety contract.
pub struct TuiBridgeRegistry {
    lock_directory: PathBuf,
    authorized_workspaces: RwLock<HashSet<PathBuf>>,
    expected_peer_uid: RwLock<Option<u32>>,
    owners: Mutex<HashMap<SessionId, TuiOwner>>,
}

impl TuiBridgeRegistry {
    #[must_use]
    pub fn new(lock_directory: impl Into<PathBuf>) -> Self {
        Self {
            lock_directory: lock_directory.into(),
            authorized_workspaces: RwLock::new(HashSet::new()),
            expected_peer_uid: RwLock::new(None),
            owners: Mutex::new(HashMap::new()),
        }
    }

    /// Replaces the current authorization view.  Workspace paths are already
    /// canonicalized by Host configuration; callers that construct an
    /// embedded registry should canonicalize them before this call.
    pub fn configure_authorization(
        &self,
        authorized_workspaces: HashSet<PathBuf>,
        expected_peer_uid: Option<u32>,
    ) {
        let authorized_workspaces = authorized_workspaces
            .into_iter()
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect();
        *self
            .authorized_workspaces
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = authorized_workspaces;
        *self
            .expected_peer_uid
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = expected_peer_uid;
    }

    /// Claims one session for a peer whose credentials were obtained outside
    /// the JSON protocol.  A reconnect from the same PID/start identity is
    /// allowed and rotates the bridge token; a different peer gets conflict.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the frame, peer, workspace, session
    /// hint, or shared ownership gate is invalid.
    pub fn register(
        &self,
        request: &TuiBridgeRegister,
        peer: &TuiBridgePeer,
    ) -> Result<TuiBridgeRegistration, TuiBridgeError> {
        validate_register(request)?;
        self.validate_peer(peer)?;
        let session_id = request
            .session_id
            .parse::<SessionId>()
            .map_err(|_| TuiBridgeError::InvalidSessionId)?;
        let cwd =
            fs::canonicalize(&request.cwd).map_err(|source| TuiBridgeError::Canonicalize {
                path: request.cwd.clone(),
                source,
            })?;
        self.validate_workspace(&cwd)?;
        let provisional = validate_session_hint(session_id, &cwd, request.session_file.as_deref())?;

        // Keep the registry mutex held while replacing a same-owner lease. It
        // prevents two simultaneous reconnects in this Host process from
        // creating a gap in the in-memory owner view; the advisory file lock
        // remains the cross-process authority.
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = owners.get(&session_id)
            && (existing.lease.record().owner_pid != peer.process.pid
                || existing.lease.record().owner_process_start_identity
                    != peer.process.process_start_identity
                || existing.workspace != cwd)
        {
            return Err(TuiBridgeError::OwnerConflict(session_id));
        }
        let previous = owners.remove(&session_id);
        drop(previous);

        let lease = SessionLease::acquire_for_tui(
            &self.lock_directory,
            session_id,
            &cwd,
            &peer.process,
            request.bridge_instance_id,
        )
        .map_err(|error| match error {
            SessionLockError::AlreadyOwned { .. } | SessionLockError::AlreadyOwnedInProcess(_) => {
                TuiBridgeError::OwnerConflict(session_id)
            }
            other => TuiBridgeError::SessionLock(other),
        })?;
        let token = token_from_lease(&lease);
        owners.insert(
            session_id,
            TuiOwner {
                lease,
                workspace: cwd,
                state: TuiBridgeConnectionState::Attached,
                session_state: SessionState::Idle,
                client_count: 0,
                provisional,
            },
        );
        Ok(TuiBridgeRegistration {
            token,
            state: TuiBridgeConnectionState::Attached,
            provisional,
        })
    }

    /// Restores a live `PiTui` owner discovered during Host startup recovery.
    /// The returned lease is held by the registry, so an RPC claim cannot pass
    /// through between recovery and the first bridge reconnect.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the recovered record no longer matches
    /// the workspace or the owner process has exited.
    pub fn restore(
        &self,
        record: &crate::session_lock::SessionLockRecord,
        workspace: &Path,
    ) -> Result<TuiBridgeRegistration, TuiBridgeError> {
        if record.owner_kind != SessionOwnerKind::PiTui {
            return Err(TuiBridgeError::InvalidOwnerKind);
        }
        if record.version != crate::session_lock::SESSION_LOCK_RECORD_VERSION {
            return Err(TuiBridgeError::UnsupportedVersion(record.version));
        }
        let canonical_workspace =
            fs::canonicalize(workspace).map_err(|source| TuiBridgeError::Canonicalize {
                path: workspace.to_path_buf(),
                source,
            })?;
        if record.workspace_fingerprint.as_deref()
            != Some(workspace_fingerprint(&canonical_workspace)?.as_str())
        {
            return Err(TuiBridgeError::WorkspaceFingerprintMismatch(
                record.session_id,
            ));
        }
        self.validate_workspace(&canonical_workspace)?;
        let owner = ProcessIdentity {
            pid: record.owner_pid,
            process_start_identity: record.owner_process_start_identity.clone(),
        };
        let owner_alive =
            ProcessIdentity::inspect(owner.pid)?.is_some_and(|identity| identity == owner);
        if !owner_alive {
            return Err(TuiBridgeError::OwnerNotLive(record.session_id));
        }
        let bridge_instance_id = record.bridge_instance_id.unwrap_or_else(Uuid::new_v4);
        let lease = SessionLease::acquire_for_tui(
            &self.lock_directory,
            record.session_id,
            &canonical_workspace,
            &owner,
            bridge_instance_id,
        )
        .map_err(TuiBridgeError::SessionLock)?;
        let token = token_from_lease(&lease);
        let registration = TuiBridgeRegistration {
            token,
            state: TuiBridgeConnectionState::Unreachable,
            provisional: false,
        };
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if owners.contains_key(&record.session_id) {
            return Err(TuiBridgeError::OwnerConflict(record.session_id));
        }
        owners.insert(
            record.session_id,
            TuiOwner {
                lease,
                workspace: canonical_workspace,
                state: TuiBridgeConnectionState::Unreachable,
                session_state: SessionState::Unavailable,
                client_count: 0,
                provisional: false,
            },
        );
        Ok(registration)
    }

    /// Marks a valid bridge connection unreachable while retaining its lease
    /// and durable owner record.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the token is stale or unknown.
    pub fn disconnect(&self, token: &TuiBridgeToken) -> Result<(), TuiBridgeError> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get_mut(&token.session_id)
            .ok_or(TuiBridgeError::OwnershipTokenMismatch(token.session_id))?;
        ensure_token(owner, token)?;
        owner.state = TuiBridgeConnectionState::Unreachable;
        owner.session_state = SessionState::Unavailable;
        Ok(())
    }

    /// Explicitly releases the current TUI owner.  Repeating a release after
    /// the owner is gone is successful; a stale token can never release a
    /// replacement owner.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the token is stale or the owner record
    /// cannot be removed atomically.
    pub fn release(&self, token: &TuiBridgeToken) -> Result<(), TuiBridgeError> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(mut owner) = owners.remove(&token.session_id) else {
            return Ok(());
        };
        if let Err(error) = ensure_token(&owner, token) {
            owners.insert(token.session_id, owner);
            return Err(error);
        }
        if let Err(error) = owner
            .lease
            .release_external(token.generation, token.claim_nonce)
        {
            owners.insert(token.session_id, owner);
            return Err(TuiBridgeError::SessionLock(error));
        }
        Ok(())
    }

    /// Updates the in-memory state reported by the eventual event adapter.
    /// This does not write conversation or runtime state to disk.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the session is not registered.
    pub fn mark_state(
        &self,
        session_id: SessionId,
        state: SessionState,
    ) -> Result<(), TuiBridgeError> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get_mut(&session_id)
            .ok_or(TuiBridgeError::UnknownSession(session_id))?;
        owner.session_state = state;
        if matches!(state, SessionState::Unavailable) {
            owner.state = TuiBridgeConnectionState::Unreachable;
        }
        Ok(())
    }

    #[must_use]
    pub fn owner(&self, session_id: SessionId) -> Option<TuiBridgeOwnerSnapshot> {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(owner_snapshot)
    }

    #[must_use]
    pub fn contains(&self, session_id: SessionId) -> bool {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&session_id)
    }

    #[must_use]
    pub fn owners(&self) -> Vec<TuiBridgeOwnerSnapshot> {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(owner_snapshot)
            .collect()
    }

    /// Marks a revoked-workspace TUI unavailable without terminating the
    /// user's Pi process. The owner remains conservative until it disconnects
    /// or the process is proven dead by recovery.
    pub fn mark_unavailable_if_workspace_not_authorized(&self, authorized: &HashSet<PathBuf>) {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for owner in owners.values_mut() {
            if !authorized.contains(&owner.workspace) {
                owner.state = TuiBridgeConnectionState::Unreachable;
                owner.session_state = SessionState::Unavailable;
            }
        }
    }

    fn validate_peer(&self, peer: &TuiBridgePeer) -> Result<(), TuiBridgeError> {
        if let Some(expected) = *self
            .expected_peer_uid
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            && expected != peer.uid
        {
            return Err(TuiBridgeError::PeerUserMismatch {
                expected,
                actual: peer.uid,
            });
        }
        let inspected = ProcessIdentity::inspect(peer.process.pid)?
            .ok_or(TuiBridgeError::PeerProcessNotFound(peer.process.pid))?;
        if inspected != peer.process {
            return Err(TuiBridgeError::PeerIdentityMismatch(peer.process.pid));
        }
        Ok(())
    }

    fn validate_workspace(&self, workspace: &Path) -> Result<(), TuiBridgeError> {
        let authorized = self
            .authorized_workspaces
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if authorized.contains(workspace) {
            Ok(())
        } else {
            Err(TuiBridgeError::WorkspaceNotAuthorized)
        }
    }
}

/// A small, UI-free protocol harness used by integration tests and future
/// Unix-socket adapters.  It parses exactly one bounded REGISTER frame and
/// delegates all ownership decisions to [`TuiBridgeRegistry`].
#[derive(Clone)]
pub struct TuiBridgeHarness {
    registry: Arc<TuiBridgeRegistry>,
}

/// A private Unix-domain listener for the host-local bridge.
///
/// The listener only parses the first bounded REGISTER frame.  Ownership is
/// still decided by [`TuiBridgeRegistry::register`], which receives the peer
/// credentials returned by the operating system.  Windows intentionally has
/// no implementation in v1.
pub struct TuiBridgeUnixSocket {
    #[cfg(unix)]
    listener: std::os::unix::net::UnixListener,
    path: PathBuf,
}

#[cfg(unix)]
impl TuiBridgeUnixSocket {
    /// Binds a private non-blocking Unix socket, removing only a stale socket
    /// at the requested path.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] if the parent cannot be secured, a live
    /// listener already exists, or the path is not a socket.
    pub fn bind(path: impl Into<PathBuf>) -> Result<Self, TuiBridgeError> {
        let path = path.into();
        let parent = path.parent().ok_or(TuiBridgeError::SocketPath)?;
        fs::create_dir_all(parent).map_err(TuiBridgeError::Io)?;
        let parent_metadata = fs::symlink_metadata(parent).map_err(TuiBridgeError::Io)?;
        if !parent_metadata.file_type().is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(TuiBridgeError::SocketPath);
        }
        secure_directory(parent)?;
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket() {
                return Err(TuiBridgeError::SocketPath);
            }
            match std::os::unix::net::UnixStream::connect(&path) {
                Ok(_) => return Err(TuiBridgeError::SocketAlreadyOwned),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(&path).map_err(TuiBridgeError::Io)?;
                }
                Err(error) => return Err(TuiBridgeError::Io(error)),
            }
        }
        let listener = std::os::unix::net::UnixListener::bind(&path).map_err(TuiBridgeError::Io)?;
        listener.set_nonblocking(true).map_err(TuiBridgeError::Io)?;
        secure_socket(&path)?;
        Ok(Self { listener, path })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accepts and decodes at most one REGISTER frame without blocking the
    /// caller.  The returned peer identity is kernel-derived and includes the
    /// process start identity inspected by Pix.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] if peer credentials or the frame are invalid.
    pub fn try_accept_register(
        &self,
    ) -> Result<
        Option<(
            TuiBridgePeer,
            TuiBridgeRegister,
            std::os::unix::net::UnixStream,
        )>,
        TuiBridgeError,
    > {
        let (stream, _) = match self.listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(TuiBridgeError::Io(error)),
        };
        let peer = peer_credentials(&stream)?;
        stream.set_nonblocking(false).map_err(TuiBridgeError::Io)?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .map_err(TuiBridgeError::Io)?;
        let mut reader = BufReader::new(stream);
        let mut frame = Vec::new();
        reader
            .by_ref()
            .take(u64::try_from(TUI_BRIDGE_MAX_FRAME_BYTES).unwrap_or(u64::MAX) + 1)
            .read_until(b'\n', &mut frame)
            .map_err(TuiBridgeError::Io)?;
        if frame.len() > TUI_BRIDGE_MAX_FRAME_BYTES {
            return Err(TuiBridgeError::FrameTooLarge);
        }
        if frame.last() != Some(&b'\n') {
            return Err(TuiBridgeError::MalformedFrame);
        }
        let register = decode_register_frame(&frame)?;
        Ok(Some((peer, register, reader.into_inner())))
    }
}

#[cfg(not(unix))]
impl TuiBridgeUnixSocket {
    /// Returns an unsupported-platform error on non-Unix hosts.
    pub fn bind(path: impl Into<PathBuf>) -> Result<Self, TuiBridgeError> {
        let _ = path.into();
        Err(TuiBridgeError::UnsupportedPlatform)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for TuiBridgeUnixSocket {
    fn drop(&mut self) {
        use std::os::unix::fs::FileTypeExt;
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl TuiBridgeHarness {
    #[must_use]
    pub fn new(registry: Arc<TuiBridgeRegistry>) -> Self {
        Self { registry }
    }

    /// Parses a bounded frame and performs the corresponding ownership claim.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] for an invalid frame, peer, workspace,
    /// session hint, or ownership conflict.
    pub fn register_frame(
        &self,
        frame: &[u8],
        peer: &TuiBridgePeer,
    ) -> Result<TuiBridgeRegistration, TuiBridgeError> {
        let request = decode_register_frame(frame)?;
        self.registry.register(&request, peer)
    }

    /// Marks a registered bridge connection unreachable without releasing its
    /// ownership lease.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] for an unknown or stale token.
    pub fn disconnect(&self, token: &TuiBridgeToken) -> Result<(), TuiBridgeError> {
        self.registry.disconnect(token)
    }

    /// Explicitly releases a registered bridge owner.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] for an unknown or stale token or a failed
    /// durable record update.
    pub fn release(&self, token: &TuiBridgeToken) -> Result<(), TuiBridgeError> {
        self.registry.release(token)
    }

    #[must_use]
    pub fn registry(&self) -> &Arc<TuiBridgeRegistry> {
        &self.registry
    }
}

/// Decodes one bounded JSONL REGISTER frame.  The frame must represent the
/// `register` message type; PIDs and UIDs are intentionally not accepted as
/// payload fields.
///
/// # Errors
///
/// Returns [`TuiBridgeError`] for oversized, malformed, or non-REGISTER
/// frames.
pub fn decode_register_frame(frame: &[u8]) -> Result<TuiBridgeRegister, TuiBridgeError> {
    if frame.len() > TUI_BRIDGE_MAX_FRAME_BYTES {
        return Err(TuiBridgeError::FrameTooLarge);
    }
    let request = serde_json::from_slice::<TuiBridgeRegister>(frame)
        .map_err(|_| TuiBridgeError::MalformedFrame)?;
    validate_register(&request)?;
    Ok(request)
}

/// Returns the owner UID for a trusted config/run directory.  This is used by
/// the host to configure the peer-credential check without an unsafe FFI call
/// in the protocol layer.  The eventual Unix transport still has to provide
/// the peer UID itself.
#[must_use]
pub fn owner_uid(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|metadata| metadata.uid())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(unix)]
fn peer_credentials(
    stream: &std::os::unix::net::UnixStream,
) -> Result<TuiBridgePeer, TuiBridgeError> {
    let (uid, pid) = {
        #[cfg(target_os = "linux")]
        {
            use nix::sys::socket::{getsockopt, sockopt};
            let credentials = getsockopt(stream, sockopt::PeerCredentials)
                .map_err(|error| TuiBridgeError::PeerCredentials(error.to_string()))?;
            (
                u32::try_from(credentials.uid())
                    .map_err(|_| TuiBridgeError::PeerCredentials("invalid UID".to_owned()))?,
                u32::try_from(credentials.pid())
                    .map_err(|_| TuiBridgeError::PeerCredentials("invalid PID".to_owned()))?,
            )
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            use nix::sys::socket::{getsockopt, sockopt};
            use nix::unistd::getpeereid;
            let (uid, _) = getpeereid(stream)
                .map_err(|error| TuiBridgeError::PeerCredentials(error.to_string()))?;
            let pid = getsockopt(stream, sockopt::LocalPeerPid)
                .map_err(|error| TuiBridgeError::PeerCredentials(error.to_string()))?;
            (
                uid.as_raw(),
                u32::try_from(pid)
                    .map_err(|_| TuiBridgeError::PeerCredentials("invalid PID".to_owned()))?,
            )
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
        {
            return Err(TuiBridgeError::UnsupportedPlatform);
        }
    };
    let process = ProcessIdentity::inspect(pid)?.ok_or(TuiBridgeError::PeerProcessNotFound(pid))?;
    Ok(TuiBridgePeer { uid, process })
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<(), TuiBridgeError> {
    use nix::unistd::Uid;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).map_err(TuiBridgeError::Io)?;
    if !metadata.file_type().is_dir() || metadata.uid() != Uid::effective().as_raw() {
        return Err(TuiBridgeError::SocketPath);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(TuiBridgeError::Io)
}

#[cfg(unix)]
fn secure_socket(path: &Path) -> Result<(), TuiBridgeError> {
    use nix::unistd::Uid;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).map_err(TuiBridgeError::Io)?;
    if !metadata.file_type().is_socket() || metadata.uid() != Uid::effective().as_raw() {
        return Err(TuiBridgeError::SocketPath);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(TuiBridgeError::Io)
}

fn validate_register(request: &TuiBridgeRegister) -> Result<(), TuiBridgeError> {
    if request.version != TUI_BRIDGE_PROTOCOL_VERSION {
        return Err(TuiBridgeError::UnsupportedVersion(request.version));
    }
    if request.message_type != REGISTER_MESSAGE_TYPE {
        return Err(TuiBridgeError::InvalidMessageType);
    }
    if request.extension_version == 0 {
        return Err(TuiBridgeError::InvalidExtensionVersion);
    }
    Ok(())
}

fn validate_session_hint(
    session_id: SessionId,
    workspace: &Path,
    session_file: Option<&Path>,
) -> Result<bool, TuiBridgeError> {
    let store = PiSessionStore::for_workspace(workspace)?;
    match store.find(session_id) {
        Ok(session) => {
            if let Some(expected) = session_file {
                let actual = fs::canonicalize(&session.path).map_err(|source| {
                    TuiBridgeError::Canonicalize {
                        path: session.path.clone(),
                        source,
                    }
                })?;
                let requested =
                    fs::canonicalize(expected).map_err(|source| TuiBridgeError::Canonicalize {
                        path: expected.to_path_buf(),
                        source,
                    })?;
                if actual != requested {
                    return Err(TuiBridgeError::SessionFileMismatch(session_id));
                }
            }
            Ok(false)
        }
        Err(SessionError::NotFound(_)) if session_file.is_none() => Ok(true),
        Err(SessionError::NotFound(_)) => Err(TuiBridgeError::SessionFileMismatch(session_id)),
        Err(error) => Err(TuiBridgeError::Session(error)),
    }
}

fn token_from_lease(lease: &SessionLease) -> TuiBridgeToken {
    let record = lease.record();
    TuiBridgeToken {
        session_id: record.session_id,
        bridge_instance_id: record.bridge_instance_id.unwrap_or_default(),
        owner: ProcessIdentity {
            pid: record.owner_pid,
            process_start_identity: record.owner_process_start_identity.clone(),
        },
        generation: record.generation,
        claim_nonce: record.claim_nonce,
    }
}

fn ensure_token(owner: &TuiOwner, token: &TuiBridgeToken) -> Result<(), TuiBridgeError> {
    let current = token_from_lease(&owner.lease);
    if current == *token {
        Ok(())
    } else {
        Err(TuiBridgeError::OwnershipTokenMismatch(token.session_id))
    }
}

fn owner_snapshot(owner: &TuiOwner) -> TuiBridgeOwnerSnapshot {
    TuiBridgeOwnerSnapshot {
        token: token_from_lease(&owner.lease),
        workspace: owner.workspace.clone(),
        state: owner.state,
        session_state: owner.session_state,
        client_count: owner.client_count,
        provisional: owner.provisional,
    }
}

#[derive(Debug, Error)]
pub enum TuiBridgeError {
    #[error("unsupported TUI bridge protocol version {0}")]
    UnsupportedVersion(u32),
    #[error("TUI bridge message is not REGISTER")]
    InvalidMessageType,
    #[error("TUI bridge extension version is invalid")]
    InvalidExtensionVersion,
    #[error("TUI bridge frame is malformed")]
    MalformedFrame,
    #[error("TUI bridge frame exceeds the 16 MiB limit")]
    FrameTooLarge,
    #[error("TUI bridge session ID is invalid")]
    InvalidSessionId,
    #[error("TUI bridge peer user does not match the Pix user")]
    PeerUserMismatch { expected: u32, actual: u32 },
    #[error("TUI bridge workspace is not authorized")]
    WorkspaceNotAuthorized,
    #[error("TUI bridge workspace cannot be canonicalized: {path}: {source}")]
    Canonicalize { path: PathBuf, source: io::Error },
    #[error("TUI bridge session file does not match the session ID")]
    SessionFileMismatch(SessionId),
    #[error("TUI bridge session is already owned")]
    OwnerConflict(SessionId),
    #[error("TUI bridge owner token does not match the current owner")]
    OwnershipTokenMismatch(SessionId),
    #[error("TUI bridge owner process is no longer live")]
    OwnerNotLive(SessionId),
    #[error("TUI bridge peer process could not be inspected: {0}")]
    PeerProcessNotFound(u32),
    #[error("TUI bridge peer process start identity does not match PID")]
    PeerIdentityMismatch(u32),
    #[error("TUI bridge peer credentials could not be read: {0}")]
    PeerCredentials(String),
    #[error("TUI bridge socket path is invalid or is not a socket")]
    SocketPath,
    #[error("TUI bridge socket is already owned")]
    SocketAlreadyOwned,
    #[error("TUI bridge is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("TUI bridge socket I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("TUI bridge session is unknown")]
    UnknownSession(SessionId),
    #[error("TUI bridge record is not a PiTui owner")]
    InvalidOwnerKind,
    #[error("TUI bridge owner workspace fingerprint does not match")]
    WorkspaceFingerprintMismatch(SessionId),
    #[error(transparent)]
    SessionLock(#[from] SessionLockError),
    #[error(transparent)]
    Session(#[from] SessionError),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::{
        TUI_BRIDGE_MAX_FRAME_BYTES, TUI_BRIDGE_PROTOCOL_VERSION, TuiBridgeConnectionState,
        TuiBridgeError, TuiBridgeHarness, TuiBridgePeer, TuiBridgeRegister, TuiBridgeRegistry,
        decode_register_frame, owner_uid,
    };
    use crate::session_lock::{ProcessIdentity, SessionId};

    fn setup() -> (
        tempfile::TempDir,
        Arc<TuiBridgeRegistry>,
        TuiBridgePeer,
        SessionId,
    ) {
        let locks = tempdir().expect("lock dir");
        let workspace = tempdir().expect("workspace");
        let registry = Arc::new(TuiBridgeRegistry::new(locks.path()));
        let mut authorized = std::collections::HashSet::new();
        authorized.insert(workspace.path().to_path_buf());
        registry.configure_authorization(authorized, owner_uid(workspace.path()));
        let process = ProcessIdentity::current().expect("current process");
        let peer = TuiBridgePeer::new(owner_uid(workspace.path()).unwrap_or_default(), process);
        (workspace, registry, peer, SessionId::new())
    }

    #[test]
    fn register_frame_claims_provisional_owner_without_trusting_payload_pid() {
        let (workspace, registry, peer, session_id) = setup();
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let request = TuiBridgeRegister::new(session_id, workspace.path(), uuid::Uuid::new_v4());
        let mut payload = serde_json::to_value(&request).expect("register payload");
        payload["pid"] = serde_json::json!(u32::MAX);
        let frame = serde_json::to_vec(&payload).expect("register frame");
        let registration = harness.register_frame(&frame, &peer).expect("claim");
        assert!(registration.provisional);
        assert_eq!(registration.state, TuiBridgeConnectionState::Attached);
        assert_eq!(registration.token.owner, peer.process);
    }

    #[test]
    fn disconnect_retains_owner_and_same_peer_reconnect_rotates_token() {
        let (workspace, registry, peer, session_id) = setup();
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let first_frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            uuid::Uuid::new_v4(),
        ))
        .expect("frame");
        let first = harness
            .register_frame(&first_frame, &peer)
            .expect("first claim");
        harness.disconnect(&first.token).expect("disconnect");
        assert_eq!(
            registry.owner(session_id).expect("owner").state,
            TuiBridgeConnectionState::Unreachable
        );
        let second_frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            uuid::Uuid::new_v4(),
        ))
        .expect("frame");
        let second = harness
            .register_frame(&second_frame, &peer)
            .expect("reconnect");
        assert!(second.token.generation > first.token.generation);
        assert_ne!(second.token.claim_nonce, first.token.claim_nonce);
        assert!(matches!(
            harness.disconnect(&first.token),
            Err(TuiBridgeError::OwnershipTokenMismatch(_))
        ));
    }

    #[test]
    fn different_peer_and_wrong_uid_cannot_replace_owner() {
        let (workspace, registry, peer, session_id) = setup();
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            uuid::Uuid::new_v4(),
        ))
        .expect("frame");
        harness.register_frame(&frame, &peer).expect("first claim");
        let wrong_uid = TuiBridgePeer::new(peer.uid.saturating_add(1), peer.process.clone());
        assert!(matches!(
            harness.register_frame(&frame, &wrong_uid),
            Err(TuiBridgeError::PeerUserMismatch { .. })
        ));
        let other_process = TuiBridgePeer::new(
            peer.uid,
            ProcessIdentity {
                pid: peer.process.pid,
                process_start_identity: "different".to_owned(),
            },
        );
        assert!(matches!(
            harness.register_frame(&frame, &other_process),
            Err(TuiBridgeError::PeerIdentityMismatch(_))
        ));
    }

    #[test]
    fn stale_token_cannot_release_replacement_and_current_release_is_idempotent() {
        let (workspace, registry, peer, session_id) = setup();
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let first_frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            uuid::Uuid::new_v4(),
        ))
        .expect("frame");
        let first = harness
            .register_frame(&first_frame, &peer)
            .expect("first claim");
        let second_frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            uuid::Uuid::new_v4(),
        ))
        .expect("frame");
        let second = harness
            .register_frame(&second_frame, &peer)
            .expect("replacement");
        assert!(matches!(
            harness.release(&first.token),
            Err(TuiBridgeError::OwnershipTokenMismatch(_))
        ));
        harness.release(&second.token).expect("release");
        harness.release(&second.token).expect("idempotent release");
        assert!(registry.owner(session_id).is_none());
    }

    #[test]
    fn decoder_rejects_wrong_type_and_oversized_frame() {
        let session_id = SessionId::new();
        let mut request = TuiBridgeRegister::new(session_id, "/tmp", uuid::Uuid::new_v4());
        request.message_type = "event".to_owned();
        assert!(matches!(
            decode_register_frame(&serde_json::to_vec(&request).expect("frame")),
            Err(TuiBridgeError::InvalidMessageType)
        ));
        assert!(matches!(
            decode_register_frame(&vec![b'x'; TUI_BRIDGE_MAX_FRAME_BYTES + 1]),
            Err(TuiBridgeError::FrameTooLarge)
        ));
        assert_eq!(TUI_BRIDGE_PROTOCOL_VERSION, 1);
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_returns_kernel_peer_identity_before_register() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixStream;
        use std::sync::mpsc;
        use std::thread;

        let (workspace, registry, _peer, session_id) = setup();
        let socket_directory = tempdir().expect("socket dir");
        let socket_path = socket_directory.path().join("tui-bridge.sock");
        let listener = super::TuiBridgeUnixSocket::bind(&socket_path).expect("bind socket");
        assert_eq!(
            fs::metadata(&socket_path)
                .expect("socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            uuid::Uuid::new_v4(),
        ))
        .expect("frame");
        let (ready_sender, ready_receiver) = mpsc::channel();
        let client_path = socket_path.clone();
        let client = thread::spawn(move || {
            let mut stream = UnixStream::connect(client_path).expect("connect socket");
            stream.write_all(&frame).expect("write frame");
            stream.write_all(b"\n").expect("write newline");
            ready_receiver.recv().expect("server read");
        });
        let incoming = loop {
            if let Some(incoming) = listener.try_accept_register().expect("accept register") {
                break incoming;
            }
            thread::yield_now();
        };
        ready_sender.send(()).expect("client release");
        client.join().expect("client");
        assert_eq!(
            incoming.0.uid,
            owner_uid(workspace.path()).expect("owner UID")
        );
        assert_eq!(
            incoming.0.process,
            ProcessIdentity::current().expect("current identity")
        );
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let registration = harness
            .registry()
            .register(&incoming.1, &incoming.0)
            .expect("register accepted peer");
        harness.release(&registration.token).expect("release");
    }

    #[test]
    fn session_file_hint_must_match_discovered_file() {
        let (workspace, registry, peer, session_id) = setup();
        let sessions = workspace.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        // The default Pi session directory is intentionally not overridden in
        // this unit test; a path hint for an undiscoverable file must fail
        // closed rather than silently becoming provisional.
        let mut request =
            TuiBridgeRegister::new(session_id, workspace.path(), uuid::Uuid::new_v4());
        request.session_file = Some(sessions.join("missing.jsonl"));
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        assert!(matches!(
            harness.register_frame(&serde_json::to_vec(&request).expect("frame"), &peer),
            Err(TuiBridgeError::SessionFileMismatch(_))
        ));
    }
}
