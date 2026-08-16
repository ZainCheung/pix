//! End-to-end LAN scenarios against a real `pix serve` process.
//!
//! One test function runs the scenarios sequentially so the suite owns its
//! process lifecycle; each scenario logs a section marker for readability.

mod e2e_support;

use std::time::Duration;

use e2e_support::{Link, Phone, Serve};
use tempfile::tempdir;

#[test]
#[allow(clippy::too_many_lines)]
fn lan_pairing_lifecycle_end_to_end() {
    let directory = tempdir().expect("config dir");
    let mut serve = Serve::start(directory.path(), None);

    // --- Scenario: full pairing with approval -----------------------------
    eprintln!("=== scenario: LAN pairing with approval ===");
    let mut phone = Phone::new();
    let pending = phone.begin_pairing(Link::connect_lan(serve.lan_addr()), "E2E iPhone");
    let host_code = serve.approve_next_pairing();
    assert_eq!(host_code, pending.confirmation_code, "SAS codes must match");
    let snapshot = phone.finish_pairing(pending);
    assert!(!snapshot.display_name.is_empty());
    serve.wait_event("connection_established", |_| true);
    serve.wait_event("device_list", |event| {
        event["devices"]
            .as_array()
            .is_some_and(|list| list.len() == 1)
    });

    // --- Scenario: IK reconnect and an authenticated round trip -----------
    eprintln!("=== scenario: IK reconnect ===");
    let (_session, info) = phone
        .reconnect(Link::connect_lan(serve.lan_addr()))
        .expect("IK reconnect after approval");
    assert!(!info.display_name.is_empty());

    // --- Scenario: phone suspends before approval, approval still lands ---
    eprintln!("=== scenario: approval with a suspended phone ===");
    let mut sleepy = Phone::new();
    let pending = sleepy.begin_pairing(Link::connect_lan(serve.lan_addr()), "Sleepy iPhone");
    let host_key = pending.host_public_key_for_tests();
    drop(pending); // The phone locked; its socket resets.
    std::thread::sleep(Duration::from_millis(300));
    serve.approve_next_pairing();
    serve.wait_event("device_list", |event| {
        event["devices"]
            .as_array()
            .is_some_and(|list| list.len() == 2)
    });
    // The phone probes with IK once it returns, exactly like the iOS app.
    sleepy.host_public_key = Some(host_key);
    let (_session, _) = sleepy
        .reconnect(Link::connect_lan(serve.lan_addr()))
        .expect("IK probe succeeds after interrupted approval");

    // --- Scenario: a foreign pairing token never becomes a request --------
    eprintln!("=== scenario: foreign pairing token ===");
    let impostor = Phone::new();
    impostor
        .attempt_pairing_with_foreign_token(Link::connect_lan(serve.lan_addr()), "Impostor iPhone");
    serve.wait_event("connection_failed", |_| true);
    let after_forge = serve.drain_events();
    assert!(
        after_forge
            .iter()
            .all(|event| event["type"] != "pairing_requested"),
        "a foreign token must not raise a pairing request"
    );

    // --- Scenario: rejection leaves no trust ------------------------------
    eprintln!("=== scenario: rejection ===");
    let stranger = Phone::new();
    let pending = stranger.begin_pairing(Link::connect_lan(serve.lan_addr()), "Rejected iPhone");
    let request = serve.wait_event("pairing_requested", |_| true);
    serve.command(&format!("reject {}", request["id"].as_str().expect("id")));
    drop(pending);
    let mut stranger = stranger;
    stranger.host_public_key = Some(phone.host_public_key.clone().expect("host key"));
    assert!(
        stranger
            .reconnect(Link::connect_lan(serve.lan_addr()))
            .is_err(),
        "a rejected phone must not authenticate"
    );

    // --- Scenario: revocation closes access immediately -------------------
    eprintln!("=== scenario: revocation ===");
    let devices = serve.wait_devices(2);
    let sleepy_id = devices
        .iter()
        .find(|device| device["name"] == "Sleepy iPhone")
        .expect("sleepy device")["id"]
        .as_str()
        .expect("device id")
        .to_owned();
    serve.command(&format!("revoke {sleepy_id}"));
    serve.wait_event("device_revoked", |_| true);
    assert!(
        sleepy
            .reconnect(Link::connect_lan(serve.lan_addr()))
            .is_err(),
        "a revoked phone must not authenticate"
    );
    let (_session, _) = phone
        .reconnect(Link::connect_lan(serve.lan_addr()))
        .expect("other paired phones stay unaffected by revocation");

    serve.quit();

    // The log keeps the whole story, without any secrets.
    let log = serve.log_lines();
    assert!(
        log.iter().any(|line| line["kind"] == "lifecycle"),
        "log records lifecycle"
    );
}
