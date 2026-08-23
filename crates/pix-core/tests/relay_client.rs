//! Host relay-bridge integration tests against an in-process mock relay.
//!
//! The mock relay mimics the deployed Worker contract: it records join
//! headers, forwards opaque binary frames between one host and one client
//! connection, and emits `peer_joined` / `peer_left` control messages. Every
//! forwarded byte is captured so tests can prove the relay path never
//! observes plaintext.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pix_core::{
    ConfigStore, DirectTcpListener, HostConfig, HostEnvironment, HostService, HostServiceEvent,
    HostState, PairingCoordinator, RelayManager, RelayServiceEvent, RuntimeManager,
    RuntimeManagerOptions,
};
use pix_wire::{
    ClientEnvelope, ClientRequest, EncryptedFrameDecoder, NoiseHandshake, NoisePattern,
    PROTOCOL_MAJOR, RelayRole, ServerEnvelope, ServerEvent, decode_pairing_offer,
    encode_encrypted_frame, generate_static_keypair, pairing_introduction, relay_channel_id,
    relay_join_proof,
};
use tempfile::tempdir;
use tungstenite::handshake::server::{Request, Response};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
struct JoinRecord {
    path: String,
    role: String,
    proof: String,
    protocol: String,
}

struct MockRelay {
    url: String,
    joins: Arc<Mutex<Vec<JoinRecord>>>,
    forwarded: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MockRelay {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock relay");
        let url = format!("ws://{}", listener.local_addr().expect("relay address"));
        listener
            .set_nonblocking(true)
            .expect("nonblocking mock relay");
        let joins = Arc::new(Mutex::new(Vec::new()));
        let forwarded = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let joins = Arc::clone(&joins);
            let forwarded = Arc::clone(&forwarded);
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("mock-relay".to_owned())
                .spawn(move || relay_loop(&listener, &joins, &forwarded, &stop))
                .expect("spawn mock relay")
        };
        Self {
            url,
            joins,
            forwarded,
            stop,
            thread: Some(thread),
        }
    }

    fn joins(&self) -> Vec<JoinRecord> {
        self.joins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn forwarded_bytes(&self) -> Vec<u8> {
        self.forwarded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for MockRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type ServerSocket = WebSocket<TcpStream>;

#[derive(Default)]
struct ChannelSlots {
    host: Option<ServerSocket>,
    client: Option<ServerSocket>,
}

fn relay_loop(
    listener: &TcpListener,
    joins: &Arc<Mutex<Vec<JoinRecord>>>,
    forwarded: &Arc<Mutex<Vec<u8>>>,
    stop: &Arc<AtomicBool>,
) {
    use std::collections::HashMap;
    // One slot pair per channel path, mirroring one Durable Object each.
    let mut channels: HashMap<String, ChannelSlots> = HashMap::new();
    while !stop.load(Ordering::Acquire) {
        if let Ok((stream, _)) = listener.accept() {
            stream.set_nonblocking(false).expect("blocking handshake");
            if let Some((socket, record)) = accept_join(stream) {
                let slots = channels.entry(record.path.clone()).or_default();
                if record.role == "host" {
                    slots.host = Some(socket);
                } else {
                    slots.client = Some(socket);
                }
                joins
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(record);
                if slots.host.is_some() && slots.client.is_some() {
                    announce(slots.host.as_mut(), "peer_joined");
                    announce(slots.client.as_mut(), "peer_joined");
                }
            }
        }

        for slots in channels.values_mut() {
            let client_dropped = pump(slots.client.as_mut(), slots.host.as_mut(), forwarded);
            if client_dropped {
                slots.client = None;
                announce(slots.host.as_mut(), "peer_left");
            }
            let host_dropped = pump(slots.host.as_mut(), slots.client.as_mut(), forwarded);
            if host_dropped {
                slots.host = None;
                announce(slots.client.as_mut(), "peer_left");
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[allow(clippy::result_large_err)]
fn accept_join(stream: TcpStream) -> Option<(ServerSocket, JoinRecord)> {
    let captured: Arc<Mutex<Option<JoinRecord>>> = Arc::new(Mutex::new(None));
    let callback_capture = Arc::clone(&captured);
    let header = move |request: &Request, response: Response| {
        let header = |name: &str| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        *callback_capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(JoinRecord {
            path: request.uri().path().to_owned(),
            role: header("X-Pix-Role"),
            proof: header("X-Pix-Join-Proof"),
            protocol: header("X-Pix-Protocol"),
        });
        Ok(response)
    };
    let socket = tungstenite::accept_hdr(stream, header).ok()?;
    socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(10)))
        .expect("socket poll timeout");
    let record = captured
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()?;
    Some((socket, record))
}

fn announce(socket: Option<&mut ServerSocket>, kind: &str) {
    if let Some(socket) = socket {
        let _ = socket.send(Message::Text(format!("{{\"type\":\"{kind}\"}}").into()));
    }
}

/// Forwards pending frames and reports whether the source connection ended.
fn pump(
    from: Option<&mut ServerSocket>,
    mut to: Option<&mut ServerSocket>,
    forwarded: &Arc<Mutex<Vec<u8>>>,
) -> bool {
    let Some(from) = from else { return false };
    loop {
        match from.read() {
            Ok(Message::Binary(frame)) => {
                forwarded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend_from_slice(&frame);
                if let Some(to) = to.as_mut() {
                    let _ = to.send(Message::Binary(frame));
                }
            }
            Ok(Message::Close(_)) => return true,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return false;
            }
            Err(_) => return true,
        }
    }
}

/// Phone-side WebSocket helper joining the mock relay as `client`.
struct RelayPhone {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    decoder: EncryptedFrameDecoder,
    pending: Vec<Vec<u8>>,
}

impl RelayPhone {
    fn join(relay_url: &str, channel_secret: &str) -> Self {
        use tungstenite::client::IntoClientRequest;
        let channel = relay_channel_id(channel_secret).expect("channel id");
        let proof = relay_join_proof(channel_secret, RelayRole::Client).expect("client proof");
        let mut request = format!("{relay_url}/v1/channel/{channel}")
            .into_client_request()
            .expect("client request");
        request.headers_mut().insert(
            "X-Pix-Protocol",
            tungstenite::http::HeaderValue::from_static("1"),
        );
        request.headers_mut().insert(
            "X-Pix-Role",
            tungstenite::http::HeaderValue::from_static("client"),
        );
        request.headers_mut().insert(
            "X-Pix-Join-Proof",
            tungstenite::http::HeaderValue::from_str(&proof).expect("proof header"),
        );
        let (socket, _) = tungstenite::connect(request).expect("phone joins relay");
        match socket.get_ref() {
            MaybeTlsStream::Plain(stream) => stream
                .set_read_timeout(Some(Duration::from_millis(20)))
                .expect("phone poll timeout"),
            _ => panic!("mock relay is plain TCP"),
        }
        Self {
            socket,
            decoder: EncryptedFrameDecoder::new(),
            pending: Vec::new(),
        }
    }

    fn wait_for_peer(&mut self) {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for host");
            match self.socket.read() {
                Ok(Message::Text(text)) if text.as_str().contains("peer_joined") => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("phone relay socket failed: {error}"),
            }
        }
    }

    fn send_record(&mut self, ciphertext: &[u8]) {
        let framed = encode_encrypted_frame(ciphertext).expect("frame record");
        self.socket
            .send(Message::Binary(framed.into()))
            .expect("send relay record");
    }

    fn read_record(&mut self) -> Vec<u8> {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            if !self.pending.is_empty() {
                return self.pending.remove(0);
            }
            assert!(Instant::now() < deadline, "timed out waiting for record");
            match self.socket.read() {
                Ok(Message::Binary(frame)) => {
                    let records = self.decoder.push(&frame).expect("decode relay frame");
                    self.pending.extend(records);
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("phone relay socket failed: {error}"),
            }
        }
    }

    fn close(mut self) {
        let _ = self.socket.close(None);
        // Drain until the close completes so the mock relay observes it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.socket.read() {
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
    }
}

fn runtime_manager(directory: &std::path::Path) -> Arc<RuntimeManager> {
    Arc::new(
        RuntimeManager::new(RuntimeManagerOptions {
            executable: std::path::PathBuf::from("unused-for-relay-tests"),
            lock_directory: directory.join("locks"),
            max_active_sessions: 4,
            max_concurrent_turns: 4,
            idle_timeout: Duration::from_secs(300),
            request_timeout: Duration::from_secs(2),
            extra_arguments: Vec::new(),
            environment: HostEnvironment::from_process(),
        })
        .expect("runtime manager"),
    )
}

fn next_service_event(service: &pix_core::HostServiceHandle) -> HostServiceEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(event) = service.try_next_event().expect("service events") {
            return event;
        }
        assert!(Instant::now() < deadline, "timed out waiting for event");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_relay_event(
    events: &mpsc::Receiver<RelayServiceEvent>,
    matches: impl Fn(&RelayServiceEvent) -> bool,
) -> RelayServiceEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) if matches(&event) => return event,
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("relay events disconnected"),
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for relay event"
        );
    }
}

