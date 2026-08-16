//! Relay rendezvous derivations shared by the host, Apple clients, and tests.
//!
//! A relay channel is identified by a 32-byte secret created on the host. The
//! secret itself never reaches the relay: both endpoints derive the public
//! rendezvous identifier and per-role join proofs locally, and the relay only
//! compares the resulting opaque hex strings. Knowledge of the channel
//! identifier alone is never sufficient to join an established channel, and
//! none of the derived values can decrypt application frames, which remain
//! protected end-to-end by the Noise transport.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use blake2::{Blake2s256, Digest};
use std::fmt::Write as _;

use crate::WireError;

/// Fixed byte length of every relay channel secret.
pub const RELAY_CHANNEL_SECRET_BYTES: usize = 32;

/// Connection role announced to the relay when joining a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayRole {
    Host,
    Client,
}

impl RelayRole {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Client => "client",
        }
    }
}

/// Decodes the canonical URL-safe base64 representation of a channel secret.
///
/// # Errors
///
/// Returns [`WireError::InvalidRelayChannelSecret`] unless the input is
/// canonical URL-safe base64 for exactly 32 bytes.
pub fn decode_relay_channel_secret(
    secret: &str,
) -> Result<[u8; RELAY_CHANNEL_SECRET_BYTES], WireError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(secret)
        .map_err(|_| WireError::InvalidRelayChannelSecret)?;
    if decoded.len() != RELAY_CHANNEL_SECRET_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != secret {
        return Err(WireError::InvalidRelayChannelSecret);
    }
    let mut bytes = [0_u8; RELAY_CHANNEL_SECRET_BYTES];
    bytes.copy_from_slice(&decoded);
    Ok(bytes)
}

/// Derives the public rendezvous identifier for one relay channel secret.
///
/// The identifier addresses one Durable Object and appears in relay request
/// paths and payload-free logs. It cannot be reversed into the secret.
///
/// # Errors
///
/// Returns [`WireError::InvalidRelayChannelSecret`] for a malformed secret.
pub fn relay_channel_id(secret: &str) -> Result<String, WireError> {
    let bytes = decode_relay_channel_secret(secret)?;
    Ok(lower_hex(&Blake2s256::digest(
        [b"Pix relay channel v1".as_slice(), &bytes].concat(),
    )))
}

/// Derives the per-role join proof presented in the relay upgrade request.
///
/// The relay pins the first proof it observes for each role of a channel and
/// requires the same value afterwards, so a leaked channel identifier alone
/// does not admit an attacker into an established channel.
///
/// # Errors
///
/// Returns [`WireError::InvalidRelayChannelSecret`] for a malformed secret.
pub fn relay_join_proof(secret: &str, role: RelayRole) -> Result<String, WireError> {
    let bytes = decode_relay_channel_secret(secret)?;
    Ok(lower_hex(&Blake2s256::digest(
        [
            b"Pix relay join v1".as_slice(),
            role.label().as_bytes(),
            &bytes,
        ]
        .concat(),
    )))
}

/// Random bytes that encode to one eight-character Crockford join code.
const JOIN_CODE_BYTES: usize = 5;
/// Displayed join-code length, excluding the hyphen.
const JOIN_CODE_CHARS: usize = 8;
const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Generates a typable remote-pairing join code `XXXX-XXXX`.
///
/// The code is 40 bits of Crockford Base32. Combined with the two-minute
/// pairing window it is enough to address a rendezvous channel without a
/// camera, while the Noise confirmation and host approval stay authoritative.
///
/// # Errors
///
/// Returns [`WireError::Randomness`] when the OS CSPRNG is unavailable.
pub fn generate_join_code() -> Result<String, WireError> {
    let mut bytes = [0_u8; JOIN_CODE_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| WireError::Randomness)?;
    Ok(format_join_code(encode_crockford(bytes)))
}

/// Canonicalizes a user-typed join code: strips separators, uppercases, and
/// maps visually ambiguous Crockford letters (`I`/`L` → `1`, `O` → `0`).
///
/// # Errors
///
/// Returns [`WireError::InvalidJoinCode`] unless the result is exactly eight
/// Crockford characters.
pub fn normalize_join_code(input: &str) -> Result<String, WireError> {
    let mut characters = String::with_capacity(JOIN_CODE_CHARS);
    for character in input.chars() {
        if character == '-' || character.is_ascii_whitespace() {
            continue;
        }
        let mapped = match character.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            candidate @ ('0'..='9'
            | 'A'..='H'
            | 'J'
            | 'K'
            | 'M'
            | 'N'
            | 'P'..='T'
            | 'V'..='Z') => candidate,
            _ => return Err(WireError::InvalidJoinCode),
        };
        characters.push(mapped);
        if characters.len() > JOIN_CODE_CHARS {
            return Err(WireError::InvalidJoinCode);
        }
    }
    if characters.len() != JOIN_CODE_CHARS {
        return Err(WireError::InvalidJoinCode);
    }
    Ok(characters)
}

