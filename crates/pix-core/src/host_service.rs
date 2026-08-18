//! Long-lived host-side connection service.
//!
//! The service owns the shared LAN endpoint and keeps the secure-channel
//! boundary in [`crate::secure_connection`]. It intentionally exposes only
//! approval summaries to a UI; pairing tokens, private keys, and decrypted
//! protocol payloads never cross this boundary.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use thiserror::Error;
use uuid::Uuid;

use crate::connection_manager::{ConnectionId, ConnectionRegistry};
use crate::direct_tcp::{DirectTcpError, DirectTcpListener, EncryptedConnection};
use crate::discovery::{LanEndpoint, LanEndpointError};
use crate::host_dispatcher::{HostProtocolDispatcher, HostState};
use crate::pairing::{PairingCoordinator, PairingError, PairingPending};
use crate::runtime_manager::{ActiveRuntimeSummary, RuntimeManager, RuntimeManagerError};
use crate::secure_connection::{
    AuthenticatedConnection, PendingPairingConnection, SecureConnectionError,
};
use crate::session_lock::SessionId;
use pix_wire::{NoiseHandshake, NoisePattern, WireError};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(120);

/// A privacy-bounded view of a device waiting for host approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRequest {
    pub id: Uuid,
    pub device_name: String,
    pub confirmation_code: String,
    pub expires_at: SystemTime,
    pub peer_addr: std::net::SocketAddr,
}

impl From<&PairingPending> for PairingRequest {
    fn from(pending: &PairingPending) -> Self {
        Self {
            id: pending.id,
            device_name: pending.device_name.clone(),
            confirmation_code: pending.confirmation_code.clone(),
            expires_at: pending.expires_at,
            // Filled by the service because PairingPending deliberately does
            // not carry transport metadata.
            peer_addr: std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
        }
    }
}

/// Events safe for a native host UI or payload-free logger to consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostServiceEvent {
    PairingRequested(PairingRequest),
    ConnectionEstablished {
        connection_id: ConnectionId,
        device_id: String,
        device_name: String,
    },
    ConnectionClosed {
        connection_id: ConnectionId,
        device_id: String,
    },
    ConnectionFailed {
        peer_addr: std::net::SocketAddr,
        stage: ConnectionStage,
    },
}

/// Coarse failure stage that is safe to show in diagnostics without exposing
/// protocol payloads or filesystem content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStage {
    ReadFirstFrame,
    Handshake,
    PairingApproval,
    Protocol,
}

/// A running host service and its native-UI control surface.
pub struct HostServiceHandle {
    shared: Arc<HostServiceShared>,
    events: mpsc::Receiver<HostServiceEvent>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HostServiceHandle {
    /// Returns the next payload-free service event without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`mpsc::TryRecvError::Disconnected`] if the service exited.
    pub fn try_next_event(&self) -> Result<Option<HostServiceEvent>, mpsc::TryRecvError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Returns a snapshot of pairing requests currently waiting for approval.
    #[must_use]
    pub fn pending_requests(&self) -> Vec<PairingRequest> {
        self.shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|pending| {
                let mut request = PairingRequest::from(pending.connection.pending());
                request.peer_addr = pending.peer_addr;
                request
            })
            .collect()
    }

