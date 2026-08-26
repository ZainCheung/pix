use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use pix_core::{
    AuthenticatedConnection, ConfigStore, DirectTcpListener, EncryptedConnection, HostConfig,
    HostEnvironment, HostProtocolDispatcher, HostState, PairingCoordinator,
    PendingPairingConnection, RuntimeManager, RuntimeManagerOptions,
};
use pix_wire::{
    ClientEnvelope, ClientRequest, NoiseHandshake, NoisePattern, PROTOCOL_MAJOR, ServerEnvelope,
    ServerEvent, generate_static_keypair,
};
use tempfile::tempdir;

struct PairedFixture {
    coordinator: std::sync::Arc<PairingCoordinator>,
    host: pix_wire::StaticKeyPair,
    phone: pix_wire::StaticKeyPair,
    _directory: tempfile::TempDir,
}

fn pair_over_xx() -> PairedFixture {
    let directory = tempdir().expect("temporary config directory");
    let store = ConfigStore::new(directory.path().join("config.json"));
    store
        .save(&HostConfig::new("Secure host"))
        .expect("initial config");
    let coordinator = std::sync::Arc::new(PairingCoordinator::new(store));
    let host = generate_static_keypair().expect("host identity");
    let phone = generate_static_keypair().expect("phone identity");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(50_000);
    let listener = DirectTcpListener::bind(0).expect("direct listener");
    let address = listener.local_addr().expect("listener address");
    let host_private = host.private_key.clone();
    let host_coordinator = std::sync::Arc::clone(&coordinator);
    let host_thread = thread::spawn(move || {
        let connection = listener.accept().expect("accept XX connection");
        let pending =
            PendingPairingConnection::accept(connection, &host_private, &host_coordinator, now)
                .expect("host XX pairing");
        assert_eq!(pending.pending().device_name, "Test iPhone");
        pending
            .approve(&host_coordinator, now)
            .expect("approve host pairing")
    });

    let mut connection = EncryptedConnection::connect(address).expect("connect XX client");
    connection
        .set_timeout(Some(Duration::from_secs(2)))
        .expect("XX timeout");
    let mut handshake =
        NoiseHandshake::initiator(NoisePattern::PairingXx, &phone.private_key, None)
            .expect("phone XX");
    let message_1 = handshake.write_message(b"").expect("XX message 1");
    connection
        .write_frame(&message_1)
        .expect("write XX message 1");
    let message_2 = connection.read_frame().expect("read XX message 2");
    let offer_payload = handshake
        .read_message(&message_2)
        .expect("phone reads XX 2");
    let token = pix_wire::decode_pairing_offer(&offer_payload).expect("decode pairing offer");
    assert!(
        !message_2
            .windows(token.len())
            .any(|window| window == token.as_bytes())
    );
    let introduction =
        pix_wire::pairing_introduction(&token, "Test iPhone").expect("encode introduction");
    connection
        .write_frame(
            &handshake
                .write_message(&introduction)
                .expect("XX message 3"),
        )
        .expect("write XX message 3");
    host_thread.join().expect("host pairing thread");
    PairedFixture {
        coordinator,
        host,
        phone,
        _directory: directory,
    }
}