/// Derives the canonical channel secret for a join code on one relay URL.
///
/// The same code on a different relay URL produces a different secret, so a
/// leaked code cannot be replayed against another endpoint. The secret is
/// never shown to the user; they type the join code.
///
/// # Errors
///
/// Returns [`WireError::InvalidJoinCode`] for a malformed code or empty URL.
pub fn relay_channel_secret_from_join_code(
    code: &str,
    relay_url: &str,
) -> Result<String, WireError> {
    if relay_url.is_empty() {
        return Err(WireError::InvalidJoinCode);
    }
    let normalized = normalize_join_code(code)?;
    let digest = Blake2s256::digest(
        [
            b"Pix remote join v1".as_slice(),
            normalized.as_bytes(),
            b"\0",
            relay_url.as_bytes(),
        ]
        .concat(),
    );
    Ok(URL_SAFE_NO_PAD.encode(digest))
}

fn encode_crockford(bytes: [u8; JOIN_CODE_BYTES]) -> [u8; JOIN_CODE_CHARS] {
    let mut value = 0_u64;
    for byte in bytes {
        value = (value << 8) | u64::from(byte);
    }
    let mut encoded = [0_u8; JOIN_CODE_CHARS];
    for slot in encoded.iter_mut().rev() {
        *slot = CROCKFORD[(value & 31) as usize];
        value >>= 5;
    }
    encoded
}

fn format_join_code(characters: [u8; JOIN_CODE_CHARS]) -> String {
    format!(
        "{}-{}",
        std::str::from_utf8(&characters[..4]).expect("crockford ascii"),
        std::str::from_utf8(&characters[4..]).expect("crockford ascii")
    )
}

fn lower_hex(digest: &[u8]) -> String {
    digest
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
}

#[cfg(test)]
mod tests {
    use super::{
        RelayRole, decode_relay_channel_secret, generate_join_code, normalize_join_code,
        relay_channel_id, relay_channel_secret_from_join_code, relay_join_proof,
    };
    use crate::WireError;

    const SECRET: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn derivations_are_deterministic_and_distinct() {
        let channel = relay_channel_id(SECRET).expect("channel id");
        let host = relay_join_proof(SECRET, RelayRole::Host).expect("host proof");
        let client = relay_join_proof(SECRET, RelayRole::Client).expect("client proof");

        assert_eq!(channel.len(), 64);
        assert_eq!(host.len(), 64);
        assert_eq!(client.len(), 64);
        assert_ne!(channel, host);
        assert_ne!(channel, client);
        assert_ne!(host, client);
        assert_eq!(relay_channel_id(SECRET).expect("stable"), channel);
    }

    #[test]
    fn rejects_non_canonical_secrets() {
        assert!(matches!(
            decode_relay_channel_secret("too-short"),
            Err(WireError::InvalidRelayChannelSecret)
        ));
        // Same bytes with padding is not canonical.
        assert!(matches!(
            relay_channel_id("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            Err(WireError::InvalidRelayChannelSecret)
        ));
        assert!(matches!(
            relay_join_proof("", RelayRole::Host),
            Err(WireError::InvalidRelayChannelSecret)
        ));
    }

    #[test]
    fn join_codes_normalize_and_derive_stable_secrets() {
        assert_eq!(normalize_join_code("ab1o-il23").expect("ok"), "AB101123");
        assert_eq!(
            relay_channel_secret_from_join_code("AB10-1123", "wss://relay.example")
                .expect("secret"),
            relay_channel_secret_from_join_code("ab1o il23", "wss://relay.example")
                .expect("same secret")
        );
        assert_ne!(
            relay_channel_secret_from_join_code("AB10-1123", "wss://relay.example")
                .expect("a"),
            relay_channel_secret_from_join_code("AB10-1123", "ws://127.0.0.1:8791").expect("b")
        );
        assert!(matches!(
            normalize_join_code("short"),
            Err(WireError::InvalidJoinCode)
        ));
        let code = generate_join_code().expect("random code");
        assert_eq!(code.len(), 9);
        assert_eq!(code.as_bytes()[4], b'-');
        assert!(relay_channel_secret_from_join_code(&code, "wss://r").is_ok());
    }
}
