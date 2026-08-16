use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ClientEnvelope, EncryptedFrameDecoder, NoiseHandshake, NoisePattern, NoiseTransport,
    PAIRING_TOKEN_BYTES, ServerEnvelope, confirmation_code, encode_encrypted_frame,
    generate_static_keypair, host_public_key_fingerprint,
};

/// Curve25519 identity material for storage in Apple Keychain.
#[derive(Debug, Clone, uniffi::Record)]
pub struct AppleStaticKeyPair {
    pub private_key: Vec<u8>,
    pub public_key: Vec<u8>,
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum AppleNoisePattern {
    PairingXx,
    ReconnectIk,
}

impl From<AppleNoisePattern> for NoisePattern {
    fn from(value: AppleNoisePattern) -> Self {
        match value {
            AppleNoisePattern::PairingXx => Self::PairingXx,
            AppleNoisePattern::ReconnectIk => Self::ReconnectIk,
        }
    }
}

enum ChannelState {
    Handshake(Box<NoiseHandshake>),
    Transport(NoiseTransport),
    Transitioning,
}

/// Swift-facing owner of all Pix Noise handshake and transport state.
///
/// Network.framework carries the returned opaque records. Swift never derives
/// nonces, parses Noise messages, or implements application fragmentation.
#[derive(uniffi::Object)]
pub struct AppleSecureChannel {
    state: Mutex<ChannelState>,
}

/// Swift-facing incremental decoder for the 4-byte ciphertext frame prefix.
#[derive(uniffi::Object)]
pub struct AppleFrameDecoder {
    decoder: Mutex<EncryptedFrameDecoder>,
}

#[uniffi::export]
impl AppleFrameDecoder {
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoder: Mutex::new(EncryptedFrameDecoder::new()),
        }
    }

    /// Consumes arbitrary Network.framework chunks and returns complete opaque
    /// ciphertext records.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] for an empty or oversized declared frame.
    #[allow(clippy::needless_pass_by_value)]
    pub fn push(&self, chunk: Vec<u8>) -> Result<Vec<Vec<u8>>, AppleWireError> {
        Ok(self
            .decoder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(&chunk)?)
    }

    #[must_use]
    pub fn has_partial_frame(&self) -> bool {
        self.decoder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_partial_frame()
    }
}

impl Default for AppleFrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[uniffi::export]
impl AppleSecureChannel {
    /// Creates the Apple-owned initiator state for XX pairing or IK reconnect.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] for invalid private or remote public keys.
    #[uniffi::constructor]
    #[allow(clippy::needless_pass_by_value)]
    pub fn initiator(
        pattern: AppleNoisePattern,
        local_private_key: Vec<u8>,
        remote_public_key: Option<Vec<u8>>,
    ) -> Result<Self, AppleWireError> {
        Ok(Self {
            state: Mutex::new(ChannelState::Handshake(Box::new(
                NoiseHandshake::initiator(
                    pattern.into(),
                    &local_private_key,
                    remote_public_key.as_deref(),
                )?,
            ))),
        })
    }

    /// Produces the next Noise handshake record.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] for invalid state, order, or payload.
    #[allow(clippy::needless_pass_by_value)]
    pub fn write_handshake(&self, payload: Vec<u8>) -> Result<Vec<u8>, AppleWireError> {
        let mut state = self.lock();
        let ChannelState::Handshake(handshake) = &mut *state else {
            return Err(AppleWireError::InvalidState);
        };
        Ok(handshake.write_message(&payload)?)
    }

    /// Authenticates and consumes one Noise handshake record.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] for invalid state, order, or ciphertext.
    #[allow(clippy::needless_pass_by_value)]
    pub fn read_handshake(&self, message: Vec<u8>) -> Result<Vec<u8>, AppleWireError> {
        let mut state = self.lock();
        let ChannelState::Handshake(handshake) = &mut *state else {
            return Err(AppleWireError::InvalidState);
        };
        Ok(handshake.read_message(&message)?)
    }

    pub fn handshake_finished(&self) -> bool {
        matches!(&*self.lock(), ChannelState::Handshake(value) if value.is_handshake_finished())
    }

