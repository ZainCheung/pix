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
use std::sync::{Arc, Mutex, RwLock, atomic::AtomicBool, mpsc};
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::pi_rpc::PiEvent;
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
const EVENT_MESSAGE_TYPE: &str = "event";
const REQUEST_MESSAGE_TYPE: &str = "request";
const RESPONSE_MESSAGE_TYPE: &str = "response";
const SNAPSHOT_COMMAND: &str = "snapshot";
pub(crate) const TUI_BRIDGE_OUTBOUND_QUEUE: usize = 32;
const TUI_BRIDGE_MAX_PENDING_REQUESTS: usize = 64;
pub(crate) const TUI_BRIDGE_RELEASE_EVENT: &str = "session_release";
const PRECLAIM_MESSAGE_TYPE: &str = "preclaim";
pub(crate) const PRECLAIM_RESULT_MESSAGE_TYPE: &str = "preclaim_result";
const TUI_BRIDGE_PRECLAIM_TTL: Duration = Duration::from_secs(5);

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
            capabilities: vec![
                "events.v1".to_owned(),
                "snapshot.v1".to_owned(),
                "commands.v1".to_owned(),
            ],
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

/// One bounded, sequenced event emitted by the Pi TUI extension after a
/// successful ownership claim. The payload keeps Pi-specific fields inside
/// the host compatibility boundary; the existing `pi_bridge` adapter maps it
/// before anything reaches `pix-wire`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiBridgeEventFrame {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub session_id: String,
    /// Identifies one bridge stream. Pi creates a fresh value for every
    /// REGISTER/reconnect so sequence numbers may safely restart at one.
    pub stream_epoch: Uuid,
    pub sequence: u64,
    pub event_type: String,
    pub payload: serde_json::Value,
}

impl TuiBridgeEventFrame {
    #[must_use]
    pub fn new(
        session_id: SessionId,
        stream_epoch: Uuid,
        sequence: u64,
        event_type: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            version: TUI_BRIDGE_PROTOCOL_VERSION,
            message_type: EVENT_MESSAGE_TYPE.to_owned(),
            session_id: session_id.to_string(),
            stream_epoch,
            sequence,
            event_type: event_type.into(),
            payload,
        }
    }
}

/// Host-to-extension request used for snapshot handoff and the bounded TUI
/// command subset. The optional payload is command-specific and remains local
/// to the bridge socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiBridgeRequestFrame {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub request_id: Uuid,
    pub session_id: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

impl TuiBridgeRequestFrame {
    #[must_use]
    pub fn command(
        session_id: SessionId,
        request_id: Uuid,
        command: impl Into<String>,
        payload: Option<serde_json::Value>,
    ) -> Self {
        Self {
            version: TUI_BRIDGE_PROTOCOL_VERSION,
            message_type: REQUEST_MESSAGE_TYPE.to_owned(),
            request_id,
            session_id: session_id.to_string(),
            command: command.into(),
            payload,
        }
    }

    #[must_use]
    pub fn snapshot(session_id: SessionId, request_id: Uuid) -> Self {
        Self::command(session_id, request_id, SNAPSHOT_COMMAND, None)
    }
}

/// Snapshot response returned by the extension. The shape deliberately uses
/// Pi's compatibility fields; the host converts it before it reaches the
/// stable wire snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiBridgeSnapshot {
    pub session_id: String,
    pub session_name: Option<String>,
    pub model: Option<serde_json::Value>,
    pub thinking_level: String,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub pending_message_count: usize,
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub inflight_assistant: Option<serde_json::Value>,
    #[serde(default)]
    pub active_tools: Vec<serde_json::Value>,
    pub through_sequence: u64,
}

/// Correlated host-local response for a bridge request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiBridgeResponseFrame {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub request_id: Uuid,
    pub session_id: String,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub snapshot: Option<TuiBridgeSnapshot>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Extension-to-host request used to reserve a destination session before a
/// Pi TUI `/resume` switch releases the current owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TuiBridgePreclaimFrame {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub request_id: Uuid,
    pub bridge_instance_id: Uuid,
    pub target_session_file: String,
}

/// Host response to a preclaim request. This remains on the local bridge
/// socket and is never forwarded through `pix-wire`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TuiBridgePreclaimResponseFrame {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    pub request_id: Uuid,
    pub allowed: bool,
    #[serde(default)]
    pub bridge_instance_id: Option<Uuid>,
    #[serde(default)]
    pub error: Option<String>,
}

pub(crate) struct TuiBridgePreclaimDecision {
    pub allowed: bool,
    pub bridge_instance_id: Option<Uuid>,
    pub error: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum TuiBridgeInboundFrame {
    Event(TuiBridgeEventFrame),
    Response(Box<TuiBridgeResponseFrame>),
    Preclaim(TuiBridgePreclaimFrame),
}

struct PendingTuiBridgeRequest {
    command: String,
    sender: mpsc::SyncSender<TuiBridgeResponseFrame>,
}

struct TuiPreclaim {
    lease: SessionLease,
    workspace: PathBuf,
    owner: ProcessIdentity,
    bridge_instance_id: Uuid,
    created_at: Instant,
}

pub(crate) struct TuiBridgeBroker {
    session_id: SessionId,
    outbound: mpsc::SyncSender<Vec<u8>>,
    pending: Mutex<HashMap<Uuid, PendingTuiBridgeRequest>>,
    closed: AtomicBool,
}

impl TuiBridgeBroker {
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn enqueue(&self, frame: Vec<u8>) -> Result<(), mpsc::TrySendError<Vec<u8>>> {
        self.outbound.try_send(frame)
    }

    pub(crate) fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        let pending = std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for (request_id, pending) in pending {
            let _ = pending.sender.send(TuiBridgeResponseFrame {
                version: TUI_BRIDGE_PROTOCOL_VERSION,
                message_type: RESPONSE_MESSAGE_TYPE.to_owned(),
                request_id,
                session_id: self.session_id.to_string(),
                command: pending.command,
                success: false,
                snapshot: None,
                result: None,
                error: Some("bridge_disconnected".to_owned()),
            });
        }
    }
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
    /// Highest event sequence accepted by the host for this owner.
    pub through_sequence: u64,
}

struct TuiOwner {
    lease: SessionLease,
    workspace: PathBuf,
    state: TuiBridgeConnectionState,
    session_state: SessionState,
    client_count: usize,
    provisional: bool,
    last_sequence: u64,
    subscribers: Vec<mpsc::Sender<PiEvent>>,
    broker: Option<Arc<TuiBridgeBroker>>,
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
    preclaims: Mutex<HashMap<SessionId, TuiPreclaim>>,
}

impl Drop for TuiBridgeRegistry {
    fn drop(&mut self) {
        let preclaims = self
            .preclaims
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, mut preclaim) in std::mem::take(preclaims) {
            let (generation, claim_nonce) = {
                let record = preclaim.lease.record();
                (record.generation, record.claim_nonce)
            };
            let _ = preclaim.lease.release_external(generation, claim_nonce);
        }
    }
}

