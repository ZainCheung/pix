use std::io::ErrorKind;
use std::time::Duration;
use std::time::{Instant, SystemTime};

use pix_wire::{
    ClientEnvelope, NoiseHandshake, NoisePattern, NoiseTransport, ServerEnvelope, WireError,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::connection_manager::{
    ConnectionId, ConnectionRegistry, ConnectionRegistryError, RequestAdmission, RequestLedger,
};
use crate::direct_tcp::{DirectTcpError, EncryptedConnection};
use crate::host_dispatcher::HostProtocolDispatcher;
use crate::pairing::{
    ApprovedDevice, PairingCoordinator, PairingError, PairingPending, PairingToken,
};

const PAIRING_CONNECTION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Serialize, Deserialize)]
struct DeviceIntroduction {
    token: String,
    device_name: String,
}

/// Completed XX handshake waiting for explicit host approval.
pub struct PendingPairingConnection {
    connection: EncryptedConnection,
    handshake: NoiseHandshake,
    pending: PairingPending,
}

impl PendingPairingConnection {
    /// Runs the host side of Noise XX and creates a local approval request.
    ///
    /// The first message has an empty payload. The host issues a bounded,
    /// short-lived token inside authenticated message 2; the device name and
    /// same token return only in encrypted/authenticated message 3.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for transport, Noise, payload, token,
    /// or device identity failures.
    pub fn accept(
        mut connection: EncryptedConnection,
        host_private_key: &[u8],
        coordinator: &PairingCoordinator,
        now: SystemTime,
    ) -> Result<Self, SecureConnectionError> {
        let message_1 = connection.read_frame()?;
        Self::accept_after_message_1(connection, &message_1, host_private_key, coordinator, now)
    }

    /// Completes the host side of Noise XX after the first frame has already
    /// been read. The host listener uses this to classify the one shared TCP
    /// endpoint without losing the first handshake message.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for transport, Noise, payload, token,
    /// or device identity failures.
    pub fn accept_after_message_1(
        connection: EncryptedConnection,
        message_1: &[u8],
        host_private_key: &[u8],
        coordinator: &PairingCoordinator,
        now: SystemTime,
    ) -> Result<Self, SecureConnectionError> {
        let mut handshake = NoiseHandshake::responder(NoisePattern::PairingXx, host_private_key)?;
        let payload = handshake.read_message(message_1)?;
        Self::accept_after_message_1_with_handshake(
            connection,
            handshake,
            &payload,
            coordinator,
            now,
        )
    }

    /// Continues XX pairing with a responder handshake that already consumed
    /// message 1. Keeping this separate lets the shared host listener try XX
    /// and IK against the same first frame without replaying bytes on a socket.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for transport, payload, token, or
    /// device identity failures.
    pub fn accept_after_message_1_with_handshake(
        mut connection: EncryptedConnection,
        mut handshake: NoiseHandshake,
        message_1_payload: &[u8],
        coordinator: &PairingCoordinator,
        now: SystemTime,
    ) -> Result<Self, SecureConnectionError> {
        if !message_1_payload.is_empty() {
            return Err(SecureConnectionError::UnexpectedHandshakePayload);
        }
        let offer_started = Instant::now();
        let offer = coordinator.issue_offer(now)?;
        let pending_result = (|| {
            let offer_payload = pix_wire::pairing_offer(offer.token.expose())?;
            let message_2 = handshake.write_message(&offer_payload)?;
            connection.write_frame(&message_2)?;
            let message_3 = connection.read_frame()?;
            let introduction: DeviceIntroduction =
                serde_json::from_slice(&handshake.read_message(&message_3)?)?;
            let token = PairingToken::parse_exposed(&introduction.token)?;
            if token != offer.token {
                return Err(SecureConnectionError::PairingTokenMismatch);
            }
            let remote_static = handshake
                .remote_static()
                .ok_or(SecureConnectionError::MissingRemoteStatic)?
                .to_vec();
            let approval_time = now
                .checked_add(offer_started.elapsed())
                .ok_or(PairingError::ClockOverflow)?;
            Ok(coordinator.begin_approval(
                &token,
                introduction.device_name,
                &remote_static,
                handshake.handshake_hash(),
                approval_time,
            )?)
        })();
        if pending_result.is_err() {
            coordinator.invalidate_offer(&offer.token);
        }
        let pending = pending_result?;
        connection.set_timeout(Some(PAIRING_CONNECTION_TIMEOUT))?;
        Ok(Self {
            connection,
            handshake,
            pending,
        })
    }

    #[must_use]
    pub const fn pending(&self) -> &PairingPending {
        &self.pending
    }

