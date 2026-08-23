use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use blake2::{Blake2s256, Digest};
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::config::{ConfigError, ConfigStore, DeviceRecord};
use crate::connection_manager::{ConnectionRegistry, ConnectionRegistryError};

const PAIRING_TTL: Duration = Duration::from_secs(120);
const STATIC_PUBLIC_KEY_BYTES: usize = 32;
pub const MAX_PENDING_PAIRING_OFFERS: usize = 64;
const RELAY_CHANNEL_BYTES: usize = 32;
const MAX_DEVICE_NAME_CHARS: usize = 80;

/// Secret, short-lived bearer value presented by a local/QR pairing flow.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingToken(String);

impl PairingToken {
    /// Parses a token received from a pairing transport.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] unless it is canonical URL-safe base64 for
    /// exactly 32 random bytes.
    pub fn parse_exposed(value: &str) -> Result<Self, PairingError> {
        if pix_wire::validate_pairing_token(value).is_err() {
            return Err(PairingError::MalformedToken);
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PairingToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PairingToken([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingOffer {
    pub token: PairingToken,
    pub expires_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPending {
    pub id: Uuid,
    pub device_name: String,
    pub confirmation_code: String,
    pub expires_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedDevice {
    pub id: String,
    pub name: String,
    pub public_key: Vec<u8>,
    pub paired_at: DateTime<Utc>,
}

/// Durable device revocation plus best-effort live socket cleanup.
pub struct DeviceRevocation {
    pub device: ApprovedDevice,
    pub closed_connections: usize,
    pub connection_cleanup_failed: bool,
}

#[derive(Clone)]
struct PendingState {
    summary: PairingPending,
    public_key: Vec<u8>,
}

#[derive(Default)]
struct PairingState {
    offers: HashMap<[u8; 32], SystemTime>,
    pending: HashMap<Uuid, PendingState>,
}

/// Host-side coordinator for explicit, single-use device pairing approval.
pub struct PairingCoordinator {
    config_store: ConfigStore,
    state: Mutex<PairingState>,
    persistence: Mutex<()>,
}

impl PairingCoordinator {
    #[must_use]
    pub fn new(config_store: ConfigStore) -> Self {
        Self {
            config_store,
            state: Mutex::new(PairingState::default()),
            persistence: Mutex::new(()),
        }
    }

    /// Issues a cryptographically random token valid for exactly two minutes.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] if secure randomness is unavailable.
    pub fn issue_offer(&self, now: SystemTime) -> Result<PairingOffer, PairingError> {
        let token_bytes = random_bytes::<{ pix_wire::PAIRING_TOKEN_BYTES }>()?;
        let token = PairingToken(URL_SAFE_NO_PAD.encode(token_bytes));
        let expires_at = now
            .checked_add(PAIRING_TTL)
            .ok_or(PairingError::ClockOverflow)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        purge_expired(&mut state, now);
        if state.offers.len().saturating_add(state.pending.len()) >= MAX_PENDING_PAIRING_OFFERS {
            return Err(PairingError::OfferCapacity);
        }
        state
            .offers
            .insert(token_digest(token.expose()), expires_at);
        Ok(PairingOffer { token, expires_at })
    }

    /// Invalidates an offer that can no longer complete its connection.
    pub fn invalidate_offer(&self, token: &PairingToken) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .offers
            .remove(&token_digest(token.expose()))
            .is_some()
    }

    /// Consumes one token after a completed Noise XX handshake and creates a
    /// host-approval request with the transcript-derived confirmation code.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] for invalid/expired/replayed tokens, malformed
    /// device identity, or invalid display names. A token is consumed on the
    /// first attempt regardless of whether later validation succeeds.
    pub fn begin_approval(
        &self,
        token: &PairingToken,
        device_name: impl Into<String>,
        remote_static_public_key: &[u8],
        handshake_hash: &[u8],
        now: SystemTime,
    ) -> Result<PairingPending, PairingError> {
        let digest = token_digest(token.expose());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        purge_expired(&mut state, now);
        let expires_at = state
            .offers
            .remove(&digest)
            .ok_or(PairingError::InvalidOrExpiredToken)?;
        if now >= expires_at {
            return Err(PairingError::InvalidOrExpiredToken);
        }
        let device_name = device_name.into();
        let device_name = device_name.trim();
        if device_name.is_empty()
            || device_name.chars().count() > MAX_DEVICE_NAME_CHARS
            || device_name.chars().any(|character| {
                character.is_control()
                    || matches!(
                        character,
                        '\u{202a}'..='\u{202e}'
                            | '\u{2066}'..='\u{2069}'
                            | '\u{200b}'..='\u{200f}'
                    )
            })
        {
            return Err(PairingError::InvalidDeviceName);
        }
        if remote_static_public_key.len() != STATIC_PUBLIC_KEY_BYTES {
            return Err(PairingError::InvalidPublicKeyLength(
                remote_static_public_key.len(),
            ));
        }
        if handshake_hash.is_empty() {
            return Err(PairingError::EmptyHandshakeHash);
        }
        let summary = PairingPending {
            id: Uuid::new_v4(),
            device_name: device_name.to_owned(),
            confirmation_code: pix_wire::confirmation_code(handshake_hash),
            expires_at,
        };
        state.pending.insert(
            summary.id,
            PendingState {
                summary: summary.clone(),
                public_key: remote_static_public_key.to_vec(),
            },
        );
        Ok(summary)
    }

    /// Explicitly approves a pending pairing and atomically persists trust.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] for unknown/expired requests, duplicate device
    /// identities, randomness failure, or configuration persistence failure.
    pub fn approve(
        &self,
        pending_id: Uuid,
        now: SystemTime,
    ) -> Result<ApprovedDevice, PairingError> {
        let pending = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            purge_expired(&mut state, now);
            state
                .pending
                .remove(&pending_id)
                .ok_or(PairingError::UnknownOrExpiredApproval)?
        };
        if now >= pending.summary.expires_at {
            return Err(PairingError::UnknownOrExpiredApproval);
        }
        let device_id = device_fingerprint(&pending.public_key);
        let paired_at = DateTime::<Utc>::from(now);
        let relay_channel = URL_SAFE_NO_PAD.encode(random_bytes::<RELAY_CHANNEL_BYTES>()?);
        let _persistence = self
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let transaction = self.config_store.transaction()?;
        let mut config = transaction.load()?;
        if let Some(existing) = config.devices.iter().find(|device| device.id == device_id) {
            let public_key = STANDARD
                .decode(&existing.public_key)
                .map_err(PairingError::InvalidStoredPublicKey)?;
            if public_key != pending.public_key {
                return Err(PairingError::AlreadyPaired(device_id));
            }
            return Ok(ApprovedDevice {
                id: existing.id.clone(),
                name: existing.name.clone(),
                public_key,
                paired_at: existing.paired_at,
            });
        }
        config.devices.push(DeviceRecord {
            id: device_id.clone(),
            name: pending.summary.device_name.clone(),
            public_key: STANDARD.encode(&pending.public_key),
            relay_channel,
            paired_at,
            unknown: serde_json::Map::new(),
        });
        transaction.save(&config)?;
        Ok(ApprovedDevice {
            id: device_id,
            name: pending.summary.device_name,
            public_key: pending.public_key,
            paired_at,
        })
    }

    /// Rejects a pending pairing without persisting device trust.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] if the request is unknown or already consumed.
    pub fn reject(&self, pending_id: Uuid) -> Result<(), PairingError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(&pending_id)
            .map(|_| ())
            .ok_or(PairingError::UnknownOrExpiredApproval)
    }

    /// Authenticates the remote static key revealed by a completed IK handshake.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] for unknown, revoked, or malformed peer keys.
    pub fn authenticate_peer(
        &self,
        remote_static_public_key: &[u8],
    ) -> Result<ApprovedDevice, PairingError> {
        if remote_static_public_key.len() != STATIC_PUBLIC_KEY_BYTES {
            return Err(PairingError::InvalidPublicKeyLength(
                remote_static_public_key.len(),
            ));
        }
        let device_id = device_fingerprint(remote_static_public_key);
        let _persistence = self
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config = self.config_store.load()?;
        let record = config
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .ok_or(PairingError::UnknownPeer)?;
        let public_key = STANDARD
            .decode(&record.public_key)
            .map_err(PairingError::InvalidStoredPublicKey)?;
        if public_key != remote_static_public_key {
            return Err(PairingError::UnknownPeer);
        }
        Ok(ApprovedDevice {
            id: record.id.clone(),
            name: record.name.clone(),
            public_key,
            paired_at: record.paired_at,
        })
    }

    /// Lists durable paired-device records without exposing live sockets.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] when the host configuration cannot be loaded.
    pub fn list_devices(&self) -> Result<Vec<DeviceRecord>, PairingError> {
        Ok(self.config_store.load()?.devices)
    }

    /// Reloads the durable host configuration after a trust mutation so
    /// in-memory views can be refreshed.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] when the host configuration cannot be loaded.
    pub fn current_config(&self) -> Result<crate::config::HostConfig, PairingError> {
        Ok(self.config_store.load()?)
    }

    /// Irreversibly removes a paired device record.
    ///
    /// The caller must close matching live connections after this succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] for unknown devices or persistence failures.
    pub fn revoke(&self, device_id: &str) -> Result<ApprovedDevice, PairingError> {
        let _persistence = self
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let transaction = self.config_store.transaction()?;
        let mut config = transaction.load()?;
        let index = config
            .devices
            .iter()
            .position(|device| device.id == device_id)
            .ok_or(PairingError::UnknownPeer)?;
        let record = config.devices.remove(index);
        let public_key = STANDARD
            .decode(&record.public_key)
            .map_err(PairingError::InvalidStoredPublicKey)?;
        transaction.save(&config)?;
        Ok(ApprovedDevice {
            id: record.id,
            name: record.name,
            public_key,
            paired_at: record.paired_at,
        })
    }

    /// Persists revocation and immediately closes all matching live sockets.
    ///
    /// The registry is marked before the config mutation so a reconnect racing
    /// with revocation cannot be admitted. If persistence fails, the in-memory
    /// barrier remains fail-closed until process restart or explicit repair.
    /// A socket shutdown error is reported only after durable revocation has
    /// been attempted, so a transient OS error cannot leave the device trusted.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] for unknown devices, persistence failures, or
    /// socket shutdown failures.
    pub fn revoke_and_disconnect(
        &self,
        device_id: &str,
        registry: &ConnectionRegistry,
    ) -> Result<DeviceRevocation, PairingError> {
        let disconnect_result = registry.revoke_device(device_id);
        let device = self.revoke(device_id)?;
        let (closed_connections, connection_cleanup_failed) = match disconnect_result {
            Ok(closed) => (closed, false),
            Err(_) => (0, true),
        };
        Ok(DeviceRevocation {
            device,
            closed_connections,
            connection_cleanup_failed,
        })
    }
}

fn purge_expired(state: &mut PairingState, now: SystemTime) {
    state.offers.retain(|_, expires_at| now < *expires_at);
    state
        .pending
        .retain(|_, pending| now < pending.summary.expires_at);
}

fn token_digest(token: &str) -> [u8; 32] {
    Blake2s256::digest([b"Pix pairing token v1".as_slice(), token.as_bytes()].concat()).into()
}

fn device_fingerprint(public_key: &[u8]) -> String {
    let digest = Blake2s256::digest([b"Pix device fingerprint v1".as_slice(), public_key].concat());
    digest
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
}

fn random_bytes<const N: usize>() -> Result<[u8; N], PairingError> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(PairingError::Random)?;
    Ok(bytes)
}

#[derive(Debug, Error)]
pub enum PairingError {
    #[error("secure randomness is unavailable: {0}")]
    Random(getrandom::Error),
    #[error("pairing expiry timestamp overflowed")]
    ClockOverflow,
    #[error("pairing token is invalid, expired, or already used")]
    InvalidOrExpiredToken,
    #[error("pairing token encoding is malformed")]
    MalformedToken,
    #[error("too many pairing handshakes are awaiting completion")]
    OfferCapacity,
    #[error("pairing approval is unknown, expired, or already handled")]
    UnknownOrExpiredApproval,
    #[error("device name cannot be empty")]
    InvalidDeviceName,
    #[error("device public key is {0} bytes; expected 32")]
    InvalidPublicKeyLength(usize),
    #[error("Noise handshake hash cannot be empty")]
    EmptyHandshakeHash,
    #[error("device is already paired: {0}")]
    AlreadyPaired(String),
    #[error("device is unknown or revoked")]
    UnknownPeer,
    #[error("stored device public key is invalid: {0}")]
    InvalidStoredPublicKey(base64::DecodeError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    ConnectionRegistry(#[from] ConnectionRegistryError),
}