/// The phone frequently suspends (and resets its socket) while the user
/// walks to the Mac to approve. Approval is durable trust and must succeed
/// anyway; the phone then completes pairing by reconnecting with IK.
#[test]
fn approval_survives_a_suspended_phone_and_ik_reconnect_completes_pairing() {
    let directory = tempdir().expect("temporary config directory");
    let store = ConfigStore::new(directory.path().join("config.json"));
    store
        .save(&HostConfig::new("Secure host"))
        .expect("initial config");
    let coordinator = std::sync::Arc::new(PairingCoordinator::new(store));
    let host = generate_static_keypair().expect("host identity");
    let phone = generate_static_keypair().expect("phone identity");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(50_000);
    let listener = DirectTcpListener::bind(0).expect("direct listener");
    let address = listener.local_addr().expect("listener address");

    let host_private = host.private_key.clone();
    let host_coordinator = std::sync::Arc::clone(&coordinator);
    let host_thread = thread::spawn(move || {
        let connection = listener.accept().expect("accept XX connection");
        let pending =
            PendingPairingConnection::accept(connection, &host_private, &host_coordinator, now)
                .expect("host XX pairing");
        (pending, listener)
    });

    let mut connection = EncryptedConnection::connect(address).expect("connect XX client");
    connection
        .set_timeout(Some(Duration::from_secs(2)))
        .expect("XX timeout");
    let mut handshake =
        NoiseHandshake::initiator(NoisePattern::PairingXx, &phone.private_key, None)
            .expect("phone XX");
    connection
        .write_frame(&handshake.write_message(b"").expect("XX message 1"))
        .expect("write XX message 1");
    let message_2 = connection.read_frame().expect("read XX message 2");
    let token = pix_wire::decode_pairing_offer(
        &handshake
            .read_message(&message_2)
            .expect("phone reads XX 2"),
    )
    .expect("decode pairing offer");
    connection
        .write_frame(
            &handshake
                .write_message(
                    &pix_wire::pairing_introduction(&token, "Test iPhone")
                        .expect("encode introduction"),
                )
                .expect("XX message 3"),
        )
        .expect("write XX message 3");
    let (pending, listener) = host_thread.join().expect("host pairing thread");

    // The phone suspends before the user approves on the Mac.
    drop(connection);
    thread::sleep(Duration::from_millis(200));

    let authenticated = pending
        .approve(&coordinator, now)
        .expect("approval must not depend on the pairing socket staying alive");
    drop(authenticated);
    let devices = coordinator.list_devices().expect("list devices");
    assert_eq!(devices.len(), 1, "approved trust must be durable");

    // The phone returns to the foreground and reconnects with IK.
    let host_private = host.private_key.clone();
    let host_coordinator = std::sync::Arc::clone(&coordinator);
    let ik_host_thread = thread::spawn(move || {
        let connection = listener.accept().expect("accept IK connection");
        AuthenticatedConnection::accept_reconnect(connection, &host_private, &host_coordinator)
            .expect("host IK reconnect after suspended pairing")
            .device()
            .clone()
    });
    let mut reconnect = EncryptedConnection::connect(address).expect("connect IK client");
    reconnect
        .set_timeout(Some(Duration::from_secs(2)))
        .expect("IK timeout");
    let mut ik = NoiseHandshake::initiator(
        NoisePattern::ReconnectIk,
        &phone.private_key,
        Some(&host.public_key),
    )
    .expect("phone IK");
    reconnect
        .write_frame(&ik.write_message(b"").expect("IK message 1"))
        .expect("write IK message 1");
    let message_2 = reconnect.read_frame().expect("read IK message 2");
    ik.read_message(&message_2).expect("phone reads IK 2");
    assert!(ik.is_handshake_finished());
    let device = ik_host_thread.join().expect("IK host thread");
    assert_eq!(device.name, "Test iPhone");
}

#[test]
fn tcp_noise_xx_approval_then_ik_application_round_trip() {
    let fixture = pair_over_xx();
    let coordinator = fixture.coordinator;
    let host = fixture.host;
    let phone = fixture.phone;
    let listener = DirectTcpListener::bind(0).expect("IK listener");
    let address = listener.local_addr().expect("IK listener address");
    let host_private = host.private_key.clone();
    let host_coordinator = std::sync::Arc::clone(&coordinator);
    let ik_host_thread = thread::spawn(move || {
        let locks = tempdir().expect("dispatcher lock directory");
        let manager = Arc::new(
            RuntimeManager::new(RuntimeManagerOptions {
                executable: PathBuf::from("unused-for-host-snapshot"),
                lock_directory: locks.path().to_path_buf(),
                max_active_sessions: 4,
                max_concurrent_turns: 4,
                idle_timeout: Duration::from_secs(300),
                request_timeout: Duration::from_secs(2),
                extra_arguments: Vec::new(),
                environment: HostEnvironment::from_process(),
            })
            .expect("runtime manager"),
        );
        let mut dispatcher = HostProtocolDispatcher::new(
            Arc::new(HostState::new(HostConfig::new("Secure host"))),
            manager,
        );
        let connection = listener.accept().expect("accept IK connection");
        let mut authenticated =
            AuthenticatedConnection::accept_reconnect(connection, &host_private, &host_coordinator)
                .expect("host IK reconnect");
        assert_eq!(
            authenticated
                .dispatch_next(&mut dispatcher)
                .expect("dispatch request"),
            1
        );
    });

    let mut connection = EncryptedConnection::connect(address).expect("connect IK client");
    connection
        .set_timeout(Some(Duration::from_secs(2)))
        .expect("IK timeout");
    let mut handshake = NoiseHandshake::initiator(
        NoisePattern::ReconnectIk,
        &phone.private_key,
        Some(&host.public_key),
    )
    .expect("phone IK");
    connection
        .write_frame(&handshake.write_message(b"").expect("IK message 1"))
        .expect("write IK message 1");
    let message_2 = connection.read_frame().expect("read IK message 2");
    handshake
        .read_message(&message_2)
        .expect("phone reads IK 2");
    let mut transport = handshake.into_transport().expect("phone transport");
    let request = ClientEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id: 77,
        request: ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    }
    .encode()
    .expect("encode client request");
    for ciphertext in transport
        .encrypt_message(&request)
        .expect("encrypt request")
    {
        connection
            .write_frame(&ciphertext)
            .expect("write request fragment");
    }
    let response = loop {
        let ciphertext = connection.read_frame().expect("read response fragment");
        if let Some(plaintext) = transport
            .decrypt_record(&ciphertext)
            .expect("decrypt response")
        {
            break plaintext;
        }
    };
    let response = ServerEnvelope::decode(&response).expect("decode server response");
    assert_eq!(response.request_id, Some(77));
    assert!(matches!(response.event, ServerEvent::HostSnapshot { .. }));
    ik_host_thread.join().expect("IK host thread");
}
