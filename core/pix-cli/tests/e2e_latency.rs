//! Payload-free latency samples for catalog reconnects.
//!
//! These numbers are process-level p50/p95 for `host.snapshot` after pairing.
//! They include handshake and host scan, never prompt or message content.

mod e2e_support;

use std::time::Instant;

use e2e_support::{Link, Phone, Relay, Serve};
use tempfile::tempdir;

const LAN_SAMPLES: usize = 7;
const RELAY_SAMPLES: usize = 5;

#[test]
fn records_payload_free_lan_host_snapshot_latency() {
    let directory = tempdir().expect("config dir");
    let mut serve = Serve::start(directory.path(), None);
    let mut phone = Phone::new();
    let pending = phone.begin_pairing(Link::connect_lan(serve.lan_addr()), "Latency iPhone");
    let host_code = serve.approve_next_pairing();
    assert_eq!(host_code, pending.confirmation_code);
    let _ = phone.finish_pairing(pending);
    serve.wait_event("connection_established", |_| true);

    let samples = sample_snapshots(&mut phone, || Link::connect_lan(serve.lan_addr()), LAN_SAMPLES);
    report("lan", &samples);
    assert!(
        percentile(&samples, 0.95) < 10_000,
        "LAN host.snapshot p95 should stay well under a 10s safety bound"
    );
}

#[test]
fn records_payload_free_relay_host_snapshot_latency() {
    let relay = Relay::start(8794);
    let directory = tempdir().expect("config dir");
    let mut serve = Serve::start(directory.path(), Some(&relay.url));
    serve.wait_event("relay_configured", |_| true);
    serve.command("pair-remote");
    let offer = serve.wait_event("remote_pairing_ready", |_| true);
    let payload = e2e_support::parse_pair_payload(&e2e_support::qr_payload(&offer));
    let mut phone = Phone::new();
    let link = Link::connect_relay(&payload.relay_url, &payload.channel_secret)
        .expect("phone joins pairing channel");
    let pending = phone.begin_pairing(link, "Relay latency iPhone");
    let request = serve.wait_event("pairing_requested", |_| true);
    serve.command(&format!(
        "approve {}",
        request["id"].as_str().expect("request id")
    ));
    let snapshot = phone.finish_pairing(pending);
    let (relay_url, device_secret) = snapshot.relay.expect("snapshot carries device channel");
    assert_eq!(relay_url, relay.url);
    serve.wait_event("connection_established", |_| true);
    serve.wait_event("relay_channel", |event| {
        event["label"] != "pairing" && event["state"] == "waiting"
    });
    let samples = sample_snapshots(
        &mut phone,
        || Link::connect_relay(&relay.url, &device_secret).expect("device channel"),
        RELAY_SAMPLES,
    );
    report("relay", &samples);
    assert!(
        percentile(&samples, 0.95) < 15_000,
        "relay host.snapshot p95 should stay well under a 15s safety bound"
    );
}

fn sample_snapshots(
    phone: &mut Phone,
    connect: impl Fn() -> Link,
    count: usize,
) -> Vec<u64> {
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let started = Instant::now();
        phone
            .reconnect(connect())
            .expect("authenticated host.snapshot");
        samples.push(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    }
    samples.sort_unstable();
    samples
}

fn report(route: &str, samples: &[u64]) {
    eprintln!(
        "catalog_latency route={route} n={} p50={}ms p95={}ms",
        samples.len(),
        percentile(samples, 0.50),
        percentile(samples, 0.95)
    );
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn percentile(sorted: &[u64], quantile: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() as f64 - 1.0) * quantile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}