    /// Persists host approval and unlocks encrypted application traffic.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] if approval persistence or Noise
    /// transport conversion fails.
    pub fn approve(
        self,
        coordinator: &PairingCoordinator,
        now: SystemTime,
    ) -> Result<AuthenticatedConnection, SecureConnectionError> {
        let device = coordinator.approve(self.pending.id, now)?;
        // Approval is durable trust and must not depend on this socket still
        // being alive: the phone often suspends while the user walks over to
        // the Mac to approve, and macOS then fails socket configuration with
        // EINVAL after the peer reset. Keep the approval; a dead connection
        // is discovered by the dispatcher's first read or write, and the
        // phone completes pairing by reconnecting with IK.
        let _ = self.connection.set_timeout(None);
        Ok(AuthenticatedConnection {
            connection: self.connection,
            transport: self.handshake.into_transport()?,
            device,
            request_ledger: RequestLedger::new(),
        })
    }

    /// Rejects host approval. Dropping this value closes the TCP connection.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] if the approval was already handled.
    pub fn reject(self, coordinator: &PairingCoordinator) -> Result<(), SecureConnectionError> {
        coordinator.reject(self.pending.id)?;
        Ok(())
    }

    #[must_use]
    pub const fn peer_addr(&self) -> std::net::SocketAddr {
        self.connection.peer_addr()
    }
}

/// IK-authenticated connection for versioned Pix application messages.
pub struct AuthenticatedConnection {
    connection: EncryptedConnection,
    transport: NoiseTransport,
    device: ApprovedDevice,
    request_ledger: RequestLedger,
}

impl AuthenticatedConnection {
    /// Runs the host side of Noise IK and authorizes the handshake's actual
    /// remote static key against non-revoked device records.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for transport, Noise, or peer trust failures.
    pub fn accept_reconnect(
        mut connection: EncryptedConnection,
        host_private_key: &[u8],
        coordinator: &PairingCoordinator,
    ) -> Result<Self, SecureConnectionError> {
        let message_1 = connection.read_frame()?;
        Self::accept_reconnect_after_message_1(
            connection,
            &message_1,
            host_private_key,
            coordinator,
        )
    }

    /// Completes the host side of Noise IK after the first frame has already
    /// been read. This is the reconnect counterpart to
    /// [`PendingPairingConnection::accept_after_message_1`].
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for transport, Noise, or peer trust
    /// failures.
    pub fn accept_reconnect_after_message_1(
        connection: EncryptedConnection,
        message_1: &[u8],
        host_private_key: &[u8],
        coordinator: &PairingCoordinator,
    ) -> Result<Self, SecureConnectionError> {
        let mut handshake = NoiseHandshake::responder(NoisePattern::ReconnectIk, host_private_key)?;
        let payload = handshake.read_message(message_1)?;
        Self::accept_reconnect_after_message_1_with_handshake(
            connection,
            handshake,
            &payload,
            coordinator,
        )
    }

    /// Continues IK reconnect with a responder handshake that already consumed
    /// message 1, allowing one TCP listener to classify both handshake modes.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for transport, Noise, or peer trust
    /// failures.
    pub fn accept_reconnect_after_message_1_with_handshake(
        mut connection: EncryptedConnection,
        mut handshake: NoiseHandshake,
        message_1_payload: &[u8],
        coordinator: &PairingCoordinator,
    ) -> Result<Self, SecureConnectionError> {
        if !message_1_payload.is_empty() {
            return Err(SecureConnectionError::UnexpectedHandshakePayload);
        }
        let remote_static = handshake
            .remote_static()
            .ok_or(SecureConnectionError::MissingRemoteStatic)?;
        let device = coordinator.authenticate_peer(remote_static)?;
        let message_2 = handshake.write_message(b"")?;
        connection.write_frame(&message_2)?;
        connection.set_timeout(None)?;
        Ok(Self {
            connection,
            transport: handshake.into_transport()?,
            device,
            request_ledger: RequestLedger::new(),
        })
    }

    #[must_use]
    pub const fn device(&self) -> &ApprovedDevice {
        &self.device
    }

    #[must_use]
    pub const fn peer_addr(&self) -> std::net::SocketAddr {
        self.connection.peer_addr()
    }

    /// Adds this connection to the live-device registry for revocation.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] if the socket handle cannot be cloned
    /// or the device was concurrently revoked.
    pub fn register(
        &self,
        registry: &ConnectionRegistry,
    ) -> Result<ConnectionId, SecureConnectionError> {
        Ok(registry.register(&self.device, self.connection.control()?)?)
    }

