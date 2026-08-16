//! Shared Pix wire-format and secure transport boundary.
//!
//! The host and Apple clients consume this Rust implementation. Swift does not
//! independently implement framing, protocol validation, or cryptography.

mod ffi;
mod frame;
mod noise;
mod protocol;
mod relay;

pub use ffi::{
    AppleFrameDecoder, AppleNoisePattern, AppleRelayRole, AppleSecureChannel, AppleStaticKeyPair,
    AppleWireError, decode_pairing_offer, pairing_introduction, pairing_offer,
    validate_pairing_token,
};
pub use frame::{EncryptedFrameDecoder, encode_encrypted_frame};
pub use noise::{
    NoiseHandshake, NoisePattern, NoiseTransport, StaticKeyPair, confirmation_code,
    generate_static_keypair, host_public_key_fingerprint, static_public_key,
};
pub use protocol::{
    ClientEnvelope, ClientRequest, CompactionEvent, ErrorCode, ExtensionUiAnswer,
    ExtensionUiRequest, HostModelDefaults, HostSnapshot, HostSummary, ModelSummary, PromptBehavior,
    RelayAccess, ServerEnvelope, ServerEvent, SessionSnapshot, SessionState, SessionSummary,
    ThinkingLevel, ToolEvent, WorkspaceAvailability, WorkspaceSummary,
};
pub use relay::{
    RELAY_CHANNEL_SECRET_BYTES, RelayRole, decode_relay_channel_secret, generate_join_code,
    normalize_join_code, relay_channel_id, relay_channel_secret_from_join_code, relay_join_proof,
};

pub const PROTOCOL_MAJOR: u16 = 1;
pub const MAX_ENCRYPTED_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_TEXT_FIELD_BYTES: usize = 512 * 1024;
pub const MAX_PENDING_REQUESTS: usize = 128;
pub const PAIRING_TOKEN_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("unsupported protocol major {found}; this build supports {supported}")]
    ProtocolVersion { found: u16, supported: u16 },
    #[error("frame is {0} bytes, exceeding the 1 MiB limit")]
    FrameTooLarge(usize),
    #[error("encrypted frame may not be empty")]
    EmptyFrame,
    #[error("text field {field} is {size} bytes, exceeding the 512 KiB limit")]
    TextTooLarge { field: &'static str, size: usize },
    #[error("required identifier {0} is empty")]
    EmptyIdentifier(&'static str),
    #[error("pairing token must be canonical URL-safe base64 for 32 bytes")]
    InvalidPairingToken,
    #[error("relay channel secret must be canonical URL-safe base64 for 32 bytes")]
    InvalidRelayChannelSecret,
    #[error("remote pairing join code must be eight Crockford characters")]
    InvalidJoinCode,
    #[error("secure randomness is unavailable")]
    Randomness,
    #[error("static key must be 32 bytes; found {0}")]
    InvalidStaticKeyLength(usize),
    #[error("failed to encode protocol message: {0}")]
    Encode(serde_json::Error),
    #[error("failed to decode protocol message: {0}")]
    Decode(serde_json::Error),
    #[error("Noise protocol failure: {0}")]
    Noise(#[from] snow::Error),
    #[error("Noise handshake is not ready for transport mode")]
    HandshakeIncomplete,
    #[error("Noise transport received an invalid message fragment")]
    InvalidFragment,
    #[error("reassembled plaintext exceeds the 1 MiB message limit")]
    ReassemblyTooLarge,
}

uniffi::setup_scaffolding!();