    /// Returns the transcript hash before transport conversion.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] after conversion to transport state.
    pub fn handshake_hash(&self) -> Result<Vec<u8>, AppleWireError> {
        let state = self.lock();
        let ChannelState::Handshake(handshake) = &*state else {
            return Err(AppleWireError::InvalidState);
        };
        Ok(handshake.handshake_hash().to_vec())
    }

    /// Returns the authenticated remote static key when Noise has revealed it.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] after conversion to transport state.
    pub fn remote_static_key(&self) -> Result<Option<Vec<u8>>, AppleWireError> {
        let state = self.lock();
        let ChannelState::Handshake(handshake) = &*state else {
            return Err(AppleWireError::InvalidState);
        };
        Ok(handshake.remote_static().map(<[u8]>::to_vec))
    }

    /// Irreversibly converts a completed handshake into transport state.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] if the handshake is incomplete or the
    /// channel is already in transport state.
    pub fn start_transport(&self) -> Result<(), AppleWireError> {
        let mut state = self.lock();
        let previous = std::mem::replace(&mut *state, ChannelState::Transitioning);
        let ChannelState::Handshake(handshake) = previous else {
            *state = previous;
            return Err(AppleWireError::InvalidState);
        };
        match handshake.into_transport() {
            Ok(transport) => {
                *state = ChannelState::Transport(transport);
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Encrypts and fragments one application message.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] outside transport state or above limits.
    #[allow(clippy::needless_pass_by_value)]
    pub fn encrypt(&self, plaintext: Vec<u8>) -> Result<Vec<Vec<u8>>, AppleWireError> {
        let mut state = self.lock();
        let ChannelState::Transport(transport) = &mut *state else {
            return Err(AppleWireError::InvalidState);
        };
        Ok(transport.encrypt_message(&plaintext)?)
    }

    /// Authenticates a record and returns a complete reassembled message.
    ///
    /// # Errors
    ///
    /// Returns [`AppleWireError`] for wrong state, tampering, replay, or
    /// invalid fragmentation.
    #[allow(clippy::needless_pass_by_value)]
    pub fn decrypt_record(&self, ciphertext: Vec<u8>) -> Result<Option<Vec<u8>>, AppleWireError> {
        let mut state = self.lock();
        let ChannelState::Transport(transport) = &mut *state else {
            return Err(AppleWireError::InvalidState);
        };
        Ok(transport.decrypt_record(&ciphertext)?)
    }
}

impl AppleSecureChannel {
    fn lock(&self) -> std::sync::MutexGuard<'_, ChannelState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[uniffi::export]
pub fn apple_generate_static_keypair() -> Result<AppleStaticKeyPair, AppleWireError> {
    let pair = generate_static_keypair()?;
    Ok(AppleStaticKeyPair {
        private_key: pair.private_key,
        public_key: pair.public_key,
    })
}

#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_confirmation_code(handshake_hash: Vec<u8>) -> String {
    confirmation_code(&handshake_hash)
}

/// Computes the canonical host fingerprint used by Bonjour and peer checks.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_host_fingerprint(public_key: Vec<u8>) -> Result<String, AppleWireError> {
    if public_key.len() != 32 {
        return Err(AppleWireError::Wire);
    }
    Ok(host_public_key_fingerprint(&public_key))
}

/// Relay join role announced by an Apple client connection.
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum AppleRelayRole {
    Host,
    Client,
}

impl From<AppleRelayRole> for crate::RelayRole {
    fn from(value: AppleRelayRole) -> Self {
        match value {
            AppleRelayRole::Host => Self::Host,
            AppleRelayRole::Client => Self::Client,
        }
    }
}

/// Derives the public rendezvous identifier for one relay channel secret.
///
/// # Errors
///
/// Returns [`AppleWireError`] for a malformed channel secret.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_relay_channel_id(secret: String) -> Result<String, AppleWireError> {
    Ok(crate::relay_channel_id(&secret)?)
}

/// Derives the per-role join proof presented in the relay upgrade request.
///
/// # Errors
///
/// Returns [`AppleWireError`] for a malformed channel secret.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_relay_join_proof(
    secret: String,
    role: AppleRelayRole,
) -> Result<String, AppleWireError> {
    Ok(crate::relay_join_proof(&secret, role.into())?)
}

