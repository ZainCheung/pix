use std::time::{Duration, SystemTime};

use pix_core::{ConfigStore, HostConfig, MAX_PENDING_PAIRING_OFFERS, PairingCoordinator};
use pix_wire::{NoiseHandshake, NoisePattern, generate_static_keypair};
use tempfile::tempdir;

fn coordinator() -> (tempfile::TempDir, PairingCoordinator, ConfigStore) {
    let directory = tempdir().expect("temporary config directory");
    let store = ConfigStore::new(directory.path().join("config.json"));
    store
        .save(&HostConfig::new("Pairing host"))
        .expect("initial config");
    let coordinator = PairingCoordinator::new(store.clone());
    (directory, coordinator, store)
}

#[test]
fn requires_explicit_approval_before_persisting_a_device() {
    let (_directory, coordinator, store) = coordinator();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    let offer = coordinator.issue_offer(now).expect("pairing offer");
    let public_key = [7_u8; 32];
    let pending = coordinator
        .begin_approval(&offer.token, "Zain's iPhone", &public_key, &[9_u8; 32], now)
        .expect("pending approval");

    assert_eq!(store.load().expect("unapproved config").devices.len(), 0);
    assert_eq!(pending.confirmation_code.len(), 6);
    let approved = coordinator.approve(pending.id, now).expect("host approval");
    assert_eq!(approved.public_key, public_key);
    assert_eq!(store.load().expect("approved config").devices.len(), 1);
    assert_eq!(coordinator.list_devices().expect("list devices").len(), 1);
    assert_eq!(
        coordinator
            .authenticate_peer(&public_key)
            .expect("known IK peer")
            .id,
        approved.id
    );

    let retry_offer = coordinator.issue_offer(now).expect("retry offer");
    let retry = coordinator
        .begin_approval(
            &retry_offer.token,
            "Zain's iPhone",
            &public_key,
            &[9_u8; 32],
            now,
        )
        .expect("retry pending");
    let retried = coordinator
        .approve(retry.id, now)
        .expect("repeat approval of the same phone");
    assert_eq!(retried.id, approved.id);
    assert_eq!(store.load().expect("still one device").devices.len(), 1);
}

#[test]
fn token_is_single_use_and_expires_after_two_minutes() {
    let (_directory, coordinator, _store) = coordinator();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000);
    let offer = coordinator.issue_offer(now).expect("pairing offer");
    coordinator
        .begin_approval(&offer.token, "Phone", &[1_u8; 32], &[2_u8; 32], now)
        .expect("first use");
    assert!(
        coordinator
            .begin_approval(&offer.token, "Phone", &[1_u8; 32], &[2_u8; 32], now)
            .is_err()
    );

    let expired = coordinator.issue_offer(now).expect("second offer");
    let after_expiry = now + Duration::from_secs(120);
    assert!(
        coordinator
            .begin_approval(
                &expired.token,
                "Phone",
                &[1_u8; 32],
                &[2_u8; 32],
                after_expiry,
            )
            .is_err()
    );
}

#[test]
fn rejected_or_revoked_device_cannot_authenticate() {
    let (_directory, coordinator, store) = coordinator();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(30_000);
    let rejected_offer = coordinator.issue_offer(now).expect("rejected offer");
    let rejected = coordinator
        .begin_approval(
            &rejected_offer.token,
            "Rejected Phone",
            &[3_u8; 32],
            &[4_u8; 32],
            now,
        )
        .expect("rejected pending");
    coordinator.reject(rejected.id).expect("reject pairing");
    assert!(coordinator.authenticate_peer(&[3_u8; 32]).is_err());

    let approved_offer = coordinator.issue_offer(now).expect("approved offer");
    let pending = coordinator
        .begin_approval(
            &approved_offer.token,
            "Revoked Phone",
            &[5_u8; 32],
            &[6_u8; 32],
            now,
        )
        .expect("approved pending");
    let approved = coordinator
        .approve(pending.id, now)
        .expect("approve pairing");
    coordinator.revoke(&approved.id).expect("revoke device");
    assert!(coordinator.authenticate_peer(&[5_u8; 32]).is_err());
    assert!(store.load().expect("revoked config").devices.is_empty());
}

#[test]
fn pairing_token_debug_output_never_exposes_secret() {
    let (_directory, coordinator, _store) = coordinator();
    let offer = coordinator
        .issue_offer(SystemTime::UNIX_EPOCH)
        .expect("pairing offer");
    let debug = format!("{:?}", offer.token);
    assert_eq!(debug, "PairingToken([redacted])");
    assert!(!debug.contains(offer.token.expose()));
}

#[test]
fn pending_offer_capacity_is_bounded_and_recoverable() {
    let (_directory, coordinator, _store) = coordinator();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(35_000);
    let mut offers = Vec::new();
    for _ in 0..MAX_PENDING_PAIRING_OFFERS {
        offers.push(coordinator.issue_offer(now).expect("bounded offer"));
    }
    assert!(coordinator.issue_offer(now).is_err());

    assert!(coordinator.invalidate_offer(&offers[0].token));
    assert!(coordinator.issue_offer(now).is_ok());
}

#[test]
fn xx_pairing_approval_authorizes_the_same_static_key_for_ik_reconnect() {
    let (_directory, coordinator, _store) = coordinator();
    let host = generate_static_keypair().expect("host static identity");
    let phone = generate_static_keypair().expect("phone static identity");
    let mut phone_xx = NoiseHandshake::initiator(NoisePattern::PairingXx, &phone.private_key, None)
        .expect("phone XX initiator");
    let mut host_xx = NoiseHandshake::responder(NoisePattern::PairingXx, &host.private_key)
        .expect("host XX responder");
    let message_1 = phone_xx.write_message(b"").expect("XX message 1");
    host_xx.read_message(&message_1).expect("host reads XX 1");
    let message_2 = host_xx.write_message(b"").expect("XX message 2");
    phone_xx.read_message(&message_2).expect("phone reads XX 2");
    let message_3 = phone_xx.write_message(b"").expect("XX message 3");
    host_xx.read_message(&message_3).expect("host reads XX 3");
    let remote_phone_key = host_xx
        .remote_static()
        .expect("XX remote phone key")
        .to_vec();
    assert_eq!(remote_phone_key, phone.public_key);

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(40_000);
    let offer = coordinator.issue_offer(now).expect("pairing offer");
    let pending = coordinator
        .begin_approval(
            &offer.token,
            "Noise Phone",
            &remote_phone_key,
            host_xx.handshake_hash(),
            now,
        )
        .expect("pending host approval");
    coordinator.approve(pending.id, now).expect("approve phone");

    let mut phone_ik = NoiseHandshake::initiator(
        NoisePattern::ReconnectIk,
        &phone.private_key,
        Some(&host.public_key),
    )
    .expect("phone IK initiator");
    let mut host_ik = NoiseHandshake::responder(NoisePattern::ReconnectIk, &host.private_key)
        .expect("host IK responder");
    let ik_message_1 = phone_ik.write_message(b"").expect("IK message 1");
    host_ik
        .read_message(&ik_message_1)
        .expect("host reads IK message 1");
    let authenticated_key = host_ik.remote_static().expect("IK remote phone key");
    let authenticated = coordinator
        .authenticate_peer(authenticated_key)
        .expect("approved IK peer");
    assert_eq!(authenticated.public_key, phone.public_key);
    let ik_message_2 = host_ik.write_message(b"").expect("IK message 2");
    phone_ik
        .read_message(&ik_message_2)
        .expect("phone reads IK message 2");
    assert!(phone_ik.is_handshake_finished());
    assert!(host_ik.is_handshake_finished());
}
