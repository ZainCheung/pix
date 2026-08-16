use std::fs;
use std::path::{Path, PathBuf};

use pix_wire::{
    ClientEnvelope, EncryptedFrameDecoder, MAX_ENCRYPTED_FRAME_BYTES, PAIRING_TOKEN_BYTES,
    RelayRole, ServerEnvelope, confirmation_code, decode_pairing_offer,
    host_public_key_fingerprint, relay_channel_id, relay_channel_secret_from_join_code,
    relay_join_proof, validate_pairing_token,
};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/fixtures/v1")
}

fn fixture_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = fs::read(path).unwrap_or_else(|error| {
        panic!("read {} ({error})", path.display());
    });
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    bytes
}

fn named_fixtures(prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(fixture_root())
        .expect("list fixtures")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(suffix))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[test]
fn every_client_request_fixture_is_canonical() {
    let fixtures = named_fixtures("client-", ".json");
    assert_eq!(fixtures.len(), 17, "one golden file per client request");
    for path in fixtures {
        let bytes = fixture_bytes(&path);
        let decoded = ClientEnvelope::decode(&bytes)
            .unwrap_or_else(|error| panic!("decode {} ({error})", path.display()));
        assert_eq!(
            decoded.encode().expect("re-encode client fixture"),
            bytes,
            "{}",
            path.display()
        );
    }
}

#[test]
fn every_server_event_fixture_is_canonical() {
    let fixtures = named_fixtures("server-", ".json");
    assert_eq!(
        fixtures.len(),
        19,
        "one golden file per server event, plus the relay-bearing host snapshot"
    );
    for path in fixtures {
        let bytes = fixture_bytes(&path);
        let decoded = ServerEnvelope::decode(&bytes)
            .unwrap_or_else(|error| panic!("decode {} ({error})", path.display()));
        assert_eq!(
            decoded.encode().expect("re-encode server fixture"),
            bytes,
            "{}",
            path.display()
        );
    }
}

#[test]
fn reject_fixtures_fail_closed() {
    assert!(
        ClientEnvelope::decode(&fixture_bytes(
            &fixture_root().join("reject-protocol-unsupported.json")
        ))
        .is_err()
    );
    assert!(
        ClientEnvelope::decode(&fixture_bytes(
            &fixture_root().join("reject-empty-session-id.json")
        ))
        .is_err()
    );
}

#[test]
fn pairing_offer_and_token_fixtures_are_canonical() {
    let token = String::from_utf8(fixture_bytes(
        &fixture_root().join("pairing-token-valid.txt"),
    ))
    .expect("token utf8");
    validate_pairing_token(&token).expect("valid token");
    assert_eq!(
        decode_pairing_offer(&fixture_bytes(&fixture_root().join("pairing-offer.json")))
            .expect("canonical offer"),
        token
    );
    assert!(
        decode_pairing_offer(&fixture_bytes(
            &fixture_root().join("pairing-offer-invalid.json")
        ))
        .is_err()
    );
    assert!(
        validate_pairing_token(
            &String::from_utf8(fixture_bytes(
                &fixture_root().join("pairing-token-invalid.txt")
            ))
            .expect("invalid token utf8")
        )
        .is_err()
    );
    assert_eq!(PAIRING_TOKEN_BYTES, 32);
}

#[test]
fn handshake_identity_and_expiry_fixtures_are_stable() {
    let fingerprint: serde_json::Value = serde_json::from_slice(&fixture_bytes(
        &fixture_root().join("host-fingerprint.json"),
    ))
    .expect("fingerprint fixture");
    let public_key = hex::decode_owned(fingerprint["public_key_hex"].as_str().expect("hex"));
    assert_eq!(
        host_public_key_fingerprint(&public_key),
        fingerprint["fingerprint"].as_str().expect("fingerprint")
    );

    let confirmation: serde_json::Value = serde_json::from_slice(&fixture_bytes(
        &fixture_root().join("confirmation-code.json"),
    ))
    .expect("confirmation fixture");
    let transcript = hex::decode_owned(confirmation["transcript_hex"].as_str().expect("hex"));
    assert_eq!(
        confirmation_code(&transcript),
        confirmation["code"].as_str().expect("code")
    );

    let expiry: serde_json::Value =
        serde_json::from_slice(&fixture_bytes(&fixture_root().join("pairing-expiry.json")))
            .expect("expiry fixture");
    assert_eq!(expiry["ttl_seconds"], 120);
    assert_eq!(expiry["single_use"], true);
}

#[test]
fn relay_channel_fixture_matches_derivations() {
    let fixture: serde_json::Value =
        serde_json::from_slice(&fixture_bytes(&fixture_root().join("relay-channel.json")))
            .expect("relay channel fixture");
    let secret = fixture["channel_secret"].as_str().expect("secret");
    assert_eq!(
        relay_channel_id(secret).expect("channel id"),
        fixture["channel_id"].as_str().expect("channel id")
    );
    assert_eq!(
        relay_join_proof(secret, RelayRole::Host).expect("host proof"),
        fixture["host_join_proof"].as_str().expect("host proof")
    );
    assert_eq!(
        relay_join_proof(secret, RelayRole::Client).expect("client proof"),
        fixture["client_join_proof"].as_str().expect("client proof")
    );
}

#[test]
fn relay_join_code_fixture_matches_derivation() {
    let fixture: serde_json::Value =
        serde_json::from_slice(&fixture_bytes(&fixture_root().join("relay-join-code.json")))
            .expect("join code fixture");
    assert_eq!(
        relay_channel_secret_from_join_code(
            fixture["join_code"].as_str().expect("code"),
            fixture["relay_url"].as_str().expect("url"),
        )
        .expect("derived secret"),
        fixture["channel_secret"].as_str().expect("secret")
    );
}

#[test]
fn frame_limit_fixtures_accept_valid_and_reject_tampered_sizes() {
    let mut decoder = EncryptedFrameDecoder::new();
    let frames = decoder
        .push(&fixture_bytes(&fixture_root().join("frame-valid.bin")))
        .expect("valid frame");
    assert_eq!(frames, vec![b"ciphertext-fixture".to_vec()]);

    let mut oversized = EncryptedFrameDecoder::new();
    assert!(
        oversized
            .push(&fixture_bytes(&fixture_root().join("frame-oversized.bin")))
            .is_err()
    );

    let mut empty = EncryptedFrameDecoder::new();
    assert!(
        empty
            .push(&fixture_bytes(&fixture_root().join("frame-empty.bin")))
            .is_err()
    );

    let mut replay = EncryptedFrameDecoder::new();
    let valid = fixture_bytes(&fixture_root().join("frame-valid.bin"));
    assert_eq!(
        replay.push(&valid).expect("first copy").len()
            + replay.push(&valid).expect("replayed copy").len(),
        2
    );
    assert_eq!(
        u32::from_be_bytes(
            fixture_bytes(&fixture_root().join("frame-oversized.bin"))
                .try_into()
                .expect("4-byte prefix")
        ) as usize,
        MAX_ENCRYPTED_FRAME_BYTES + 1
    );
}

mod hex {
    pub fn decode_owned(value: &str) -> Vec<u8> {
        (0..value.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&value[index..index + 2], 16).expect("hex byte"))
            .collect()
    }
}