    /// Receives, reassembles, authenticates, and decodes one client request.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for transport, authentication,
    /// fragmentation, replay, capacity, or protocol failures.
    pub fn receive_request(&mut self) -> Result<ClientEnvelope, SecureConnectionError> {
        loop {
            let ciphertext = self.connection.read_frame()?;
            if let Some(plaintext) = self.transport.decrypt_record(&ciphertext)? {
                let request = ClientEnvelope::decode(&plaintext)?;
                match self.request_ledger.admit(request.request_id)? {
                    RequestAdmission::Accepted => return Ok(request),
                    RequestAdmission::DuplicateOrStale => {
                        return Err(SecureConnectionError::DuplicateOrStaleRequest(
                            request.request_id,
                        ));
                    }
                }
            }
        }
    }

    /// Marks one accepted request complete, freeing a pending-request slot.
    pub fn complete_request(&mut self, request_id: u64) -> bool {
        self.request_ledger.complete(request_id)
    }

    /// Sets the receive polling timeout used by the host event loop.
    ///
    /// A finite timeout lets the connection service forward Pi events while
    /// the phone is idle without creating a second writer for the Noise
    /// transport. The timeout is applied to both directions by the TCP layer;
    /// writes remain bounded by the same value.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] when the socket deadline cannot be
    /// configured.
    pub fn set_receive_timeout(
        &self,
        timeout: Option<Duration>,
    ) -> Result<(), SecureConnectionError> {
        self.connection.set_read_timeout(timeout)?;
        Ok(())
    }

    /// Encodes and encrypts one server event, writing every Noise fragment.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for protocol, encryption, or transport failures.
    pub fn send_event(&mut self, event: &ServerEnvelope) -> Result<(), SecureConnectionError> {
        let plaintext = event.encode()?;
        for ciphertext in self.transport.encrypt_message(&plaintext)? {
            self.connection.write_frame(&ciphertext)?;
        }
        Ok(())
    }

    /// Returns one unsolicited event mapped from an attached Pi runtime.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] when canonical encoding, Noise
    /// encryption, or the socket write fails.
    pub fn send_pending_events(
        &mut self,
        dispatcher: &mut HostProtocolDispatcher,
    ) -> Result<usize, SecureConnectionError> {
        let events = dispatcher.drain_events();
        let count = events.len();
        for event in events {
            self.send_event(&event)?;
        }
        Ok(count)
    }

    /// Receives and dispatches one request, sends every immediate response,
    /// then releases its pending-request slot.
    ///
    /// The slot remains occupied until all responses are written. If the
    /// transport fails, the connection is unusable and its ledger is discarded
    /// with it, preserving the no-automatic-retry contract.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for receive, dispatch response
    /// encoding, encryption, or transport failures.
    pub fn dispatch_next(
        &mut self,
        dispatcher: &mut HostProtocolDispatcher,
    ) -> Result<usize, SecureConnectionError> {
        let request = self.receive_request()?;
        self.dispatch_request(request, dispatcher)
    }

    /// Dispatches one request when available, returning `Ok(None)` when the
    /// configured receive timeout elapsed. This keeps request handling and
    /// event forwarding on the same connection writer.
    ///
    /// # Errors
    ///
    /// Returns [`SecureConnectionError`] for malformed, replayed, or failed
    /// requests and encrypted response writes.
    pub fn try_dispatch_next(
        &mut self,
        dispatcher: &mut HostProtocolDispatcher,
    ) -> Result<Option<usize>, SecureConnectionError> {
        let request = match self.receive_request() {
            Ok(request) => request,
            Err(SecureConnectionError::DirectTcp(DirectTcpError::Read(error)))
                if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        self.dispatch_request(request, dispatcher).map(Some)
    }

    fn dispatch_request(
        &mut self,
        request: ClientEnvelope,
        dispatcher: &mut HostProtocolDispatcher,
    ) -> Result<usize, SecureConnectionError> {
        let request_id = request.request_id;
        let responses = dispatcher.prepare_dispatch(request);
        let response_count = responses.len();
        for pending in responses {
            // Deferred work (notably an authoritative snapshot after create or
            // rename) starts only after the acknowledgement has reached the
            // encrypted transport.
            self.send_event(&dispatcher.resolve_response(pending))?;
        }
        debug_assert!(self.complete_request(request_id));
        Ok(response_count)
    }
}

#[derive(Debug, Error)]
pub enum SecureConnectionError {
    #[error(transparent)]
    DirectTcp(#[from] DirectTcpError),
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Pairing(#[from] PairingError),
    #[error(transparent)]
    ConnectionRegistry(#[from] ConnectionRegistryError),
    #[error("secure handshake payload is invalid: {0}")]
    Payload(#[from] serde_json::Error),
    #[error("secure handshake did not reveal a remote static identity")]
    MissingRemoteStatic,
    #[error("Noise XX first message unexpectedly contained a payload")]
    UnexpectedHandshakePayload,
    #[error("Noise XX pairing introduction returned a different token")]
    PairingTokenMismatch,
    #[error("request ID {0} is duplicated or older than the connection watermark")]
    DuplicateOrStaleRequest(u64),
}
