//! End-to-end remote pairing over a real local relay Worker (wrangler dev).
//!
//! These scenarios reproduce the exact field failures seen during Release 2
//! bring-up: pairing through the relay bridge, replaced pairing channels
//! (the double-QR bug), and approval interrupted by a dropped route.

mod e2e_support;

use std::time::Duration;

use e2e_support::{Link, Phone, Relay, Serve, join_code, parse_pair_payload, qr_payload};
use tempfile::tempdir;

#[test]
#[allow(clippy::too_many_lines)]
fn remote_pairing_end_to_end_over_a_real_relay() {
    let relay = Relay::start(8791);
    let directory = tempdir().expect("config dir");
    let mut serve = Serve::start(directory.path(), Some(&relay.url));
    serve.wait_event("relay_configured", |_| true);

    // --- Scenario: QR pairing through the relay bridge --------------------
    // The QR event is the contract the Mac UI waits for. After that the
    // phone may join immediately — the host is already on the channel.
    eprintln!("=== scenario: remote QR pairing ===");
    serve.command("pair-remote");
    let offer = serve.wait_event("remote_pairing_ready", |_| true);
    let payload = parse_pair_payload(&qr_payload(&offer));
    assert_eq!(payload.relay_url, relay.url);
    assert_eq!(payload.host_fingerprint, serve.fingerprint);
    let derived = pix_wire::relay_channel_secret_from_join_code(&join_code(&offer), &relay.url)
        .expect("join code derives the pairing secret");
    assert_eq!(derived, payload.channel_secret);

    let mut phone = Phone::new();
    let link = Link::connect_relay(&payload.relay_url, &payload.channel_secret)
        .expect("phone joins immediately after the QR is shown");
    let pending = phone.begin_pairing(link, "Remote iPhone");
    let request = serve.wait_event("pairing_requested", |_| true);
    assert!(request["id"].as_str().is_some(), "pairing id");
    assert_eq!(request["device_name"], "Remote iPhone");
    assert_eq!(
        request["confirmation_code"].as_str().map(str::len),
        Some(6),
        "six-digit confirmation"
    );
    assert!(request["expires_at"].as_u64().is_some(), "expiry");
    serve.command(&format!(
        "approve {}",
        request["id"].as_str().expect("request id")
    ));
    assert_eq!(
        request["confirmation_code"].as_str().expect("code"),
        pending.confirmation_code
    );
    let snapshot = phone.finish_pairing(pending);
    let (relay_url, device_secret) = snapshot.relay.clone().expect("snapshot carries relay");
    assert_eq!(relay_url, relay.url);
    serve.wait_event("connection_established", |_| true);

    // --- Scenario: handshake races the host's local bridge ----------------
    // A phone that sends XX message 1 without waiting for peer_joined used
    // to have that frame dropped; the Mac then never saw a pairing request.
    eprintln!("=== scenario: early handshake frame ===");
    serve.command("pair-remote");
    let early = parse_pair_payload(&qr_payload(
        &serve.wait_event("remote_pairing_ready", |_| true),
    ));
    let mut racer = Phone::new();
    let link = Link::join_relay(&early.relay_url, &early.channel_secret)
        .expect("join without waiting for peer_joined");
    let pending = racer.begin_pairing(link, "Racer iPhone");
    serve.approve_next_pairing();
    racer.finish_pairing(pending);

    // --- Scenario: a stranger proof cannot sit on the pairing channel -----
    // The Durable Object pins the first client proof; a later join with a
    // different proof must be rejected rather than superseding the phone.
    eprintln!("=== scenario: wrong join proof ===");
    serve.command("pair-remote");
    let guarded = parse_pair_payload(&qr_payload(
        &serve.wait_event("remote_pairing_ready", |_| true),
    ));
    let _holder = Link::connect_relay(&guarded.relay_url, &guarded.channel_secret)
        .expect("legitimate phone pins the client proof");
    let channel = pix_wire::relay_channel_id(&guarded.channel_secret).expect("channel id");
    let stranger = pix_wire::relay_join_proof(
        &e2e_support::random_channel_secret(),
        pix_wire::RelayRole::Client,
    )
    .expect("stranger proof");
    match Link::join_relay_raw(&guarded.relay_url, &channel, &stranger) {
        Err(reason) => eprintln!("stranger correctly rejected: {reason}"),
        Ok(_) => panic!("a stranger proof must not join the pairing channel"),
    }

    // --- Scenario: replaced pairing channel (the double-QR failure) -------
    // A second pair-remote replaces the first channel. A phone that scanned
    // the stale QR must fail fast instead of hanging, and the fresh QR must
    // still work end to end.
    eprintln!("=== scenario: replaced pairing channel ===");
    serve.command("pair-remote");
    let stale = parse_pair_payload(&qr_payload(
        &serve.wait_event("remote_pairing_ready", |_| true),
    ));
    serve.command("pair-remote");
    let fresh = parse_pair_payload(&qr_payload(
        &serve.wait_event("remote_pairing_ready", |_| true),
    ));
    assert_ne!(stale.channel_secret, fresh.channel_secret);
    serve.wait_event("relay_channel", |event| {
        event["label"] == "pairing" && event["state"] == "waiting"
    });

    // Give the retired agent a moment to finish closing; a phone in the
    // field always arrives seconds after the QR was replaced.
    std::thread::sleep(Duration::from_secs(2));
    match Link::connect_relay(&stale.relay_url, &stale.channel_secret) {
        Err(reason) => eprintln!("stale channel correctly dead: {reason}"),
        Ok(_) => panic!("the replaced pairing channel must not present a host"),
    }

    let mut second_phone = Phone::new();
    let link = Link::connect_relay(&fresh.relay_url, &fresh.channel_secret)
        .expect("fresh pairing channel works");
    let pending = second_phone.begin_pairing(link, "Second iPhone");
    serve.approve_next_pairing();
    second_phone.finish_pairing(pending);

    // --- Scenario: approval interrupted mid-flight, probe completes -------
    eprintln!("=== scenario: interrupted approval, relay probe ===");
    serve.command("pair-remote");
    let payload3 = parse_pair_payload(&qr_payload(
        &serve.wait_event("remote_pairing_ready", |_| true),
    ));
    serve.wait_event("relay_channel", |event| {
        event["label"] == "pairing" && event["state"] == "waiting"
    });
    let mut third_phone = Phone::new();
    let link = Link::connect_relay(&payload3.relay_url, &payload3.channel_secret)
        .expect("third phone joins");
    let pending = third_phone.begin_pairing(link, "Third iPhone");
    let host_key = pending.host_public_key_for_tests();
    drop(pending); // The route drops while the user approves on the Mac.
    serve.approve_next_pairing();
    serve.wait_event("device_list", |event| {
        event["devices"].as_array().is_some_and(|list| list.len() == 4)
    });

    // The phone rejoins the same pairing channel and probes with IK.
    third_phone.host_public_key = Some(host_key);
    let mut probed = None;
    for _ in 0..10 {
        std::thread::sleep(Duration::from_secs(1));
        let Ok(link) = Link::connect_relay(&payload3.relay_url, &payload3.channel_secret) else {
            continue;
        };
        if let Ok(outcome) = third_phone.reconnect(link) {
            probed = Some(outcome);
            break;
        }
    }
    let (_, info) = probed.expect("IK probe through the pairing channel succeeds");
    assert!(info.relay.is_some(), "probe snapshot hands out the device channel");

    // --- Scenario: the paired phone reaches the host through its durable
    // channel (the away-from-home path) -----------------------------------
    eprintln!("=== scenario: device channel reconnect ===");
    serve.wait_event("relay_channel", |event| {
        event["label"] != "pairing" && event["state"] == "waiting"
    });
    let link = Link::connect_relay(&relay_url, &device_secret)
        .expect("device channel reachable");
    let (mut session, _) = phone.reconnect(link).expect("IK over the device channel");
    session
        .request(pix_wire::ClientRequest::HostSnapshot)
        .expect("authenticated snapshot over the device channel");

    // A dropped route must come back with IK on the same durable channel.
    eprintln!("=== scenario: device channel drop then IK reconnect ===");
    drop(session);
    let link = Link::connect_relay(&relay_url, &device_secret)
        .expect("device channel still joinable");
    let (_session, info) = phone
        .reconnect(link)
        .expect("IK after the previous relay session closed");
    assert!(info.relay.is_some());

    serve.quit();
}
