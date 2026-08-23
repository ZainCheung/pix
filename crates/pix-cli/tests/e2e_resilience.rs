//! End-to-end resilience scenarios: relay outages, host process restarts,
//! stalled UI readers, and log hygiene.

mod e2e_support;

use std::io::Write as _;
use std::net::TcpListener;
use std::time::{Duration, Instant};

use e2e_support::{EVENT_TIMEOUT, Link, Phone, Relay, Serve};
use tempfile::tempdir;

#[test]
fn host_and_relay_survive_restarts_and_stalled_readers() {
    let relay_port = TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve relay port")
        .local_addr()
        .expect("relay address")
        .port();
    let mut relay = Relay::start(relay_port);
    let directory = tempdir().expect("config dir");
    let mut serve = Serve::start(directory.path(), Some(&relay.url));

    // Pair one phone over the LAN so a durable device channel exists.
    eprintln!("=== setup: LAN pairing ===");
    let mut phone = Phone::new();
    let pending = phone.begin_pairing(Link::connect_lan(serve.lan_addr()), "Resilience iPhone");
    serve.approve_next_pairing();
    let snapshot = phone.finish_pairing(pending);
    let (relay_url, device_secret) = snapshot.relay.expect("relay access in snapshot");
    serve.wait_event("relay_channel", |event| event["state"] == "waiting");

    // --- Scenario: relay restart; the standing channel recovers alone ------
    eprintln!("=== scenario: relay restart recovery ===");
    relay.stop();
    serve.wait_event("relay_channel", |event| {
        event["state"]
            .as_str()
            .is_some_and(|state| state.starts_with("failed"))
    });
    assert!(serve.is_running(), "serve must survive a relay outage");
    // Held until the end of the test so the relay stays up for later scenarios.
    let _relay = Relay::start(relay_port);
    serve.wait_event("relay_channel", |event| event["state"] == "waiting");
    let link = Link::connect_relay(&relay_url, &device_secret)
        .expect("device channel back after relay restart");
    let (mut session_one, _) = phone.reconnect(link).expect("IK after relay restart");

    // --- Scenario: route change: a second connection supersedes the first --
    // The first session from the previous scenario is still connected; a
    // Wi-Fi to cellular move opens a second one through the same channel.
    eprintln!("=== scenario: connection supersession ===");
    let second = Link::connect_relay(&relay_url, &device_secret).expect("second route");
    let (_session_two, _) = phone.reconnect(second).expect("second IK supersedes");
    assert!(
        session_one
            .request(pix_wire::ClientRequest::HostSnapshot {
                capabilities: Vec::new()
            })
            .is_err(),
        "the superseded route must be closed"
    );

    // --- Scenario: host process dies; a fresh serve restores the channel ---
    eprintln!("=== scenario: host restart ===");
    serve.kill();
    let mut serve = Serve::start(directory.path(), None); // relay URL persists in config
    serve.wait_event("relay_channel", |event| event["state"] == "waiting");
    let link =
        Link::connect_relay(&relay_url, &device_secret).expect("device channel after host restart");
    let (_session, _) = phone.reconnect(link).expect("IK after host restart");

    // --- Scenario: stalled stdout reader must not kill or wedge serve ------
    eprintln!("=== scenario: stalled event reader ===");
    // Flood serve with commands that produce events while nobody drains our
    // side quickly; serve writes are best-effort and must never block or die.
    for _ in 0..200 {
        serve.command("devices");
        serve.command("sessions");
    }
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while Instant::now() < deadline {
        assert!(serve.is_running(), "serve died under event pressure");
        std::thread::sleep(Duration::from_millis(100));
        if serve.drain_events().len() > 100 {
            break;
        }
    }
    assert!(serve.is_running());
    let link = Link::connect_relay(&relay_url, &device_secret).expect("still reachable");
    let (_session, _) = phone.reconnect(link).expect("still serving after pressure");

    // --- Scenario: log hygiene ---------------------------------------------
    eprintln!("=== scenario: log hygiene ===");
    serve.quit();
    let log_text = serde_json::to_string(&serve.log_lines()).expect("log json");
    assert!(
        !log_text.contains(&device_secret),
        "device channel secrets must never reach the log"
    );
    let log_path = serve
        .config_path
        .parent()
        .expect("config dir")
        .join("logs/host.jsonl");
    let raw = std::fs::read_to_string(log_path).expect("log file");
    let mut checked = 0;
    for line in raw.lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("every log line is JSON");
        checked += 1;
    }
    assert!(checked > 0, "log has entries");
    let _ = std::io::stderr().flush();
}