/// Canonicalizes a typed remote-pairing join code.
///
/// # Errors
///
/// Returns [`AppleWireError`] unless the input is eight Crockford characters
/// after stripping hyphens and spaces.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_normalize_join_code(code: String) -> Result<String, AppleWireError> {
    Ok(crate::normalize_join_code(&code)?)
}

/// Derives the relay channel secret for a typed join code on one relay URL.
///
/// # Errors
///
/// Returns [`AppleWireError`] for a malformed code or empty relay URL.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_relay_channel_secret_from_join_code(
    code: String,
    relay_url: String,
) -> Result<String, AppleWireError> {
    Ok(crate::relay_channel_secret_from_join_code(
        &code, &relay_url,
    )?)
}

/// Decodes the token delivered inside the authenticated XX message 2 payload.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_decode_pairing_offer(payload: Vec<u8>) -> Result<String, AppleWireError> {
    Ok(decode_pairing_offer(&payload)?)
}

/// Adds the Pix 4-byte network-order prefix to one ciphertext record.
///
/// # Errors
///
/// Returns [`AppleWireError`] for an empty or oversized record.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_encode_encrypted_frame(ciphertext: Vec<u8>) -> Result<Vec<u8>, AppleWireError> {
    Ok(encode_encrypted_frame(&ciphertext)?)
}

/// Decodes, validates, and canonically re-encodes a client envelope before it
/// enters the secure channel.
///
/// Swift may construct UI values, but protocol limits and compatibility are
/// always enforced by Rust.
///
/// # Errors
///
/// Returns [`AppleWireError`] for malformed or incompatible protocol data.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_canonical_client_envelope(json: Vec<u8>) -> Result<Vec<u8>, AppleWireError> {
    Ok(ClientEnvelope::decode(&json)?.encode()?)
}

/// Decodes, validates, and canonically re-encodes a server envelope after it
/// leaves the secure channel.
///
/// # Errors
///
/// Returns [`AppleWireError`] for malformed or incompatible protocol data.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_canonical_server_envelope(json: Vec<u8>) -> Result<Vec<u8>, AppleWireError> {
    Ok(ServerEnvelope::decode(&json)?.encode()?)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingOfferPayload {
    token: String,
}

#[derive(Serialize)]
struct PairingIntroduction<'a> {
    token: &'a str,
    device_name: &'a str,
}

/// Produces the encrypted third-message payload for Noise XX pairing.
///
/// # Errors
///
/// Returns [`AppleWireError`] if the payload cannot be encoded.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn apple_pairing_introduction(
    token: String,
    device_name: String,
) -> Result<Vec<u8>, AppleWireError> {
    Ok(pairing_introduction(&token, &device_name)?)
}

/// Encodes the authenticated Noise XX introduction shared by host and Apple
/// clients.
///
/// # Errors
///
/// Returns [`crate::WireError`] if JSON encoding fails.
pub fn pairing_introduction(token: &str, device_name: &str) -> Result<Vec<u8>, crate::WireError> {
    validate_pairing_token(token)?;
    serde_json::to_vec(&PairingIntroduction { token, device_name })
        .map_err(crate::WireError::Encode)
}

/// Encodes the short-lived token into the authenticated XX message 2 payload.
///
/// # Errors
///
/// Returns [`crate::WireError`] for a malformed token or JSON encoding failure.
pub fn pairing_offer(token: &str) -> Result<Vec<u8>, crate::WireError> {
    validate_pairing_token(token)?;
    serde_json::to_vec(&PairingOfferPayload {
        token: token.to_owned(),
    })
    .map_err(crate::WireError::Encode)
}

/// Decodes a canonical authenticated XX message 2 pairing offer.
///
/// # Errors
///
/// Returns [`crate::WireError`] for malformed, non-canonical, or invalid data.
pub fn decode_pairing_offer(payload: &[u8]) -> Result<String, crate::WireError> {
    let offer: PairingOfferPayload =
        serde_json::from_slice(payload).map_err(crate::WireError::Decode)?;
    validate_pairing_token(&offer.token)?;
    if pairing_offer(&offer.token)? != payload {
        return Err(crate::WireError::InvalidPairingToken);
    }
    Ok(offer.token)
}

