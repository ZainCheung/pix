use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use thiserror::Error;
use uuid::Uuid;

use pix_wire::MAX_PENDING_REQUESTS;

use crate::direct_tcp::{ConnectionControl, DirectTcpError};
use crate::pairing::ApprovedDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(Uuid);

impl ConnectionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

struct ConnectionRecord {
    device_id: String,
    control: ConnectionControl,
}

#[derive(Default)]
struct RegistryState {
    connections: HashMap<ConnectionId, ConnectionRecord>,
    revoked_devices: HashSet<String>,
}

/// Tracks authenticated live sockets so device revocation is immediate.
#[derive(Default)]
pub struct ConnectionRegistry {
    state: Mutex<RegistryState>,
}

impl ConnectionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an authenticated connection unless the device was revoked.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionRegistryError`] for revoked devices.
    pub fn register(
        &self,
        device: &ApprovedDevice,
        control: ConnectionControl,
    ) -> Result<ConnectionId, ConnectionRegistryError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.revoked_devices.contains(&device.id) {
            let _ = control.close();
            return Err(ConnectionRegistryError::Revoked(device.id.clone()));
        }
        let id = ConnectionId::new();
        state.connections.insert(
            id,
            ConnectionRecord {
                device_id: device.id.clone(),
                control,
            },
        );
        Ok(id)
    }

    pub fn unregister(&self, connection_id: ConnectionId) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .connections
            .remove(&connection_id)
            .is_some()
    }

    /// Marks a device revoked and closes all matching live sockets.
    ///
    /// The revocation marker also closes connections racing with persistence.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionRegistryError`] if any socket shutdown fails. All
    /// matching connections are removed regardless.
    pub fn revoke_device(&self, device_id: &str) -> Result<usize, ConnectionRegistryError> {
        let controls = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.revoked_devices.insert(device_id.to_owned());
            let ids = state
                .connections
                .iter()
                .filter(|(_, record)| record.device_id == device_id)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| state.connections.remove(&id))
                .map(|record| record.control)
                .collect::<Vec<_>>()
        };
        let count = controls.len();
        let mut first_error = None;
        for control in controls {
            if let Err(error) = control.close()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            return Err(ConnectionRegistryError::Close(error));
        }
        Ok(count)
    }

    /// Closes every currently registered socket, used during host service
    /// shutdown. The registry is emptied even if one operating-system close
    /// operation reports an error.
    ///
    /// # Errors
    ///
    /// Returns the first socket shutdown error after all connections have been
    /// removed from the registry.
    pub fn close_all(&self) -> Result<usize, ConnectionRegistryError> {
        let controls = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .connections
                .drain()
                .map(|(_, record)| record.control)
                .collect::<Vec<_>>()
        };
        let count = controls.len();
        let mut first_error = None;
        for control in controls {
            if let Err(error) = control.close()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            return Err(ConnectionRegistryError::Close(error));
        }
        Ok(count)
    }

    /// Clears the local race barrier after the same static identity is
    /// explicitly paired again.
    pub fn allow_repaired_device(&self, device_id: &str) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revoked_devices
            .remove(device_id);
    }

    #[must_use]
    pub fn active_for_device(&self, device_id: &str) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .connections
            .values()
            .filter(|record| record.device_id == device_id)
            .count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestAdmission {
    Accepted,
    DuplicateOrStale,
}

/// Bounded connection-scoped request correlation and replay rejection.
#[derive(Debug, Default)]
pub struct RequestLedger {
    highest_request_id: Option<u64>,
    pending: HashSet<u64>,
}

impl RequestLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admits a strictly increasing request ID while enforcing 128 in flight.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionRegistryError::PendingCapacity`] when the connection
    /// already has 128 accepted requests without completion.
    pub fn admit(&mut self, request_id: u64) -> Result<RequestAdmission, ConnectionRegistryError> {
        if self
            .highest_request_id
            .is_some_and(|highest| request_id <= highest)
        {
            return Ok(RequestAdmission::DuplicateOrStale);
        }
        if self.pending.len() >= MAX_PENDING_REQUESTS {
            return Err(ConnectionRegistryError::PendingCapacity);
        }
        self.highest_request_id = Some(request_id);
        self.pending.insert(request_id);
        Ok(RequestAdmission::Accepted)
    }

    pub fn complete(&mut self, request_id: u64) -> bool {
        self.pending.remove(&request_id)
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Debug, Error)]
pub enum ConnectionRegistryError {
    #[error("device is revoked: {0}")]
    Revoked(String),
    #[error("connection already has 128 pending requests")]
    PendingCapacity,
    #[error("failed closing revoked connection: {0}")]
    Close(DirectTcpError),
}

#[cfg(test)]
mod tests {
    use super::{RequestAdmission, RequestLedger};
    use pix_wire::MAX_PENDING_REQUESTS;

    #[test]
    fn request_ledger_rejects_replay_without_unbounded_history() {
        let mut ledger = RequestLedger::new();
        assert_eq!(
            ledger.admit(10).expect("admit request"),
            RequestAdmission::Accepted
        );
        assert!(ledger.complete(10));
        assert_eq!(
            ledger.admit(10).expect("detect replay"),
            RequestAdmission::DuplicateOrStale
        );
        assert_eq!(
            ledger.admit(9).expect("detect stale ID"),
            RequestAdmission::DuplicateOrStale
        );
    }

    #[test]
    fn request_ledger_enforces_pending_limit() {
        let mut ledger = RequestLedger::new();
        for id in 1..=u64::try_from(MAX_PENDING_REQUESTS).expect("limit fits u64") {
            assert_eq!(
                ledger.admit(id).expect("admit within limit"),
                RequestAdmission::Accepted
            );
        }
        assert!(ledger.admit(129).is_err());
        assert_eq!(ledger.pending_count(), MAX_PENDING_REQUESTS);
    }
}