    /// Approves a pending phone and starts its authenticated protocol loop.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] when the request expired, was already
    /// handled, or durable device trust could not be written.
    pub fn approve(&self, request_id: Uuid) -> Result<(), HostServiceError> {
        let pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&request_id)
            .ok_or(PairingError::UnknownOrExpiredApproval)?;
        let now = SystemTime::now();
        let authenticated = pending.connection.approve(&self.shared.coordinator, now)?;
        // Revocation is fail-closed for reconnects racing with a trust
        // mutation. An explicit re-pair of the same static device identity is
        // the user-authenticated repair operation, so clear that in-memory
        // barrier before registering the newly approved connection.
        let device_id = authenticated.device().id.clone();
        self.shared.registry.allow_repaired_device(&device_id);
        self.shared.refresh_host_state();
        spawn_authenticated(authenticated, &self.shared);
        Ok(())
    }

    /// Rejects a pending phone and closes its socket.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] when the request is unknown or expired.
    pub fn reject(&self, request_id: Uuid) -> Result<(), HostServiceError> {
        let pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&request_id)
            .ok_or(PairingError::UnknownOrExpiredApproval)?;
        pending.connection.reject(&self.shared.coordinator)?;
        Ok(())
    }

    /// Lists durable paired devices from host configuration.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] when configuration cannot be loaded.
    pub fn paired_devices(&self) -> Result<Vec<crate::config::DeviceRecord>, HostServiceError> {
        Ok(self.shared.coordinator.list_devices()?)
    }

    /// Revokes a paired device and closes every matching live connection.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] when the device is unknown or persistence
    /// or socket shutdown fails.
    pub fn revoke_device(
        &self,
        device_id: &str,
    ) -> Result<crate::pairing::ApprovedDevice, HostServiceError> {
        let revoked = self
            .shared
            .coordinator
            .revoke_and_disconnect(device_id, &self.shared.registry)?
            .0;
        self.shared.refresh_host_state();
        Ok(revoked)
    }

    /// Returns Pix-managed Pi runtimes currently held by this host process.
    #[must_use]
    pub fn active_sessions(&self) -> Vec<ActiveRuntimeSummary> {
        self.shared.runtimes.active_sessions()
    }

    /// Stops one active Pi runtime so the native session can be resumed later.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] when the session is unknown or cannot be
    /// stopped cleanly.
    pub fn release_session(&self, session_id: &str) -> Result<(), HostServiceError> {
        let session_id = session_id
            .parse::<SessionId>()
            .map_err(|_| HostServiceError::InvalidSession(session_id.to_owned()))?;
        self.shared.runtimes.release(session_id)?;
        Ok(())
    }

    /// Requests service shutdown and waits for its accept loop to exit.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, pending)| pending.connection)
            .collect::<Vec<_>>();
        for connection in pending {
            let _ = connection.reject(&self.shared.coordinator);
        }
        let _ = self.shared.registry.close_all();
    }
}

impl Drop for HostServiceHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts the Bonjour-advertised Pix host endpoint.
pub struct HostService;

impl HostService {
    /// Starts a cancellable service on a Bonjour endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] if the endpoint cannot enter non-blocking
    /// mode.
    pub fn start(
        endpoint: LanEndpoint,
        host_private_key: Vec<u8>,
        coordinator: Arc<PairingCoordinator>,
        host_state: Arc<HostState>,
        runtimes: Arc<RuntimeManager>,
    ) -> Result<HostServiceHandle, HostServiceError> {
        Self::start_listener(
            ServiceListener::Lan(endpoint),
            host_private_key,
            coordinator,
            host_state,
            runtimes,
        )
    }

    /// Starts the same service on a caller-supplied direct listener. This is
    /// used by deterministic host-core tests and does not advertise Bonjour.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] if the listener cannot enter non-blocking
    /// mode.
    pub fn start_direct(
        listener: DirectTcpListener,
        host_private_key: Vec<u8>,
        coordinator: Arc<PairingCoordinator>,
        host_state: Arc<HostState>,
        runtimes: Arc<RuntimeManager>,
    ) -> Result<HostServiceHandle, HostServiceError> {
        Self::start_listener(
            ServiceListener::Direct(listener),
            host_private_key,
            coordinator,
            host_state,
            runtimes,
        )
    }