impl TuiBridgeRegistry {
    #[must_use]
    pub fn new(lock_directory: impl Into<PathBuf>) -> Self {
        Self {
            lock_directory: lock_directory.into(),
            authorized_workspaces: RwLock::new(HashSet::new()),
            expected_peer_uid: RwLock::new(None),
            owners: Mutex::new(HashMap::new()),
            preclaims: Mutex::new(HashMap::new()),
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
            .collect::<HashSet<_>>();
        self.refresh_authorized_workspaces(&authorized_workspaces);
        *self
            .expected_peer_uid
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = expected_peer_uid;
    }

    /// Replaces the authorized workspace view without changing the expected
    /// peer UID.  Host configuration refreshes use this narrower update so a
    /// workspace added while the service is running becomes eligible for a
    /// new REGISTER without reopening the ownership trust boundary.
    ///
    /// Paths are canonicalized defensively; entries that no longer resolve
    /// are omitted from the live authorization view.
    pub fn refresh_authorized_workspaces(&self, authorized_workspaces: &HashSet<PathBuf>) {
        let authorized_workspaces = authorized_workspaces
            .iter()
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect::<HashSet<_>>();
        *self
            .authorized_workspaces
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = authorized_workspaces;
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
        self.expire_preclaims();
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
        let mut reserved = {
            let mut preclaims = self
                .preclaims
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match preclaims.get(&session_id) {
                Some(preclaim) if preclaim.owner == peer.process && preclaim.workspace == cwd => {
                    preclaims.remove(&session_id)
                }
                Some(_) => return Err(TuiBridgeError::OwnerConflict(session_id)),
                None => None,
            }
        };
        if let Some(preclaim) = reserved.as_mut() {
            preclaim
                .lease
                .clear_preclaim_expiry()
                .map_err(TuiBridgeError::SessionLock)?;
        }
        if let Some(mut previous) = owners.remove(&session_id) {
            if let Some(broker) = previous.broker.take() {
                broker.close();
            }
            for subscriber in previous.subscribers.drain(..) {
                let _ = subscriber.send(PiEvent::Closed);
            }
        }

        let lease = if let Some(reserved) = reserved {
            reserved.lease
        } else {
            SessionLease::acquire_for_tui(
                &self.lock_directory,
                session_id,
                &cwd,
                &peer.process,
                request.bridge_instance_id,
            )
            .map_err(|error| match error {
                SessionLockError::AlreadyOwned { .. }
                | SessionLockError::AlreadyOwnedInProcess(_) => {
                    TuiBridgeError::OwnerConflict(session_id)
                }
                other => TuiBridgeError::SessionLock(other),
            })?
        };
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
                last_sequence: 0,
                subscribers: Vec::new(),
                broker: None,
            },
        );
        Ok(TuiBridgeRegistration {
            token,
            state: TuiBridgeConnectionState::Attached,
            provisional,
        })
    }

    fn expire_preclaims(&self) {
        let expired = {
            let mut preclaims = self
                .preclaims
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let ids = preclaims
                .iter()
                .filter(|(_, preclaim)| {
                    now.duration_since(preclaim.created_at) >= TUI_BRIDGE_PRECLAIM_TTL
                })
                .map(|(session_id, _)| *session_id)
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|session_id| preclaims.remove(&session_id))
                .collect::<Vec<_>>()
        };
        for mut preclaim in expired {
            let (generation, claim_nonce) = {
                let record = preclaim.lease.record();
                (record.generation, record.claim_nonce)
            };
            let _ = preclaim.lease.release_external(generation, claim_nonce);
        }

        // A preclaim restored during Host startup is represented as an
        // unreachable owner until the TUI reconnects.  Keep the durable TTL
        // effective for that recovered in-memory owner as well.
        let expired_owner_ids = {
            let owners = self
                .owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Utc::now();
            owners
                .iter()
                .filter(|(_, owner)| {
                    owner
                        .lease
                        .record()
                        .preclaim_expires_at
                        .is_some_and(|expires_at| expires_at <= now)
                })
                .map(|(session_id, _)| *session_id)
                .collect::<Vec<_>>()
        };
        for session_id in expired_owner_ids {
            let mut owners = self
                .owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(owner) = owners.get_mut(&session_id) else {
                continue;
            };
            let Some(expires_at) = owner.lease.record().preclaim_expires_at else {
                // The owner may have been replaced after the candidate scan;
                // never let an old expiry snapshot release a fresh owner.
                continue;
            };
            if expires_at > Utc::now() {
                // The owner may have been replaced after the candidate scan;
                // never let an old expiry snapshot release a fresh owner.
                continue;
            }
            let (generation, claim_nonce) = {
                let record = owner.lease.record();
                (record.generation, record.claim_nonce)
            };
            if owner
                .lease
                .release_external(generation, claim_nonce)
                .is_err()
            {
                // Keep the owner as a safety barrier if durable cleanup could
                // not prove that this exact preclaim still owns the sidecar.
                continue;
            }
            let Some(mut removed) = owners.remove(&session_id) else {
                continue;
            };
            if let Some(broker) = removed.broker.take() {
                broker.close();
            }
            for subscriber in removed.subscribers.drain(..) {
                let _ = subscriber.send(PiEvent::Closed);
            }
        }
    }

    /// Checks and reserves a destination session for a TUI `/resume` switch.
    /// The reservation holds the normal external session lease for a short
    /// bounded window, preventing an RPC runtime from winning the gap between
    /// the pre-switch check and the new REGISTER.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn preclaim(
        &self,
        token: &TuiBridgeToken,
        target_session_file: &Path,
        bridge_instance_id: Uuid,
    ) -> Result<TuiBridgePreclaimDecision, TuiBridgeError> {
        self.expire_preclaims();
        if bridge_instance_id.is_nil() {
            return Err(TuiBridgeError::InvalidPreclaimId);
        }
        let (workspace, owner_identity) = {
            let owners = self
                .owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let owner = owners
                .get(&token.session_id)
                .ok_or(TuiBridgeError::OwnershipTokenMismatch(token.session_id))?;
            ensure_token(owner, token)?;
            if owner.state != TuiBridgeConnectionState::Attached {
                return Err(TuiBridgeError::BridgeUnreachable(token.session_id));
            }
            (owner.workspace.clone(), token.owner.clone())
        };
        let Ok(target_path) = fs::canonicalize(target_session_file) else {
            return Ok(TuiBridgePreclaimDecision {
                allowed: false,
                bridge_instance_id: None,
                error: Some("session_not_found"),
            });
        };
        let store = PiSessionStore::for_workspace(&workspace)?;
        let target = store.list()?.into_iter().find(|session| {
            fs::canonicalize(&session.path)
                .ok()
                .is_some_and(|path| path == target_path)
        });
        let Some(target) = target else {
            return Ok(TuiBridgePreclaimDecision {
                allowed: false,
                bridge_instance_id: None,
                error: Some("session_not_found"),
            });
        };
        if target.summary.id == token.session_id {
            return Ok(TuiBridgePreclaimDecision {
                allowed: true,
                bridge_instance_id: Some(bridge_instance_id),
                error: None,
            });
        }

        let owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = owners
            .get(&token.session_id)
            .ok_or(TuiBridgeError::OwnershipTokenMismatch(token.session_id))?;
        ensure_token(current, token)?;
        if owners.contains_key(&target.summary.id) {
            return Ok(TuiBridgePreclaimDecision {
                allowed: false,
                bridge_instance_id: None,
                error: Some("session_owned"),
            });
        }
        let mut preclaims = self
            .preclaims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = preclaims.get(&target.summary.id) {
            if existing.owner == owner_identity && existing.workspace == workspace {
                return Ok(TuiBridgePreclaimDecision {
                    allowed: true,
                    bridge_instance_id: Some(existing.bridge_instance_id),
                    error: None,
                });
            }
            return Ok(TuiBridgePreclaimDecision {
                allowed: false,
                bridge_instance_id: None,
                error: Some("session_owned"),
            });
        }
        let expires_at = Utc::now() + ChronoDuration::seconds(5);
        let lease = match SessionLease::acquire_for_tui_preclaim(
            &self.lock_directory,
            target.summary.id,
            &workspace,
            &owner_identity,
            bridge_instance_id,
            expires_at,
        ) {
            Ok(lease) => lease,
            Err(
                SessionLockError::AlreadyOwned { .. }
                | SessionLockError::AlreadyOwnedInProcess(_)
                | SessionLockError::UnknownWriter { .. }
                | SessionLockError::ProcessInspection(_)
                | SessionLockError::ProcessIdentityEncoding(_)
                | SessionLockError::UnsupportedPlatform,
            ) => {
                return Ok(TuiBridgePreclaimDecision {
                    allowed: false,
                    bridge_instance_id: None,
                    error: Some("session_owned"),
                });
            }
            Err(error) => return Err(TuiBridgeError::SessionLock(error)),
        };
        preclaims.insert(
            target.summary.id,
            TuiPreclaim {
                lease,
                workspace,
                owner: owner_identity,
                bridge_instance_id,
                created_at: Instant::now(),
            },
        );
        Ok(TuiBridgePreclaimDecision {
            allowed: true,
            bridge_instance_id: Some(bridge_instance_id),
            error: None,
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
        let lease = if let Some(expires_at) = record.preclaim_expires_at {
            SessionLease::acquire_for_tui_preclaim(
                &self.lock_directory,
                record.session_id,
                &canonical_workspace,
                &owner,
                bridge_instance_id,
                expires_at,
            )
        } else {
            SessionLease::acquire_for_tui(
                &self.lock_directory,
                record.session_id,
                &canonical_workspace,
                &owner,
                bridge_instance_id,
            )
        }
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
                last_sequence: 0,
                subscribers: Vec::new(),
                broker: None,
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
        if let Some(broker) = owner.broker.take() {
            broker.close();
        }
        let subscribers = std::mem::take(&mut owner.subscribers);
        for subscriber in subscribers {
            let _ = subscriber.send(PiEvent::Closed);
        }
        Ok(())
    }

    /// Releases TUI owners whose recorded process is no longer live (or whose
    /// PID has been reused). A bridge disconnect alone remains conservative:
    /// only this identity check can turn an unreachable owner into a free
    /// session while the Host stays running.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError::SessionLock`] when process inspection or
    /// durable lease cleanup cannot be completed. In that case the owner is
    /// left in place and remains a safety barrier for RPC claims.
    pub fn reap_dead_tui_owners(&self) -> Result<Vec<SessionId>, TuiBridgeError> {
        // Preclaims are intentionally lazy resources. The normal Host
        // maintenance tick calls this method, so their five-second TTL also
        // takes effect when no subsequent bridge message arrives.
        self.expire_preclaims();
        let candidates = {
            let owners = self
                .owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            owners
                .iter()
                .filter(|(_, owner)| owner.state == TuiBridgeConnectionState::Unreachable)
                .map(|(session_id, owner)| {
                    (
                        *session_id,
                        owner.lease.record().owner_pid,
                        owner.lease.record().owner_process_start_identity.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut reaped = Vec::new();
        for (session_id, owner_pid, owner_start_identity) in candidates {
            let process = ProcessIdentity::inspect(owner_pid)?;
            if process
                .is_some_and(|identity| identity.process_start_identity == owner_start_identity)
            {
                continue;
            }
            let mut owners = self
                .owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(owner) = owners.get_mut(&session_id) else {
                continue;
            };
            let record = owner.lease.record();
            if record.owner_pid != owner_pid
                || record.owner_process_start_identity != owner_start_identity
            {
                continue;
            }
            let (generation, claim_nonce) = (record.generation, record.claim_nonce);
            owner
                .lease
                .release_external(generation, claim_nonce)
                .map_err(TuiBridgeError::SessionLock)?;
            let Some(mut removed) = owners.remove(&session_id) else {
                continue;
            };
            if let Some(broker) = removed.broker.take() {
                broker.close();
            }
            for subscriber in removed.subscribers.drain(..) {
                let _ = subscriber.send(PiEvent::Closed);
            }
            reaped.push(session_id);
        }
        Ok(reaped)
    }

    /// Attaches one remote Pix client to an online TUI owner.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the owner is missing or unreachable.
    pub fn attach_client(&self, session_id: SessionId) -> Result<(), TuiBridgeError> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get_mut(&session_id)
            .ok_or(TuiBridgeError::UnknownSession(session_id))?;
        if owner.state != TuiBridgeConnectionState::Attached {
            return Err(TuiBridgeError::BridgeUnreachable(session_id));
        }
        owner.client_count = owner.client_count.saturating_add(1);
        Ok(())
    }

    /// Detaches one remote Pix client from an online TUI owner.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the owner is missing or has no client.
    pub fn detach_client(&self, session_id: SessionId) -> Result<(), TuiBridgeError> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get_mut(&session_id)
            .ok_or(TuiBridgeError::UnknownSession(session_id))?;
        if owner.client_count == 0 {
            return Err(TuiBridgeError::NoAttachedClient(session_id));
        }
        owner.client_count -= 1;
        Ok(())
    }

    /// Subscribes one remote Pix connection to sequenced TUI events.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the owner is missing or unreachable.
    pub fn subscribe(
        &self,
        session_id: SessionId,
    ) -> Result<mpsc::Receiver<PiEvent>, TuiBridgeError> {
        let (sender, receiver) = mpsc::channel();
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get_mut(&session_id)
            .ok_or(TuiBridgeError::UnknownSession(session_id))?;
        if owner.state != TuiBridgeConnectionState::Attached {
            return Err(TuiBridgeError::BridgeUnreachable(session_id));
        }
        owner.subscribers.push(sender);
        Ok(receiver)
    }

    /// Binds the host-side writer for a successfully registered socket. The
    /// reader remains owned by `HostService`; this broker only queues bounded
    /// host requests and correlates extension responses.
    pub(crate) fn bind_transport(
        &self,
        token: &TuiBridgeToken,
        outbound: mpsc::SyncSender<Vec<u8>>,
    ) -> Result<Arc<TuiBridgeBroker>, TuiBridgeError> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get_mut(&token.session_id)
            .ok_or(TuiBridgeError::OwnershipTokenMismatch(token.session_id))?;
        ensure_token(owner, token)?;
        if owner.state != TuiBridgeConnectionState::Attached {
            return Err(TuiBridgeError::BridgeUnreachable(token.session_id));
        }
        if let Some(previous) = owner.broker.replace(Arc::new(TuiBridgeBroker {
            session_id: token.session_id,
            outbound,
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        })) {
            previous.close();
        }
        Ok(Arc::clone(owner.broker.as_ref().expect("broker inserted")))
    }

    fn attached_broker(
        &self,
        session_id: SessionId,
    ) -> Result<Arc<TuiBridgeBroker>, TuiBridgeError> {
        let owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get(&session_id)
            .ok_or(TuiBridgeError::UnknownSession(session_id))?;
        if owner.state != TuiBridgeConnectionState::Attached {
            return Err(TuiBridgeError::BridgeUnreachable(session_id));
        }
        owner
            .broker
            .clone()
            .ok_or(TuiBridgeError::BridgeUnreachable(session_id))
    }

    /// Sends one bounded command to the attached Pi TUI and waits for its
    /// correlated response. The request is host-local and never enters the
    /// encrypted Pix wire protocol directly.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the bridge is unavailable, the request
    /// queue is full, the response is rejected, or the extension misses the
    /// deadline.
    pub fn request_command(
        &self,
        session_id: SessionId,
        command: impl Into<String>,
        payload: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<TuiBridgeResponseFrame, TuiBridgeError> {
        let command = command.into();
        validate_bridge_text(&command, 128).map_err(|()| TuiBridgeError::InvalidRequestCommand)?;
        let request_id = Uuid::new_v4();
        let (sender, receiver) = mpsc::sync_channel(1);
        let broker = self.attached_broker(session_id)?;
        if broker.is_closed() {
            return Err(TuiBridgeError::BridgeUnreachable(session_id));
        }
        {
            let mut pending = broker
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending.len() >= TUI_BRIDGE_MAX_PENDING_REQUESTS {
                return Err(TuiBridgeError::BridgeBackpressure(session_id));
            }
            pending.insert(
                request_id,
                PendingTuiBridgeRequest {
                    command: command.clone(),
                    sender,
                },
            );
        }
        if broker.is_closed() {
            broker
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&request_id);
            return Err(TuiBridgeError::BridgeUnreachable(session_id));
        }
        let mut frame = match serde_json::to_vec(&TuiBridgeRequestFrame::command(
            session_id,
            request_id,
            command.clone(),
            payload,
        )) {
            Ok(frame) => frame,
            Err(error) => {
                broker
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&request_id);
                return Err(TuiBridgeError::Encode(error));
            }
        };
        frame.push(b'\n');
        match broker.outbound.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                broker
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&request_id);
                return Err(TuiBridgeError::BridgeBackpressure(session_id));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                broker
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&request_id);
                return Err(TuiBridgeError::BridgeUnreachable(session_id));
            }
        }
        let response = match receiver.recv_timeout(timeout) {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                broker
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&request_id);
                return Err(TuiBridgeError::CommandTimeout(session_id));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(TuiBridgeError::BridgeUnreachable(session_id));
            }
        };
        if response.command != command {
            return Err(TuiBridgeError::ResponseCommandMismatch(session_id));
        }
        if !response.success {
            if response.error.as_deref() == Some("bridge_disconnected") {
                return Err(TuiBridgeError::BridgeUnreachable(session_id));
            }
            return Err(TuiBridgeError::CommandRejected(session_id));
        }
        Ok(response)
    }

    /// Requests a bounded authoritative snapshot from the attached Pi TUI.
    /// The returned cursor covers every event queued before the response, so a
    /// caller can discard subscription frames up to that sequence.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] when the bridge is unavailable, the request
    /// queue is full, or the extension misses the deadline.
    pub fn request_snapshot(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> Result<TuiBridgeSnapshot, TuiBridgeError> {
        let response = self
            .request_command(session_id, SNAPSHOT_COMMAND, None, timeout)
            .map_err(|error| match error {
                TuiBridgeError::CommandRejected(session_id) => {
                    TuiBridgeError::SnapshotRejected(session_id)
                }
                TuiBridgeError::CommandTimeout(session_id) => {
                    TuiBridgeError::SnapshotTimeout(session_id)
                }
                other => other,
            })?;
        response
            .snapshot
            .ok_or(TuiBridgeError::InvalidSnapshotResponse(session_id))
    }

    /// Resolves one response received by the socket reader.
    pub(crate) fn resolve_response(
        &self,
        token: &TuiBridgeToken,
        response: TuiBridgeResponseFrame,
    ) -> Result<(), TuiBridgeError> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get_mut(&token.session_id)
            .ok_or(TuiBridgeError::OwnershipTokenMismatch(token.session_id))?;
        ensure_token(owner, token)?;
        if response.session_id != token.session_id.to_string() {
            return Err(TuiBridgeError::ResponseSessionMismatch(token.session_id));
        }
        if response
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.session_id != response.session_id)
        {
            return Err(TuiBridgeError::InvalidSnapshotResponse(token.session_id));
        }
        let broker = owner
            .broker
            .clone()
            .ok_or(TuiBridgeError::BridgeUnreachable(token.session_id))?;
        let sender = broker
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&response.request_id)
            .ok_or(TuiBridgeError::UnknownRequest(response.request_id))?;
        if sender.command != response.command {
            return Err(TuiBridgeError::ResponseCommandMismatch(token.session_id));
        }
        let _ = sender.sender.send(response);
        Ok(())
    }

    /// Compatibility wrapper for callers that only accept snapshot responses.
    #[cfg(test)]
    pub(crate) fn resolve_snapshot_response(
        &self,
        token: &TuiBridgeToken,
        response: TuiBridgeResponseFrame,
    ) -> Result<(), TuiBridgeError> {
        if response.command != SNAPSHOT_COMMAND {
            return Err(TuiBridgeError::InvalidResponseCommand);
        }
        self.resolve_response(token, response)
    }

    /// Accepts one event frame from the registered TUI connection and
    /// broadcasts it to subscribed Pix connections. The sequence is strictly
    /// monotonic per owner; stale or duplicate frames are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] for a stale token, mismatched session, or
    /// non-monotonic sequence.
    pub fn publish_event(
        &self,
        token: &TuiBridgeToken,
        frame: &TuiBridgeEventFrame,
    ) -> Result<usize, TuiBridgeError> {
        let session_id = frame
            .session_id
            .parse::<SessionId>()
            .map_err(|_| TuiBridgeError::InvalidEventSessionId)?;
        if session_id != token.session_id {
            return Err(TuiBridgeError::EventSessionMismatch(token.session_id));
        }
        if frame.stream_epoch != token.bridge_instance_id {
            return Err(TuiBridgeError::EventStreamEpochMismatch(token.session_id));
        }
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners
            .get_mut(&token.session_id)
            .ok_or(TuiBridgeError::OwnershipTokenMismatch(token.session_id))?;
        ensure_token(owner, token)?;
        if owner.state != TuiBridgeConnectionState::Attached {
            return Err(TuiBridgeError::BridgeUnreachable(token.session_id));
        }
        if frame.sequence == 0 || frame.sequence <= owner.last_sequence {
            return Err(TuiBridgeError::EventSequence(token.session_id));
        }
        owner.last_sequence = frame.sequence;
        let event = PiEvent::Event {
            sequence: Some(frame.sequence),
            event_type: frame.event_type.clone(),
            payload: frame.payload.clone(),
        };
        owner
            .subscribers
            .retain(|subscriber| subscriber.send(event.clone()).is_ok());
        Ok(owner.subscribers.len())
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
        if let Some(broker) = owner.broker.take() {
            broker.close();
        }
        for subscriber in owner.subscribers.drain(..) {
            let _ = subscriber.send(PiEvent::Closed);
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
            if let Some(broker) = owner.broker.take() {
                broker.close();
            }
            for subscriber in owner.subscribers.drain(..) {
                let _ = subscriber.send(PiEvent::Closed);
            }
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
        self.expire_preclaims();
        let revoked_preclaims = {
            let mut preclaims = self
                .preclaims
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ids = preclaims
                .iter()
                .filter(|(_, preclaim)| !authorized.contains(&preclaim.workspace))
                .map(|(session_id, _)| *session_id)
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|session_id| preclaims.remove(&session_id))
                .collect::<Vec<_>>()
        };
        for mut preclaim in revoked_preclaims {
            let (generation, claim_nonce) = {
                let record = preclaim.lease.record();
                (record.generation, record.claim_nonce)
            };
            let _ = preclaim.lease.release_external(generation, claim_nonce);
        }
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for owner in owners.values_mut() {
            if !authorized.contains(&owner.workspace) {
                owner.state = TuiBridgeConnectionState::Unreachable;
                owner.session_state = SessionState::Unavailable;
                if let Some(broker) = owner.broker.take() {
                    broker.close();
                }
                for subscriber in owner.subscribers.drain(..) {
                    let _ = subscriber.send(PiEvent::Closed);
                }
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

    /// Decodes and publishes one bounded event frame for a registered owner.
    ///
    /// # Errors
    ///
    /// Returns [`TuiBridgeError`] for an invalid frame, stale token, or
    /// non-monotonic event sequence.
    pub fn publish_event_frame(
        &self,
        frame: &[u8],
        token: &TuiBridgeToken,
    ) -> Result<usize, TuiBridgeError> {
        let event = decode_event_frame(frame)?;
        self.registry.publish_event(token, &event)
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

/// Decodes one bounded JSONL event frame emitted after a successful REGISTER.
/// UIDs, PIDs, and filesystem paths are not accepted as event ownership data.
///
/// # Errors
///
/// Returns [`TuiBridgeError`] for oversized, malformed, or non-event frames.
pub fn decode_event_frame(frame: &[u8]) -> Result<TuiBridgeEventFrame, TuiBridgeError> {
    if frame.len() > TUI_BRIDGE_MAX_FRAME_BYTES {
        return Err(TuiBridgeError::FrameTooLarge);
    }
    let event = serde_json::from_slice::<TuiBridgeEventFrame>(frame)
        .map_err(|_| TuiBridgeError::MalformedFrame)?;
    if event.version != TUI_BRIDGE_PROTOCOL_VERSION {
        return Err(TuiBridgeError::UnsupportedVersion(event.version));
    }
    if event.message_type != EVENT_MESSAGE_TYPE {
        return Err(TuiBridgeError::InvalidEventMessageType);
    }
    if event.sequence == 0 || event.event_type.is_empty() {
        return Err(TuiBridgeError::InvalidEventFrame);
    }
    if event.stream_epoch.is_nil() {
        return Err(TuiBridgeError::InvalidEventStreamEpoch);
    }
    if event.event_type.len() > 128 || event.event_type.chars().any(char::is_control) {
        return Err(TuiBridgeError::InvalidEventFrame);
    }
    if !event.payload.is_object() {
        return Err(TuiBridgeError::InvalidEventPayload);
    }
    event
        .session_id
        .parse::<SessionId>()
        .map_err(|_| TuiBridgeError::InvalidEventSessionId)?;
    Ok(event)
}

/// Decodes one host-local frame after REGISTER. Only event frames and
/// correlated command responses are accepted on the TUI connection.
pub(crate) fn decode_inbound_frame(frame: &[u8]) -> Result<TuiBridgeInboundFrame, TuiBridgeError> {
    if frame.len() > TUI_BRIDGE_MAX_FRAME_BYTES {
        return Err(TuiBridgeError::FrameTooLarge);
    }
    let value = serde_json::from_slice::<serde_json::Value>(frame)
        .map_err(|_| TuiBridgeError::MalformedFrame)?;
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some(EVENT_MESSAGE_TYPE) => Ok(TuiBridgeInboundFrame::Event(decode_event_frame(frame)?)),
        Some(RESPONSE_MESSAGE_TYPE) => Ok(TuiBridgeInboundFrame::Response(Box::new(
            decode_response_frame(frame)?,
        ))),
        Some(PRECLAIM_MESSAGE_TYPE) => Ok(TuiBridgeInboundFrame::Preclaim(decode_preclaim_frame(
            frame,
        )?)),
        _ => Err(TuiBridgeError::InvalidInboundMessageType),
    }
}

/// Decodes and validates one response from the extension.
///
/// # Errors
///
/// Returns [`TuiBridgeError`] when the response is oversized, malformed, or
/// violates the correlated command schema.
pub(crate) fn decode_response_frame(
    frame: &[u8],
) -> Result<TuiBridgeResponseFrame, TuiBridgeError> {
    if frame.len() > TUI_BRIDGE_MAX_FRAME_BYTES {
        return Err(TuiBridgeError::FrameTooLarge);
    }
    let response = serde_json::from_slice::<TuiBridgeResponseFrame>(frame)
        .map_err(|_| TuiBridgeError::MalformedFrame)?;
    if response.version != TUI_BRIDGE_PROTOCOL_VERSION {
        return Err(TuiBridgeError::UnsupportedVersion(response.version));
    }
    if response.message_type != RESPONSE_MESSAGE_TYPE {
        return Err(TuiBridgeError::InvalidResponseMessageType);
    }
    if response.request_id.is_nil() {
        return Err(TuiBridgeError::InvalidRequestId);
    }
    let session_id = response
        .session_id
        .parse::<SessionId>()
        .map_err(|_| TuiBridgeError::InvalidResponseSessionId)?;
    validate_bridge_text(&response.command, 128)
        .map_err(|()| TuiBridgeError::InvalidResponseCommand)?;
    if response.snapshot.is_some() && response.result.is_some() {
        return Err(TuiBridgeError::InvalidCommandResponse(session_id));
    }
    if response.success {
        if response.error.is_some() {
            return Err(TuiBridgeError::InvalidCommandResponse(session_id));
        }
        if response.command == SNAPSHOT_COMMAND {
            let snapshot = response
                .snapshot
                .as_ref()
                .ok_or(TuiBridgeError::InvalidSnapshotResponse(session_id))?;
            validate_snapshot(snapshot)?;
        } else if response.snapshot.is_some() {
            return Err(TuiBridgeError::InvalidCommandResponse(session_id));
        }
    } else {
        if response.snapshot.is_some() || response.result.is_some() {
            return Err(TuiBridgeError::InvalidCommandResponse(session_id));
        }
        let Some(error) = response.error.as_deref() else {
            return Err(TuiBridgeError::InvalidCommandResponse(session_id));
        };
        validate_bridge_text(error, 512)
            .map_err(|()| TuiBridgeError::InvalidCommandResponse(session_id))?;
    }
    Ok(response)
}

fn decode_preclaim_frame(frame: &[u8]) -> Result<TuiBridgePreclaimFrame, TuiBridgeError> {
    if frame.len() > TUI_BRIDGE_MAX_FRAME_BYTES {
        return Err(TuiBridgeError::FrameTooLarge);
    }
    let request = serde_json::from_slice::<TuiBridgePreclaimFrame>(frame)
        .map_err(|_| TuiBridgeError::MalformedFrame)?;
    if request.version != TUI_BRIDGE_PROTOCOL_VERSION {
        return Err(TuiBridgeError::UnsupportedVersion(request.version));
    }
    if request.message_type != PRECLAIM_MESSAGE_TYPE {
        return Err(TuiBridgeError::InvalidPreclaimMessageType);
    }
    if request.request_id.is_nil() || request.bridge_instance_id.is_nil() {
        return Err(TuiBridgeError::InvalidPreclaimId);
    }
    validate_bridge_text(&request.target_session_file, 4096)
        .map_err(|()| TuiBridgeError::InvalidPreclaimPath)?;
    Ok(request)
}

/// Decodes and validates a snapshot response from the extension.
///
/// # Errors
///
/// Returns [`TuiBridgeError`] when the response is malformed, oversized, or
/// does not contain a valid snapshot response.
pub fn decode_snapshot_response(frame: &[u8]) -> Result<TuiBridgeResponseFrame, TuiBridgeError> {
    let response = decode_response_frame(frame)?;
    if response.command != SNAPSHOT_COMMAND {
        return Err(TuiBridgeError::InvalidResponseCommand);
    }
    Ok(response)
}

fn validate_snapshot(snapshot: &TuiBridgeSnapshot) -> Result<(), TuiBridgeError> {
    snapshot
        .session_id
        .parse::<SessionId>()
        .map_err(|_| TuiBridgeError::InvalidSnapshotSessionId)?;
    validate_bridge_text(&snapshot.thinking_level, 32)
        .map_err(|()| TuiBridgeError::InvalidSnapshotPayload)?;
    if !snapshot.messages.iter().all(serde_json::Value::is_object) {
        return Err(TuiBridgeError::InvalidSnapshotPayload);
    }
    if snapshot.active_tools.iter().any(|tool| !tool.is_object()) {
        return Err(TuiBridgeError::InvalidSnapshotPayload);
    }
    if snapshot
        .inflight_assistant
        .as_ref()
        .is_some_and(|message| !message.is_object())
    {
        return Err(TuiBridgeError::InvalidSnapshotPayload);
    }
    if snapshot
        .model
        .as_ref()
        .is_some_and(|model| !model.is_object())
    {
        return Err(TuiBridgeError::InvalidSnapshotPayload);
    }
    Ok(())
}

fn validate_bridge_text(value: &str, max_bytes: usize) -> Result<(), ()> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        Err(())
    } else {
        Ok(())
    }
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
                credentials.uid(),
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
        through_sequence: owner.last_sequence,
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
    #[error("TUI bridge connection is unreachable")]
    BridgeUnreachable(SessionId),
    #[error("TUI bridge session has no attached client")]
    NoAttachedClient(SessionId),
    #[error("TUI bridge event session does not match its owner")]
    EventSessionMismatch(SessionId),
    #[error("TUI bridge event sequence is stale or invalid")]
    EventSequence(SessionId),
    #[error("TUI bridge event stream epoch does not match its owner")]
    EventStreamEpochMismatch(SessionId),
    #[error("TUI bridge event session ID is invalid")]
    InvalidEventSessionId,
    #[error("TUI bridge event stream epoch is invalid")]
    InvalidEventStreamEpoch,
    #[error("TUI bridge message is not an event")]
    InvalidEventMessageType,
    #[error("TUI bridge event frame is invalid")]
    InvalidEventFrame,
    #[error("TUI bridge event payload must be an object")]
    InvalidEventPayload,
    #[error("failed to encode TUI bridge request: {0}")]
    Encode(serde_json::Error),
    #[error("TUI bridge request queue is full")]
    BridgeBackpressure(SessionId),
    #[error("TUI bridge snapshot request timed out")]
    SnapshotTimeout(SessionId),
    #[error("TUI bridge snapshot request was rejected")]
    SnapshotRejected(SessionId),
    #[error("TUI bridge command request timed out")]
    CommandTimeout(SessionId),
    #[error("TUI bridge command request was rejected")]
    CommandRejected(SessionId),
    #[error("TUI bridge snapshot response is invalid")]
    InvalidSnapshotResponse(SessionId),
    #[error("TUI bridge command response is invalid")]
    InvalidCommandResponse(SessionId),
    #[error("TUI bridge snapshot payload is invalid")]
    InvalidSnapshotPayload,
    #[error("TUI bridge response has an unknown request ID")]
    UnknownRequest(Uuid),
    #[error("TUI bridge response session ID is invalid")]
    InvalidResponseSessionId,
    #[error("TUI bridge response session ID does not match its owner")]
    ResponseSessionMismatch(SessionId),
    #[error("TUI bridge response command is invalid")]
    InvalidResponseCommand,
    #[error("TUI bridge response message type is invalid")]
    InvalidResponseMessageType,
    #[error("TUI bridge request ID is invalid")]
    InvalidRequestId,
    #[error("TUI bridge request command is invalid")]
    InvalidRequestCommand,
    #[error("TUI bridge preclaim message type is invalid")]
    InvalidPreclaimMessageType,
    #[error("TUI bridge preclaim request ID is invalid")]
    InvalidPreclaimId,
    #[error("TUI bridge preclaim target path is invalid")]
    InvalidPreclaimPath,
    #[error("TUI bridge inbound message type is invalid")]
    InvalidInboundMessageType,
    #[error("TUI bridge response snapshot session ID is invalid")]
    InvalidSnapshotSessionId,
    #[error("TUI bridge response command does not match the request")]
    ResponseCommandMismatch(SessionId),
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
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{
        TUI_BRIDGE_MAX_FRAME_BYTES, TUI_BRIDGE_PROTOCOL_VERSION, TuiBridgeConnectionState,
        TuiBridgeError, TuiBridgeEventFrame, TuiBridgeHarness, TuiBridgePeer, TuiBridgeRegister,
        TuiBridgeRegistry, TuiBridgeRequestFrame, TuiBridgeResponseFrame, TuiBridgeSnapshot,
        decode_event_frame, decode_inbound_frame, decode_register_frame, decode_snapshot_response,
        owner_uid,
    };
    use crate::pi_rpc::PiEvent;
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
        let receiver = registry
            .subscribe(session_id)
            .expect("subscribe first stream");
        let first_event = TuiBridgeEventFrame::new(
            session_id,
            first.token.bridge_instance_id,
            1,
            "agent_start",
            serde_json::json!({}),
        );
        harness
            .publish_event_frame(
                &serde_json::to_vec(&first_event).expect("first event"),
                &first.token,
            )
            .expect("publish first event");
        assert!(matches!(
            receiver.recv().expect("first event received"),
            PiEvent::Event { .. }
        ));
        harness.disconnect(&first.token).expect("disconnect");
        assert!(matches!(
            receiver.recv().expect("first stream closed"),
            PiEvent::Closed
        ));
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
        let second_receiver = registry
            .subscribe(session_id)
            .expect("subscribe second stream");
        let second_event = TuiBridgeEventFrame::new(
            session_id,
            second.token.bridge_instance_id,
            1,
            "agent_settled",
            serde_json::json!({}),
        );
        harness
            .publish_event_frame(
                &serde_json::to_vec(&second_event).expect("second event"),
                &second.token,
            )
            .expect("publish second event");
        assert!(matches!(
            second_receiver.recv().expect("second event received"),
            PiEvent::Event { event_type, .. } if event_type == "agent_settled"
        ));
        assert!(second.token.generation > first.token.generation);
        assert_ne!(second.token.claim_nonce, first.token.claim_nonce);
        assert!(matches!(
            harness.disconnect(&first.token),
            Err(TuiBridgeError::OwnershipTokenMismatch(_))
        ));
    }

    #[test]
    fn preclaim_reservation_is_consumed_by_same_process_register() {
        let (workspace, registry, peer, current_session_id) = setup();
        let store =
            crate::session::PiSessionStore::for_workspace(workspace.path()).expect("session store");
        let session_directory = store.session_directory().to_path_buf();
        fs::create_dir_all(&session_directory).expect("session directory");
        let target_session_id = SessionId::new();
        let target_path = session_directory.join(format!("preclaim-{target_session_id}.jsonl"));
        let cwd = serde_json::to_string(workspace.path().to_str().expect("workspace UTF-8"))
            .expect("cwd JSON");
        fs::write(
            &target_path,
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{target_session_id}\",\"timestamp\":\"2026-08-12T00:00:00Z\",\"cwd\":{cwd}}}\n"
            ),
        )
        .expect("target session");
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let current = harness
            .register_frame(
                &serde_json::to_vec(&TuiBridgeRegister::new(
                    current_session_id,
                    workspace.path(),
                    Uuid::new_v4(),
                ))
                .expect("current register frame"),
                &peer,
            )
            .expect("current register");
        let reserved_bridge_id = Uuid::new_v4();
        let decision = registry
            .preclaim(&current.token, &target_path, reserved_bridge_id)
            .expect("preclaim target");
        assert!(decision.allowed);
        assert_eq!(decision.bridge_instance_id, Some(reserved_bridge_id));

        let target_occupied = crate::session_lock::SessionLease::acquire_for_workspace(
            &registry.lock_directory,
            target_session_id,
            workspace.path(),
        );
        assert!(matches!(
            target_occupied,
            Err(crate::session_lock::SessionLockError::AlreadyOwned { .. }
                | crate::session_lock::SessionLockError::AlreadyOwnedInProcess(_))
        ));

        let mut target_request =
            TuiBridgeRegister::new(target_session_id, workspace.path(), Uuid::new_v4());
        target_request.session_file = Some(target_path.clone());
        let target = harness
            .register_frame(
                &serde_json::to_vec(&target_request).expect("target register frame"),
                &peer,
            )
            .expect("consume preclaim");
        assert_eq!(target.token.bridge_instance_id, reserved_bridge_id);
        harness.release(&target.token).expect("release target");
        harness.release(&current.token).expect("release current");
        fs::remove_file(&target_path).expect("remove target session");
        let _ = fs::remove_dir(&session_directory);
    }

    #[cfg(unix)]
    #[test]
    fn dead_tui_process_is_reaped_after_socket_disconnect() {
        use std::process::Command;

        let (workspace, registry, _peer, session_id) = setup();
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("spawn TUI stand-in");
        let process = ProcessIdentity::inspect(child.id())
            .expect("inspect TUI stand-in")
            .expect("TUI stand-in is live");
        let peer = TuiBridgePeer::new(
            owner_uid(workspace.path()).expect("workspace owner"),
            process,
        );
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            Uuid::new_v4(),
        ))
        .expect("register frame");
        let registration = harness
            .register_frame(&frame, &peer)
            .expect("register stand-in TUI");
        assert!(registry.contains(session_id));
        harness
            .disconnect(&registration.token)
            .expect("disconnect stand-in socket");

        child.kill().expect("kill TUI stand-in");
        child.wait().expect("wait TUI stand-in");
        let reaped = registry.reap_dead_tui_owners().expect("reap dead TUI");
        assert_eq!(reaped, vec![session_id]);
        assert!(!registry.contains(session_id));
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

    #[test]
    fn event_frames_are_bounded_sequenced_and_broadcast() {
        let (workspace, registry, peer, session_id) = setup();
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let register = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            uuid::Uuid::new_v4(),
        ))
        .expect("register frame");
        let registration = harness
            .register_frame(&register, &peer)
            .expect("register owner");
        let receiver = registry.subscribe(session_id).expect("subscribe events");
        let event = TuiBridgeEventFrame::new(
            session_id,
            registration.token.bridge_instance_id,
            1,
            "agent_start",
            serde_json::json!({}),
        );
        let frame = serde_json::to_vec(&event).expect("event frame");
        assert_eq!(
            harness
                .publish_event_frame(&frame, &registration.token)
                .expect("publish event"),
            1
        );
        assert!(matches!(
            receiver.recv().expect("event"),
            PiEvent::Event { event_type, .. } if event_type == "agent_start"
        ));
        assert!(matches!(
            harness.publish_event_frame(&frame, &registration.token),
            Err(TuiBridgeError::EventSequence(id)) if id == session_id
        ));
        let wrong_session = TuiBridgeEventFrame::new(
            SessionId::new(),
            registration.token.bridge_instance_id,
            2,
            "agent_start",
            serde_json::json!({}),
        );
        let wrong_frame = serde_json::to_vec(&wrong_session).expect("wrong event frame");
        assert!(matches!(
            harness.publish_event_frame(&wrong_frame, &registration.token),
            Err(TuiBridgeError::EventSessionMismatch(id)) if id == session_id
        ));
        let wrong_epoch = TuiBridgeEventFrame::new(
            session_id,
            Uuid::new_v4(),
            2,
            "agent_start",
            serde_json::json!({}),
        );
        assert!(matches!(
            harness.publish_event_frame(
                &serde_json::to_vec(&wrong_epoch).expect("wrong epoch frame"),
                &registration.token,
            ),
            Err(TuiBridgeError::EventStreamEpochMismatch(id)) if id == session_id
        ));
        harness.disconnect(&registration.token).expect("disconnect");
        assert!(matches!(
            receiver.recv().expect("closed event"),
            PiEvent::Closed
        ));
    }

    #[test]
    fn event_decoder_rejects_non_object_payload() {
        let event = TuiBridgeEventFrame::new(
            SessionId::new(),
            Uuid::new_v4(),
            1,
            "agent_start",
            serde_json::json!("not-an-object"),
        );
        assert!(matches!(
            decode_event_frame(&serde_json::to_vec(&event).expect("event frame")),
            Err(TuiBridgeError::InvalidEventPayload)
        ));
    }

    #[test]
    fn snapshot_response_decoder_validates_cursor_and_payload_shape() {
        let session_id = SessionId::new();
        let response = TuiBridgeResponseFrame {
            version: TUI_BRIDGE_PROTOCOL_VERSION,
            message_type: "response".to_owned(),
            request_id: Uuid::new_v4(),
            session_id: session_id.to_string(),
            command: "snapshot".to_owned(),
            success: true,
            snapshot: Some(TuiBridgeSnapshot {
                session_id: session_id.to_string(),
                session_name: Some("snapshot".to_owned()),
                model: None,
                thinking_level: "high".to_owned(),
                is_streaming: true,
                is_compacting: false,
                pending_message_count: 0,
                messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
                inflight_assistant: None,
                active_tools: Vec::new(),
                through_sequence: 4,
            }),
            result: None,
            error: None,
        };
        let frame = serde_json::to_vec(&response).expect("snapshot response");
        let decoded = decode_snapshot_response(&frame).expect("decode snapshot response");
        assert_eq!(decoded, response);
        let mut invalid = response;
        invalid.snapshot.as_mut().expect("snapshot").messages =
            vec![serde_json::json!("not-an-object")];
        assert!(matches!(
            decode_snapshot_response(&serde_json::to_vec(&invalid).expect("invalid response")),
            Err(TuiBridgeError::InvalidSnapshotPayload)
        ));
    }

    #[test]
    fn snapshot_request_correlates_response_without_blocking_event_reader() {
        let (workspace, registry, peer, session_id) = setup();
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let request_frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            Uuid::new_v4(),
        ))
        .expect("register frame");
        let registration = harness
            .register_frame(&request_frame, &peer)
            .expect("register owner");
        let (outbound, requests) = mpsc::sync_channel(1);
        let _broker = registry
            .bind_transport(&registration.token, outbound)
            .expect("bind transport");
        let request_registry = Arc::clone(&registry);
        let request_token = registration.token.clone();
        let request_thread = std::thread::spawn(move || {
            request_registry.request_snapshot(session_id, Duration::from_secs(2))
        });
        let request = requests
            .recv_timeout(Duration::from_secs(1))
            .expect("read snapshot request");
        let request =
            serde_json::from_slice::<TuiBridgeRequestFrame>(&request[..request.len() - 1])
                .expect("decode snapshot request");
        assert_eq!(request.command, "snapshot");
        assert_eq!(request.session_id, session_id.to_string());
        let response = TuiBridgeResponseFrame {
            version: TUI_BRIDGE_PROTOCOL_VERSION,
            message_type: "response".to_owned(),
            request_id: request.request_id,
            session_id: session_id.to_string(),
            command: "snapshot".to_owned(),
            success: true,
            snapshot: Some(TuiBridgeSnapshot {
                session_id: session_id.to_string(),
                session_name: None,
                model: None,
                thinking_level: "high".to_owned(),
                is_streaming: false,
                is_compacting: false,
                pending_message_count: 0,
                messages: Vec::new(),
                inflight_assistant: None,
                active_tools: Vec::new(),
                through_sequence: 3,
            }),
            result: None,
            error: None,
        };
        registry
            .resolve_snapshot_response(&request_token, response)
            .expect("resolve snapshot response");
        let snapshot = request_thread
            .join()
            .expect("snapshot request thread")
            .expect("snapshot response");
        assert_eq!(snapshot.through_sequence, 3);
    }

    #[test]
    fn command_request_round_trip_preserves_payload_and_result() {
        let (workspace, registry, peer, session_id) = setup();
        let harness = TuiBridgeHarness::new(Arc::clone(&registry));
        let request_frame = serde_json::to_vec(&TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            Uuid::new_v4(),
        ))
        .expect("register frame");
        let registration = harness
            .register_frame(&request_frame, &peer)
            .expect("register owner");
        let (outbound, requests) = mpsc::sync_channel(1);
        let _broker = registry
            .bind_transport(&registration.token, outbound)
            .expect("bind transport");
        let command_registry = Arc::clone(&registry);
        let command_token = registration.token.clone();
        let command_thread = std::thread::spawn(move || {
            command_registry.request_command(
                session_id,
                "prompt",
                Some(serde_json::json!({"content": "hi"})),
                Duration::from_secs(2),
            )
        });
        let request = requests
            .recv_timeout(Duration::from_secs(1))
            .expect("read command request");
        let request =
            serde_json::from_slice::<TuiBridgeRequestFrame>(&request[..request.len() - 1])
                .expect("decode command request");
        assert_eq!(request.command, "prompt");
        assert_eq!(request.payload, Some(serde_json::json!({"content": "hi"})));
        registry
            .resolve_response(
                &command_token,
                TuiBridgeResponseFrame {
                    version: TUI_BRIDGE_PROTOCOL_VERSION,
                    message_type: "response".to_owned(),
                    request_id: request.request_id,
                    session_id: session_id.to_string(),
                    command: "prompt".to_owned(),
                    success: true,
                    snapshot: None,
                    result: Some(serde_json::json!({"status": "accepted"})),
                    error: None,
                },
            )
            .expect("resolve command response");
        let response = command_thread
            .join()
            .expect("command response thread")
            .expect("command response");
        assert_eq!(
            response.result,
            Some(serde_json::json!({"status": "accepted"}))
        );
    }

    #[test]
    fn preclaim_decoder_rejects_controlled_paths_and_accepts_bounded_requests() {
        let request_id = Uuid::new_v4();
        let bridge_instance_id = Uuid::new_v4();
        let frame = serde_json::json!({
            "version": TUI_BRIDGE_PROTOCOL_VERSION,
            "type": "preclaim",
            "requestId": request_id,
            "bridgeInstanceId": bridge_instance_id,
            "targetSessionFile": "/tmp/session.jsonl"
        });
        let decoded = decode_inbound_frame(
            serde_json::to_string(&frame)
                .expect("preclaim JSON")
                .as_bytes(),
        )
        .expect("decode preclaim");
        assert!(matches!(
            decoded,
            super::TuiBridgeInboundFrame::Preclaim(request)
                if request.request_id == request_id
        ));
        let invalid = serde_json::json!({
            "version": TUI_BRIDGE_PROTOCOL_VERSION,
            "type": "preclaim",
            "requestId": request_id,
            "bridgeInstanceId": bridge_instance_id,
            "targetSessionFile": "/tmp/session\n.jsonl"
        });
        assert!(matches!(
            decode_inbound_frame(
                serde_json::to_string(&invalid)
                    .expect("invalid preclaim JSON")
                    .as_bytes()
            ),
            Err(TuiBridgeError::InvalidPreclaimPath)
        ));
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