struct SecureClient {
    transport: pix_wire::NoiseTransport,
    request_id: u64,
}

impl SecureClient {
    fn request(&mut self, phone: &mut RelayPhone, request: ClientRequest) -> ServerEnvelope {
        self.request_id += 1;
        let encoded = ClientEnvelope {
            protocol: PROTOCOL_MAJOR,
            request_id: self.request_id,
            request,
        }
        .encode()
        .expect("encode request");
        for ciphertext in self
            .transport
            .encrypt_message(&encoded)
            .expect("encrypt request")
        {
            phone.send_record(&ciphertext);
        }
        loop {
            let record = phone.read_record();
            if let Some(plaintext) = self
                .transport
                .decrypt_record(&record)
                .expect("decrypt response")
            {
                return ServerEnvelope::decode(&plaintext).expect("decode response");
            }
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn relay_bridges_pairing_ik_reconnect_and_stays_ciphertext_only() {
    let directory = tempdir().expect("test directory");
    let store = ConfigStore::new(directory.path().join("config.json"));
    let relay = MockRelay::start();

    let mut config = HostConfig::new("Relay Test Mac");
    config.preferences.relay_url = Some(relay.url.clone());
    store.save(&config).expect("initial config");

    let coordinator = Arc::new(PairingCoordinator::new(store.clone()));
    let host_keys = generate_static_keypair().expect("host key");
    let phone_keys = generate_static_keypair().expect("phone key");
    let listener = DirectTcpListener::bind(0).expect("host listener");
    let local_port = listener.local_addr().expect("listener address").port();
    let host_state = Arc::new(HostState::new(config));
    let mut service = HostService::start_direct(
        listener,
        host_keys.private_key.clone(),
        Arc::clone(&coordinator),
        Arc::clone(&host_state),
        runtime_manager(directory.path()),
    )
    .expect("start host service");

    // Remote QR pairing: a two-minute single-use channel is announced out of
    // band; the phone joins it and runs the standard XX flow through the
    // bridge.
    let (relay_events_tx, relay_events) = mpsc::channel();
    let manager = RelayManager::new(relay.url.clone(), local_port, relay_events_tx);
    let pairing_offer = manager
        .start_remote_pairing(Duration::from_secs(120))
        .expect("start remote pairing channel");
    wait_for_relay_event(
        &relay_events,
        |event| matches!(event, RelayServiceEvent::ChannelWaiting { label } if label == "pairing"),
    );

    let mut phone = RelayPhone::join(&relay.url, &pairing_offer.channel_secret);
    phone.wait_for_peer();
    wait_for_relay_event(
        &relay_events,
        |event| matches!(event, RelayServiceEvent::PeerJoined { label } if label == "pairing"),
    );

    let mut handshake =
        NoiseHandshake::initiator(NoisePattern::PairingXx, &phone_keys.private_key, None)
            .expect("phone XX handshake");
    phone.send_record(&handshake.write_message(b"").expect("XX message 1"));
    let message_2 = phone.read_record();
    let token = decode_pairing_offer(
        &handshake
            .read_message(&message_2)
            .expect("XX message 2 payload"),
    )
    .expect("pairing token");
    phone.send_record(
        &handshake
            .write_message(&pairing_introduction(&token, "Remote iPhone").expect("introduction"))
            .expect("XX message 3"),
    );

    let request = match next_service_event(&service) {
        HostServiceEvent::PairingRequested(request) => request,
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(request.device_name, "Remote iPhone");
    service.approve(request.id).expect("approve remote phone");

    let mut secure = SecureClient {
        transport: handshake.into_transport().expect("phone transport"),
        request_id: 0,
    };
    let snapshot = secure.request(
        &mut phone,
        ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    );
    let ServerEvent::HostSnapshot { snapshot } = snapshot.event else {
        panic!("expected host snapshot, got {:?}", snapshot.event);
    };
    // The authenticated snapshot hands the phone its durable relay channel.
    let relay_access = snapshot.relay.expect("snapshot exposes relay access");
    assert_eq!(relay_access.url, relay.url);
    let device_channel_secret = relay_access.channel_secret;
    assert_ne!(device_channel_secret, pairing_offer.channel_secret);
    phone.close();
    wait_for_relay_event(
        &relay_events,
        |event| matches!(event, RelayServiceEvent::PeerLeft { label } if label == "pairing"),
    );

    // A phone whose route dropped during approval rejoins the same pairing
    // channel within the window and probes with IK. IK completing is the
    // approval proof.
    let mut probe = RelayPhone::join(&relay.url, &pairing_offer.channel_secret);
    probe.wait_for_peer();
    let mut probe_ik = NoiseHandshake::initiator(
        NoisePattern::ReconnectIk,
        &phone_keys.private_key,
        Some(&host_keys.public_key),
    )
    .expect("probe IK handshake");
    probe.send_record(&probe_ik.write_message(b"").expect("probe IK message 1"));
    let message_2 = probe.read_record();
    probe_ik
        .read_message(&message_2)
        .expect("probe IK message 2");
    let mut probe_secure = SecureClient {
        transport: probe_ik.into_transport().expect("probe transport"),
        request_id: 0,
    };
    let response = probe_secure.request(
        &mut probe,
        ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    );
    assert!(matches!(response.event, ServerEvent::HostSnapshot { .. }));
    probe.close();

    // The durable channel comes up for the paired device list, then the phone
    // reconnects with IK through it.
    let devices = store.load().expect("config with device").devices;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].relay_channel, device_channel_secret);
    manager
        .sync_devices(
            devices
                .iter()
                .map(|device| (device.id.as_str(), device.relay_channel.as_str())),
        )
        .expect("start device channel");
    wait_for_relay_event(
        &relay_events,
        |event| matches!(event, RelayServiceEvent::ChannelWaiting { label } if label != "pairing"),
    );

    let mut phone = RelayPhone::join(&relay.url, &device_channel_secret);
    phone.wait_for_peer();
    let mut ik = NoiseHandshake::initiator(
        NoisePattern::ReconnectIk,
        &phone_keys.private_key,
        Some(&host_keys.public_key),
    )
    .expect("phone IK handshake");
    phone.send_record(&ik.write_message(b"").expect("IK message 1"));
    let message_2 = phone.read_record();
    ik.read_message(&message_2).expect("IK message 2");
    let mut secure = SecureClient {
        transport: ik.into_transport().expect("IK transport"),
        request_id: 0,
    };
    let response = secure.request(
        &mut phone,
        ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    );
    assert!(matches!(response.event, ServerEvent::HostSnapshot { .. }));
    phone.close();
    wait_for_relay_event(
        &relay_events,
        |event| matches!(event, RelayServiceEvent::PeerLeft { label } if label != "pairing"),
    );

    // Route change: the same phone joins again and must be re-bridged.
    let mut phone = RelayPhone::join(&relay.url, &device_channel_secret);
    phone.wait_for_peer();
    let mut ik = NoiseHandshake::initiator(
        NoisePattern::ReconnectIk,
        &phone_keys.private_key,
        Some(&host_keys.public_key),
    )
    .expect("second IK handshake");
    phone.send_record(&ik.write_message(b"").expect("IK message 1"));
    let message_2 = phone.read_record();
    ik.read_message(&message_2).expect("IK message 2");
    let mut secure = SecureClient {
        transport: ik.into_transport().expect("IK transport"),
        request_id: 0,
    };
    let response = secure.request(
        &mut phone,
        ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    );
    assert!(matches!(response.event, ServerEvent::HostSnapshot { .. }));
    phone.close();

    // The relay path saw real traffic, none of it decryptable.
    let forwarded = relay.forwarded_bytes();
    assert!(!forwarded.is_empty(), "mock relay saw no frames");
    for plaintext in [
        "host.snapshot".as_bytes(),
        "Relay Test Mac".as_bytes(),
        "Remote iPhone".as_bytes(),
        token.as_bytes(),
        device_channel_secret.as_bytes(),
    ] {
        assert!(
            !contains(&forwarded, plaintext),
            "relay observed plaintext {:?}",
            String::from_utf8_lossy(plaintext)
        );
    }

    // Join proofs matched the pix-wire derivations and never the secrets.
    let joins = relay.joins();
    assert!(joins.iter().all(|join| join.protocol == "1"));
    let pairing_channel = relay_channel_id(&pairing_offer.channel_secret).expect("pairing id");
    let device_channel = relay_channel_id(&device_channel_secret).expect("device id");
    assert!(
        joins
            .iter()
            .any(|join| join.path == format!("/v1/channel/{pairing_channel}")
                && join.role == "host"
                && join.proof
                    == relay_join_proof(&pairing_offer.channel_secret, RelayRole::Host)
                        .expect("proof"))
    );
    assert!(
        joins
            .iter()
            .any(|join| join.path == format!("/v1/channel/{device_channel}")
                && join.role == "client"
                && join.proof
                    == relay_join_proof(&device_channel_secret, RelayRole::Client).expect("proof"))
    );

    manager.shutdown();
    service.shutdown();
}

#[test]
fn relay_agent_reconnects_with_backoff_after_relay_outage() {
    // Reserve an address, then refuse connections on it until the "relay"
    // comes back.
    let placeholder = TcpListener::bind("127.0.0.1:0").expect("reserve relay address");
    let relay_addr = placeholder.local_addr().expect("relay address");
    drop(placeholder);
    let relay_url = format!("ws://{relay_addr}");

    let secret = {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7_u8; 32])
    };