    fn start_listener(
        mut listener: ServiceListener,
        host_private_key: Vec<u8>,
        coordinator: Arc<PairingCoordinator>,
        host_state: Arc<HostState>,
        runtimes: Arc<RuntimeManager>,
    ) -> Result<HostServiceHandle, HostServiceError> {
        NoiseHandshake::responder(NoisePattern::PairingXx, &host_private_key)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (events_tx, events_rx) = mpsc::channel();
        let shared = Arc::new(HostServiceShared {
            coordinator,
            host_state,
            runtimes,
            host_private_key: Arc::new(host_private_key),
            registry: Arc::new(ConnectionRegistry::new()),
            pending: Mutex::new(HashMap::new()),
            events: events_tx,
        });
        let thread_shared = Arc::clone(&shared);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("pix-host-accept".to_owned())
            .spawn(move || accept_loop(&listener, &thread_shared, &thread_stop))
            .map_err(HostServiceError::Spawn)?;
        Ok(HostServiceHandle {
            shared,
            events: events_rx,
            stop,
            thread: Some(thread),
        })
    }
}

struct HostServiceShared {
    coordinator: Arc<PairingCoordinator>,
    host_state: Arc<HostState>,
    runtimes: Arc<RuntimeManager>,
    host_private_key: Arc<Vec<u8>>,
    registry: Arc<ConnectionRegistry>,
    pending: Mutex<HashMap<Uuid, PendingConnection>>,
    events: mpsc::Sender<HostServiceEvent>,
}

impl HostServiceShared {
    /// Reloads durable configuration into the shared in-memory view after a
    /// device trust mutation, so per-device data such as the relay channel is
    /// visible to dispatchers immediately.
    fn refresh_host_state(&self) {
        if let Ok(config) = self.coordinator.current_config() {
            let _ = self.host_state.replace(config);
        }
    }
}

struct PendingConnection {
    connection: PendingPairingConnection,
    peer_addr: std::net::SocketAddr,
}

enum ServiceListener {
    Lan(LanEndpoint),
    Direct(DirectTcpListener),
}

impl ServiceListener {
    fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), HostServiceError> {
        match self {
            Self::Lan(endpoint) => endpoint
                .set_nonblocking(nonblocking)
                .map_err(HostServiceError::LanEndpoint),
            Self::Direct(listener) => listener
                .set_nonblocking(nonblocking)
                .map_err(HostServiceError::DirectTcp),
        }
    }

    fn accept(&self) -> Result<EncryptedConnection, HostServiceError> {
        match self {
            Self::Lan(endpoint) => endpoint.accept().map_err(HostServiceError::LanEndpoint),
            Self::Direct(listener) => listener.accept().map_err(HostServiceError::DirectTcp),
        }
    }
}

fn accept_loop(
    listener: &ServiceListener,
    shared: &Arc<HostServiceShared>,
    stop: &Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok(connection) => {
                let connection_shared = Arc::clone(shared);
                let _ = thread::Builder::new()
                    .name("pix-host-handshake".to_owned())
                    .spawn(move || classify_connection(connection, &connection_shared));
            }
            Err(error) if error.is_would_block() => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => break,
        }
    }
}

fn classify_connection(mut connection: EncryptedConnection, shared: &Arc<HostServiceShared>) {
    let peer_addr = connection.peer_addr();
    if connection.set_timeout(Some(HANDSHAKE_TIMEOUT)).is_err() {
        emit_failure(shared, peer_addr, ConnectionStage::Handshake);
        return;
    }
    let Ok(first) = connection.read_frame() else {
        emit_failure(shared, peer_addr, ConnectionStage::ReadFirstFrame);
        return;
    };

    let Ok(mut pairing) =
        NoiseHandshake::responder(NoisePattern::PairingXx, &shared.host_private_key)
    else {
        emit_failure(shared, peer_addr, ConnectionStage::Handshake);
        return;
    };
    if let Ok(payload) = pairing.read_message(&first)
        && payload.is_empty()
    {
        match PendingPairingConnection::accept_after_message_1_with_handshake(
            connection,
            pairing,
            &payload,
            &shared.coordinator,
            SystemTime::now(),
        ) {
            Ok(pending) => {
                let mut summary = PairingRequest::from(pending.pending());
                summary.peer_addr = peer_addr;
                let id = summary.id;
                shared
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        id,
                        PendingConnection {
                            connection: pending,
                            peer_addr,
                        },
                    );
                let _ = shared
                    .events
                    .send(HostServiceEvent::PairingRequested(summary));
            }
            Err(_) => emit_failure(shared, peer_addr, ConnectionStage::Handshake),
        }
        return;
    }

    let Ok(mut reconnect) =
        NoiseHandshake::responder(NoisePattern::ReconnectIk, &shared.host_private_key)
    else {
        emit_failure(shared, peer_addr, ConnectionStage::Handshake);
        return;
    };
    let Ok(payload) = reconnect.read_message(&first) else {
        emit_failure(shared, peer_addr, ConnectionStage::Handshake);
        return;
    };
    if !payload.is_empty() {
        emit_failure(shared, peer_addr, ConnectionStage::Handshake);
        return;
    }
    match AuthenticatedConnection::accept_reconnect_after_message_1_with_handshake(
        connection,
        reconnect,
        &payload,
        &shared.coordinator,
    ) {
        Ok(authenticated) => spawn_authenticated(authenticated, shared),
        Err(_) => emit_failure(shared, peer_addr, ConnectionStage::Handshake),
    }
}