/// Validates the single-use token representation shared by host and clients.
///
/// # Errors
///
/// Returns [`crate::WireError::InvalidPairingToken`] unless the token is
/// canonical URL-safe base64 for exactly 32 bytes.
pub fn validate_pairing_token(token: &str) -> Result<(), crate::WireError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| crate::WireError::InvalidPairingToken)?;
    if decoded.len() != PAIRING_TOKEN_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != token {
        return Err(crate::WireError::InvalidPairingToken);
    }
    Ok(())
}

#[derive(Debug, Error, uniffi::Error)]
pub enum AppleWireError {
    #[error("secure channel operation is invalid in the current state")]
    InvalidState,
    #[error("secure channel rejected the supplied data")]
    Wire,
}

impl From<crate::WireError> for AppleWireError {
    fn from(_value: crate::WireError) -> Self {
        Self::Wire
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppleFrameDecoder, AppleNoisePattern, AppleSecureChannel, apple_canonical_client_envelope,
        apple_decode_pairing_offer, apple_encode_encrypted_frame, apple_generate_static_keypair,
        decode_pairing_offer, pairing_introduction, pairing_offer,
    };
    use crate::{NoiseHandshake, NoisePattern};
    use serde_json::json;

    #[test]
    fn apple_boundary_completes_xx_and_transports_fragmented_data() {
        let initiator_key = apple_generate_static_keypair().expect("initiator key");
        let responder_key = apple_generate_static_keypair().expect("responder key");
        let initiator = AppleSecureChannel::initiator(
            AppleNoisePattern::PairingXx,
            initiator_key.private_key,
            None,
        )
        .expect("initiator");
        let mut responder =
            NoiseHandshake::responder(NoisePattern::PairingXx, &responder_key.private_key)
                .expect("responder");
        let first = initiator.write_handshake(Vec::new()).expect("first");
        responder.read_message(&first).expect("read first");
        let second = responder.write_message(b"").expect("second");
        initiator.read_handshake(second).expect("read second");
        let third = initiator.write_handshake(Vec::new()).expect("third");
        responder.read_message(&third).expect("read third");
        initiator.start_transport().expect("transport");
        let mut responder = responder.into_transport().expect("responder transport");
        let plaintext = vec![9_u8; 200_000];
        let mut decoded = None;
        for record in initiator.encrypt(plaintext.clone()).expect("encrypt") {
            decoded = responder
                .decrypt_record(&record)
                .expect("decrypt")
                .or(decoded);
        }
        assert_eq!(decoded, Some(plaintext));
    }

    #[test]
    fn apple_boundary_owns_framing_and_protocol_validation() {
        let frame = apple_encode_encrypted_frame(b"ciphertext".to_vec()).expect("frame");
        let decoder = AppleFrameDecoder::new();
        assert!(
            decoder
                .push(frame[..3].to_vec())
                .expect("prefix")
                .is_empty()
        );
        assert!(decoder.has_partial_frame());
        assert_eq!(
            decoder.push(frame[3..].to_vec()).expect("payload"),
            vec![b"ciphertext".to_vec()]
        );

        let envelope = serde_json::to_vec(&json!({
            "protocol": 1,
            "request_id": 9,
            "type": "host.snapshot"
        }))
        .expect("json");
        assert_eq!(
            crate::ClientEnvelope::decode(
                &apple_canonical_client_envelope(envelope).expect("canonical envelope")
            )
            .expect("decode envelope")
            .request_id,
            9
        );
    }

    #[test]
    fn pairing_offer_is_canonical_and_shared_with_apple() {
        let token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let payload = pairing_offer(token).expect("pairing offer");
        assert_eq!(decode_pairing_offer(&payload).expect("decode offer"), token);
        assert_eq!(
            apple_decode_pairing_offer(payload).expect("Apple decodes offer"),
            token
        );
        assert!(pairing_introduction(token, "Test iPhone").is_ok());
        assert!(
            decode_pairing_offer(br#"{ "token":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#)
                .is_err()
        );
        assert!(pairing_introduction("not-a-token", "Test iPhone").is_err());
    }
}