    let (events_tx, events) = mpsc::channel();
    let manager = RelayManager::new(relay_url, 1, events_tx);
    manager
        .sync_devices([("device-under-test", secret.as_str())])
        .expect("start channel");

    wait_for_relay_event(&events, |event| {
        matches!(
            event,
            RelayServiceEvent::ChannelFailed {
                stage: pix_core::RelayStage::Connect,
                ..
            }
        )
    });

    // Relay comes back on the same address; the agent must recover on its own.
    let listener = TcpListener::bind(relay_addr).expect("restart mock relay");
    listener.set_nonblocking(true).expect("nonblocking");
    let joins = Arc::new(Mutex::new(Vec::new()));
    let forwarded = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let relay_thread = {
        let joins = Arc::clone(&joins);
        let forwarded = Arc::clone(&forwarded);
        let stop = Arc::clone(&stop);
        thread::spawn(move || relay_loop(&listener, &joins, &forwarded, &stop))
    };

    wait_for_relay_event(&events, |event| {
        matches!(event, RelayServiceEvent::ChannelWaiting { .. })
    });
    assert!(
        joins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|join: &JoinRecord| join.role == "host")
    );

    manager.shutdown();
    stop.store(true, Ordering::Release);
    relay_thread.join().expect("mock relay thread");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The mock relay is plaintext, so this is the only test that exercises the
/// TLS code path. A `wss://` attempt must fail with a payload-free event —
/// never a panic in the agent thread, which is what an uninstalled rustls
/// crypto provider caused in production.
#[test]
fn wss_join_attempt_fails_cleanly_instead_of_panicking() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind TLS placeholder");
    let address = listener.local_addr().expect("placeholder address");
    // Accept-and-drop so the TLS client sees a closed stream mid-handshake.
    // Deliberately detached: it blocks in accept() between connections and
    // ends with the test process.
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            drop(stream);
        }
    });

    let secret = {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9_u8; 32])
    };
    let (events_tx, events) = mpsc::channel();
    let manager = RelayManager::new(format!("wss://{address}"), 1, events_tx);
    manager
        .sync_devices([("tls-path-device", secret.as_str())])
        .expect("start wss channel");

    // A panic in the agent thread would surface as a missing event here.
    wait_for_relay_event(&events, |event| {
        matches!(event, RelayServiceEvent::ChannelFailed { .. })
    });

    manager.shutdown();
}