fn spawn_authenticated(
    mut authenticated: AuthenticatedConnection,
    shared: &Arc<HostServiceShared>,
) {
    let Ok(connection_id) = authenticated.register(&shared.registry) else {
        emit_failure(
            shared,
            authenticated.peer_addr(),
            ConnectionStage::PairingApproval,
        );
        return;
    };
    let device = authenticated.device().clone();
    let _ = shared.events.send(HostServiceEvent::ConnectionEstablished {
        connection_id,
        device_id: device.id.clone(),
        device_name: device.name.clone(),
    });
    if authenticated
        .set_receive_timeout(Some(EVENT_POLL_INTERVAL))
        .is_err()
    {
        shared.registry.unregister(connection_id);
        let _ = shared.events.send(HostServiceEvent::ConnectionClosed {
            connection_id,
            device_id: device.id,
        });
        return;
    }
    let thread_shared = Arc::clone(shared);
    let _ = thread::Builder::new()
        .name("pix-host-connection".to_owned())
        .spawn(move || {
            let mut dispatcher = HostProtocolDispatcher::new(
                Arc::clone(&thread_shared.host_state),
                Arc::clone(&thread_shared.runtimes),
            );
            dispatcher.set_device(device.id.clone());
            loop {
                if authenticated.try_dispatch_next(&mut dispatcher).is_err() {
                    break;
                }
                if authenticated.send_pending_events(&mut dispatcher).is_err() {
                    break;
                }
            }
            dispatcher.disconnect();
            thread_shared.registry.unregister(connection_id);
            let _ = thread_shared
                .events
                .send(HostServiceEvent::ConnectionClosed {
                    connection_id,
                    device_id: device.id,
                });
        });
}

fn emit_failure(
    shared: &HostServiceShared,
    peer_addr: std::net::SocketAddr,
    stage: ConnectionStage,
) {
    let _ = shared
        .events
        .send(HostServiceEvent::ConnectionFailed { peer_addr, stage });
}

#[derive(Debug, Error)]
pub enum HostServiceError {
    #[error(transparent)]
    LanEndpoint(#[from] LanEndpointError),
    #[error(transparent)]
    DirectTcp(#[from] DirectTcpError),
    #[error(transparent)]
    SecureConnection(#[from] SecureConnectionError),
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Pairing(#[from] PairingError),
    #[error(transparent)]
    Runtime(#[from] RuntimeManagerError),
    #[error("invalid session identifier: {0}")]
    InvalidSession(String),
    #[error("failed to start Pix host thread: {0}")]
    Spawn(io::Error),
}

impl HostServiceError {
    fn is_would_block(&self) -> bool {
        match self {
            Self::LanEndpoint(LanEndpointError::DirectTcp(DirectTcpError::Accept(error)))
            | Self::DirectTcp(DirectTcpError::Accept(error)) => {
                error.kind() == io::ErrorKind::WouldBlock
            }
            _ => false,
        }
    }
}
