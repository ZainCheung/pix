use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;

use mdns_sd::{ServiceDaemon, ServiceInfo};
use thiserror::Error;
use uuid::Uuid;

use pix_wire::{PROTOCOL_MAJOR, host_public_key_fingerprint};

use crate::direct_tcp::{DirectTcpError, DirectTcpListener, EncryptedConnection};

pub const PIX_SERVICE_TYPE: &str = "_pix._tcp.local.";
const MAX_DISPLAY_NAME_BYTES: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BonjourMetadata {
    pub protocol_major: u16,
    pub display_name: String,
    pub public_key_fingerprint: String,
    pub instance_id: Uuid,
}

impl BonjourMetadata {
    /// Creates privacy-bounded Bonjour metadata from the host public key.
    ///
    /// # Errors
    ///
    /// Returns [`BonjourError`] for an empty display name or invalid key length.
    pub fn new(
        display_name: impl Into<String>,
        host_public_key: &[u8],
        instance_id: Uuid,
    ) -> Result<Self, BonjourError> {
        let display_name = display_name.into();
        if display_name.trim().is_empty() {
            return Err(BonjourError::EmptyDisplayName);
        }
        if display_name.len() > MAX_DISPLAY_NAME_BYTES {
            return Err(BonjourError::DisplayNameTooLong(display_name.len()));
        }
        if host_public_key.len() != 32 {
            return Err(BonjourError::InvalidPublicKeyLength(host_public_key.len()));
        }
        Ok(Self {
            protocol_major: PROTOCOL_MAJOR,
            display_name,
            public_key_fingerprint: host_public_key_fingerprint(host_public_key),
            instance_id,
        })
    }

    #[must_use]
    pub fn txt_records(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("v".to_owned(), self.protocol_major.to_string()),
            ("name".to_owned(), self.display_name.clone()),
            ("pk".to_owned(), self.public_key_fingerprint.clone()),
            ("instance".to_owned(), self.instance_id.to_string()),
        ])
    }
}

/// Registered `_pix._tcp.local.` advertisement.
pub struct BonjourAdvertisement {
    daemon: ServiceDaemon,
    fullname: String,
    metadata: BonjourMetadata,
}

impl BonjourAdvertisement {
    /// Starts a Bonjour responder for an already-bound direct TCP port.
    ///
    /// Automatic address publication follows active network interfaces. TXT
    /// data contains only the four fields authorized by the architecture.
    ///
    /// # Errors
    ///
    /// Returns [`BonjourError`] when the mDNS daemon, service record, or
    /// registration cannot be created.
    pub fn register(port: u16, metadata: BonjourMetadata) -> Result<Self, BonjourError> {
        let daemon = ServiceDaemon::new()?;
        let hostname = format!("pix-{}.local.", metadata.instance_id.simple());
        let instance_name = format!("Pix {}", &metadata.instance_id.simple().to_string()[..12]);
        let properties: HashMap<String, String> = metadata.txt_records().into_iter().collect();
        let service = ServiceInfo::new(
            PIX_SERVICE_TYPE,
            &instance_name,
            &hostname,
            (),
            port,
            properties,
        )?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_owned();
        daemon.register(service)?;
        Ok(Self {
            daemon,
            fullname,
            metadata,
        })
    }

    #[must_use]
    pub const fn metadata(&self) -> &BonjourMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn fullname(&self) -> &str {
        &self.fullname
    }
}

impl Drop for BonjourAdvertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// A bound direct TCP listener advertised on Bonjour as one atomic endpoint.
///
/// Constructing these together prevents publishing a stale or guessed port.
/// Dropping the endpoint withdraws Bonjour before closing the listener.
pub struct LanEndpoint {
    advertisement: BonjourAdvertisement,
    listener: DirectTcpListener,
}

impl LanEndpoint {
    /// Binds the requested port and advertises the operating system-selected
    /// bound port. Port zero is supported for dynamic allocation.
    ///
    /// # Errors
    ///
    /// Returns [`LanEndpointError`] when metadata, binding, inspection, or
    /// Bonjour registration fails.
    pub fn start(
        port: u16,
        display_name: impl Into<String>,
        host_public_key: &[u8],
        instance_id: Uuid,
    ) -> Result<Self, LanEndpointError> {
        let metadata = BonjourMetadata::new(display_name, host_public_key, instance_id)?;
        let listener = DirectTcpListener::bind(port)?;
        let bound_port = listener.local_addr()?.port();
        let advertisement = BonjourAdvertisement::register(bound_port, metadata)?;
        Ok(Self {
            advertisement,
            listener,
        })
    }

    /// Accepts one unauthenticated ciphertext-only connection.
    ///
    /// # Errors
    ///
    /// Returns [`LanEndpointError`] if the listener cannot accept or configure
    /// the connection.
    pub fn accept(&self) -> Result<EncryptedConnection, LanEndpointError> {
        Ok(self.listener.accept()?)
    }

    /// Returns the actual bound address and port.
    ///
    /// # Errors
    ///
    /// Returns [`LanEndpointError`] if the socket cannot be inspected.
    pub fn local_addr(&self) -> Result<SocketAddr, LanEndpointError> {
        Ok(self.listener.local_addr()?)
    }

    /// Configures whether [`accept`](Self::accept) should block when no client
    /// is ready. This is used by the host service's cancellable accept loop.
    ///
    /// # Errors
    ///
    /// Returns [`LanEndpointError`] when the operating system rejects the
    /// requested mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), LanEndpointError> {
        self.listener.set_nonblocking(nonblocking)?;
        Ok(())
    }

    #[must_use]
    pub const fn metadata(&self) -> &BonjourMetadata {
        self.advertisement.metadata()
    }
}

#[derive(Debug, Error)]
pub enum BonjourError {
    #[error("Bonjour host display name cannot be empty")]
    EmptyDisplayName,
    #[error("Bonjour host display name is {0} bytes; maximum is 200")]
    DisplayNameTooLong(usize),
    #[error("Bonjour host public key is {0} bytes; expected 32")]
    InvalidPublicKeyLength(usize),
    #[error(transparent)]
    Mdns(#[from] mdns_sd::Error),
}

#[derive(Debug, Error)]
pub enum LanEndpointError {
    #[error(transparent)]
    Bonjour(#[from] BonjourError),
    #[error(transparent)]
    DirectTcp(#[from] DirectTcpError),
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::BonjourMetadata;

    #[test]
    fn txt_records_expose_only_approved_metadata() {
        let instance_id = Uuid::new_v4();
        let metadata =
            BonjourMetadata::new("Test Mac", &[7_u8; 32], instance_id).expect("Bonjour metadata");
        let records = metadata.txt_records();

        assert_eq!(
            records.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["instance", "name", "pk", "v"]
        );
        assert_eq!(records["name"], "Test Mac");
        assert_eq!(records["instance"], instance_id.to_string());
        assert!(!records.values().any(|value| value.contains('/')));
        assert_eq!(records["pk"].len(), 64);
    }

    #[test]
    fn rejects_display_names_that_cannot_fit_safely_in_txt_metadata() {
        let error = BonjourMetadata::new("x".repeat(201), &[7_u8; 32], Uuid::new_v4())
            .expect_err("oversized display name should fail");

        assert!(matches!(
            error,
            super::BonjourError::DisplayNameTooLong(201)
        ));
    }
}
