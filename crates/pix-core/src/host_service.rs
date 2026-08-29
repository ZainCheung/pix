//! Long-lived host-side connection service.
//!
//! The service owns the shared LAN endpoint and keeps the secure-channel
//! boundary in [`crate::secure_connection`]. It intentionally exposes only
//! approval summaries to a UI; pairing tokens, private keys, and decrypted
//! protocol payloads never cross this boundary.

use std::collections::HashMap;
use std::io::{self, Read, Write};
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
use crate::pairing::{
    MAX_PENDING_PAIRING_OFFERS, PairingCoordinator, PairingError, PairingPending,
};
use crate::runtime_manager::{ActiveRuntimeSummary, RuntimeManager, RuntimeManagerError};
use crate::secure_connection::{
    AuthenticatedConnection, PendingPairingConnection, SecureConnectionError,
};
use crate::session_lock::SessionId;
use crate::tui_bridge::{
    TUI_BRIDGE_OUTBOUND_QUEUE, TuiBridgeError, TuiBridgeRegisterResponse, TuiBridgeToken,
    TuiBridgeUnixSocket, decode_inbound_frame, encode_register_response,
};
use pix_wire::{NoiseHandshake, NoisePattern, WireError};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(120);
const TUI_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const TUI_CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A privacy-bounded view of a device waiting for host approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRequest {
    pub id: Uuid,
    pub device_name: String,
    pub confirmation_code: String,
    pub expires_at: SystemTime,
    pub peer_addr: std::net::SocketAddr,
}

/// Result of reconciling durable configuration with the running host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigRefreshReport {
    pub authorization_changed: bool,
    pub released_sessions: usize,
    /// Authorization is already fail-closed; background reconciliation will
    /// retry termination of a runtime that was briefly busy.
    pub cleanup_pending: bool,
    /// Revoked device dispatch is already denied even when an OS socket close
    /// reported an error.
    pub connection_cleanup_failed: bool,
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
    tui_stop: Arc<AtomicBool>,
    tui_thread: Option<JoinHandle<()>>,
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
        self.expire_pending_requests();
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

    /// Drops expired unapproved pairing sockets and their coordinator state.
    ///
    /// This bounds unauthenticated resources even when no host UI is open.
    pub fn expire_pending_requests(&self) -> usize {
        let now = SystemTime::now();
        let expired = {
            let mut pending = self
                .shared
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ids = pending
                .iter()
                .filter(|(_, connection)| now >= connection.connection.pending().expires_at)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id))
                .collect::<Vec<_>>()
        };
        let count = expired.len();
        for pending in expired {
            let _ = pending.connection.reject(&self.shared.coordinator);
        }
        count
    }

    /// Approves a pending phone and starts its authenticated protocol loop.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] when the request expired, was already
    /// handled, or durable device trust could not be written.
    pub fn approve(&self, request_id: Uuid) -> Result<(), HostServiceError> {
        self.expire_pending_requests();
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
        self.expire_pending_requests();
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

    /// Reloads host configuration changed by a local Pix CLI or native client.
    ///
    /// The persistent service owns the in-memory workspace authorization view;
    /// a config mutation performed by another Pix process must explicitly
    /// refresh it before the next remote request is authorized.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] when the durable configuration cannot be
    /// loaded or fails `HostState` validation.
    pub fn refresh_config(&self) -> Result<ConfigRefreshReport, HostServiceError> {
        let config = self.shared.coordinator.current_config()?;
        let previous = self.shared.host_state.snapshot();
        let changed = config != previous;
        let authorized = config
            .workspaces
            .iter()
            .filter_map(|workspace| {
                std::fs::canonicalize(&workspace.path)
                    .ok()
                    .filter(|canonical| canonical == &workspace.path && canonical.is_dir())
            })
            .collect();
        let mut connection_error = None;
        if changed {
            for device in &previous.devices {
                if !config.devices.iter().any(|current| current.id == device.id)
                    && let Err(error) = self.shared.registry.revoke_device(&device.id)
                    && connection_error.is_none()
                {
                    connection_error = Some(error);
                }
            }
            self.shared
                .host_state
                .replace(config)
                .map_err(PairingError::Config)
                .map_err(HostServiceError::Pairing)?;
        }
        let (released_sessions, cleanup_pending) =
            match self.shared.runtimes.release_outside_workspaces(&authorized) {
                Ok(released) => (released.len(), false),
                Err(_) => (0, true),
            };
        Ok(ConfigRefreshReport {
            authorization_changed: changed,
            released_sessions,
            cleanup_pending,
            connection_cleanup_failed: connection_error.is_some(),
        })
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
    ) -> Result<crate::pairing::DeviceRevocation, HostServiceError> {
        let revoked = self
            .shared
            .coordinator
            .revoke_and_disconnect(device_id, &self.shared.registry)?;
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
        self.tui_stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.tui_thread.take() {
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
            None,
        )
    }

    /// Starts the Bonjour-advertised endpoint together with the optional
    /// host-local TUI bridge socket.  The socket is intentionally separate
    /// from the encrypted `pix-wire` listener.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] if either listener cannot be started.
    #[cfg(unix)]
    pub fn start_with_tui_socket(
        endpoint: LanEndpoint,
        host_private_key: Vec<u8>,
        coordinator: Arc<PairingCoordinator>,
        host_state: Arc<HostState>,
        runtimes: Arc<RuntimeManager>,
        tui_socket: TuiBridgeUnixSocket,
    ) -> Result<HostServiceHandle, HostServiceError> {
        Self::start_listener(
            ServiceListener::Lan(endpoint),
            host_private_key,
            coordinator,
            host_state,
            runtimes,
            Some(tui_socket),
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
            None,
        )
    }

    /// Starts a direct test listener together with a host-local TUI bridge
    /// socket.
    ///
    /// # Errors
    ///
    /// Returns [`HostServiceError`] if either listener cannot be started.
    #[cfg(unix)]
    pub fn start_direct_with_tui_socket(
        listener: DirectTcpListener,
        host_private_key: Vec<u8>,
        coordinator: Arc<PairingCoordinator>,
        host_state: Arc<HostState>,
        runtimes: Arc<RuntimeManager>,
        tui_socket: TuiBridgeUnixSocket,
    ) -> Result<HostServiceHandle, HostServiceError> {
        Self::start_listener(
            ServiceListener::Direct(listener),
            host_private_key,
            coordinator,
            host_state,
            runtimes,
            Some(tui_socket),
        )
    }

    fn start_listener(
        mut listener: ServiceListener,
        host_private_key: Vec<u8>,
        coordinator: Arc<PairingCoordinator>,
        host_state: Arc<HostState>,
        runtimes: Arc<RuntimeManager>,
        tui_socket: Option<TuiBridgeUnixSocket>,
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
        let tui_stop = Arc::new(AtomicBool::new(false));
        #[cfg(unix)]
        let tui_thread = if let Some(tui_socket) = tui_socket {
            let tui_shared = Arc::clone(&shared);
            let tui_stop_for_thread = Arc::clone(&tui_stop);
            match thread::Builder::new()
                .name("pix-tui-bridge-accept".to_owned())
                .spawn(move || {
                    tui_bridge_accept_loop(tui_socket, &tui_shared, &tui_stop_for_thread);
                }) {
                Ok(thread) => Some(thread),
                Err(error) => {
                    stop.store(true, Ordering::Release);
                    let _ = thread.join();
                    return Err(HostServiceError::Spawn(error));
                }
            }
        } else {
            None
        };
        #[cfg(not(unix))]
        let tui_thread = {
            let _ = tui_socket;
            None
        };
        Ok(HostServiceHandle {
            shared,
            events: events_rx,
            stop,
            thread: Some(thread),
            tui_stop,
            tui_thread,
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

#[cfg(unix)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "The listener's owned socket must be dropped with its accept thread."
)]
fn tui_bridge_accept_loop(
    listener: TuiBridgeUnixSocket,
    shared: &Arc<HostServiceShared>,
    stop: &Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.try_accept_register() {
            Ok(Some((peer, request, stream))) => {
                let connection_shared = Arc::clone(shared);
                let connection_stop = Arc::clone(stop);
                let _ = thread::Builder::new()
                    .name("pix-tui-bridge-connection".to_owned())
                    .spawn(move || {
                        handle_tui_bridge_connection(
                            &peer,
                            request,
                            stream,
                            &connection_shared,
                            &connection_stop,
                        );
                    });
            }
            Ok(None) | Err(_) => {
                // A malformed or short-lived local client must not stop the host.
                // The listener remains available for the next REGISTER attempt.
                thread::sleep(TUI_ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

#[cfg(unix)]
fn handle_tui_bridge_connection(
    peer: &crate::tui_bridge::TuiBridgePeer,
    request: crate::tui_bridge::TuiBridgeRegister,
    mut stream: std::os::unix::net::UnixStream,
    shared: &Arc<HostServiceShared>,
    stop: &Arc<AtomicBool>,
) {
    let registry = shared.runtimes.tui_bridge();
    let registration = match registry.register(&request, peer) {
        Ok(registration) => registration,
        Err(error) => {
            let response = TuiBridgeRegisterResponse::denied(
                request.session_id,
                tui_bridge_error_code(&error),
            );
            let _ = write_tui_bridge_response(&mut stream, &response);
            return;
        }
    };
    let response = TuiBridgeRegisterResponse::granted(&registration);
    if write_tui_bridge_response(&mut stream, &response).is_err() {
        let _ = registry.disconnect(&registration.token);
        return;
    }
    let Ok(writer_stream) = stream.try_clone() else {
        let _ = registry.disconnect(&registration.token);
        return;
    };
    let (outbound, outbound_receiver) = mpsc::sync_channel(TUI_BRIDGE_OUTBOUND_QUEUE);
    let Ok(broker) = registry.bind_transport(&registration.token, outbound) else {
        let _ = registry.disconnect(&registration.token);
        return;
    };
    let writer_broker = Arc::clone(&broker);
    let writer_registry = Arc::clone(&registry);
    let writer_token = registration.token.clone();
    let writer_stop = Arc::clone(stop);
    if thread::Builder::new()
        .name("pix-tui-bridge-writer".to_owned())
        .spawn(move || {
            tui_bridge_writer_loop(
                writer_stream,
                outbound_receiver,
                writer_broker,
                writer_registry,
                writer_token,
                writer_stop,
            );
        })
        .is_err()
    {
        broker.close();
        let _ = registry.disconnect(&registration.token);
        return;
    }
    tui_bridge_connection_loop(stream, &registration.token, &registry, stop);
    broker.close();
}

#[cfg(unix)]
fn write_tui_bridge_response(
    stream: &mut std::os::unix::net::UnixStream,
    response: &TuiBridgeRegisterResponse,
) -> Result<(), HostServiceError> {
    stream
        .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(HostServiceError::Io)?;
    let frame = encode_register_response(response).map_err(HostServiceError::TuiBridgeEncode)?;
    stream.write_all(&frame).map_err(HostServiceError::Io)?;
    stream.flush().map_err(HostServiceError::Io)
}

#[cfg(unix)]
fn tui_bridge_connection_loop(
    mut stream: std::os::unix::net::UnixStream,
    token: &TuiBridgeToken,
    registry: &Arc<crate::tui_bridge::TuiBridgeRegistry>,
    stop: &Arc<AtomicBool>,
) {
    if stream
        .set_read_timeout(Some(TUI_CONNECTION_POLL_INTERVAL))
        .is_err()
    {
        let _ = registry.disconnect(token);
        return;
    }
    let mut reader = TuiBridgeFrameReader::default();
    while !stop.load(Ordering::Acquire) {
        let Ok(Some(frame)) = reader.next(&mut stream) else {
            let _ = registry.disconnect(token);
            break;
        };
        let Ok(inbound) = decode_inbound_frame(&frame) else {
            let _ = registry.disconnect(token);
            break;
        };
        match inbound {
            crate::tui_bridge::TuiBridgeInboundFrame::Event(event) => {
                if registry.publish_event(token, &event).is_err() {
                    let _ = registry.disconnect(token);
                    break;
                }
            }
            crate::tui_bridge::TuiBridgeInboundFrame::Response(response) => {
                if registry.resolve_response(token, *response).is_err() {
                    let _ = registry.disconnect(token);
                    break;
                }
            }
        }
    }
}

#[cfg(unix)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "These values are moved into the detached bridge writer thread."
)]
fn tui_bridge_writer_loop(
    mut stream: std::os::unix::net::UnixStream,
    receiver: mpsc::Receiver<Vec<u8>>,
    broker: Arc<crate::tui_bridge::TuiBridgeBroker>,
    registry: Arc<crate::tui_bridge::TuiBridgeRegistry>,
    token: TuiBridgeToken,
    stop: Arc<AtomicBool>,
) {
    let _ = stream.set_write_timeout(Some(TUI_CONNECTION_POLL_INTERVAL));
    while !stop.load(Ordering::Acquire) && !broker.is_closed() {
        match receiver.recv_timeout(TUI_CONNECTION_POLL_INTERVAL) {
            Ok(frame) => {
                if stream.write_all(&frame).is_err() || stream.flush().is_err() {
                    broker.close();
                    let _ = registry.disconnect(&token);
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(unix)]
#[derive(Default)]
struct TuiBridgeFrameReader {
    pending: Vec<u8>,
}

#[cfg(unix)]
impl TuiBridgeFrameReader {
    fn next(
        &mut self,
        stream: &mut std::os::unix::net::UnixStream,
    ) -> Result<Option<Vec<u8>>, TuiBridgeError> {
        loop {
            if let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
                return Ok(Some(self.pending.drain(..=position).collect()));
            }
            if self.pending.len() > crate::tui_bridge::TUI_BRIDGE_MAX_FRAME_BYTES {
                return Err(TuiBridgeError::FrameTooLarge);
            }
            let mut chunk = [0_u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) if self.pending.is_empty() => return Ok(None),
                Ok(0) => return Err(TuiBridgeError::MalformedFrame),
                Ok(count) => self.pending.extend_from_slice(&chunk[..count]),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(TuiBridgeError::Io(error)),
            }
        }
    }
}

#[cfg(unix)]
fn tui_bridge_error_code(error: &TuiBridgeError) -> &'static str {
    match error {
        TuiBridgeError::UnsupportedVersion(_) => "unsupported_version",
        TuiBridgeError::OwnerConflict(_) => "conflict",
        TuiBridgeError::PeerUserMismatch { .. }
        | TuiBridgeError::PeerProcessNotFound(_)
        | TuiBridgeError::PeerIdentityMismatch(_)
        | TuiBridgeError::WorkspaceNotAuthorized
        | TuiBridgeError::SessionFileMismatch(_) => "unauthorized",
        _ => "invalid_request",
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
                let mut pending_connections = shared
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if pending_connections.len() >= MAX_PENDING_PAIRING_OFFERS {
                    drop(pending_connections);
                    let _ = pending.reject(&shared.coordinator);
                    emit_failure(shared, peer_addr, ConnectionStage::PairingApproval);
                    return;
                }
                pending_connections.insert(
                    id,
                    PendingConnection {
                        connection: pending,
                        peer_addr,
                    },
                );
                drop(pending_connections);
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
    #[error("failed to encode TUI bridge response: {0}")]
    TuiBridgeEncode(serde_json::Error),
    #[error("TUI bridge socket I/O failed: {0}")]
    Io(io::Error),
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
