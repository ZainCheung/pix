use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use pix_core::{
    ConfigStore, DirectTcpListener, HostConfig, HostEnvironment, HostService, HostServiceEvent,
    HostState, PairingCoordinator, RuntimeManager, RuntimeManagerOptions,
};
use pix_wire::{
    ClientEnvelope, ClientRequest, NoiseHandshake, NoisePattern, NoiseTransport, PROTOCOL_MAJOR,
    ServerEnvelope, ServerEvent, decode_pairing_offer, generate_static_keypair,
    pairing_introduction,
};
use tempfile::tempdir;

fn next_event(service: &pix_core::HostServiceHandle) -> HostServiceEvent {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(event) = service.try_next_event().expect("service event channel") {
            return event;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for service event"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn next_matching_event(
    service: &pix_core::HostServiceHandle,
    matches: impl Fn(&HostServiceEvent) -> bool,
) -> HostServiceEvent {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let event = next_event(service);
        if matches(&event) {
            return event;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for matching service event (last={event:?})"
        );
    }
}

fn runtime_manager(directory: &std::path::Path) -> Arc<RuntimeManager> {
    Arc::new(
        RuntimeManager::new(RuntimeManagerOptions {
            executable: PathBuf::from("unused-for-host-snapshot"),
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

fn read_server_envelope(
    connection: &mut pix_core::EncryptedConnection,
    transport: &mut pix_wire::NoiseTransport,
) -> ServerEnvelope {
    loop {
        let ciphertext = connection.read_frame().expect("read server event");
        if let Some(plaintext) = transport
            .decrypt_record(&ciphertext)
            .expect("decrypt server event")
        {
            return ServerEnvelope::decode(&plaintext).expect("decode server event");
        }
    }
}

fn pair_and_approve(
    service: &pix_core::HostServiceHandle,
    address: SocketAddr,
    phone_private_key: &[u8],
    device_name: &str,
) -> (pix_core::EncryptedConnection, NoiseTransport) {
    let mut connection = pix_core::EncryptedConnection::connect(address).expect("connect phone");
    connection
        .set_timeout(Some(Duration::from_secs(3)))
        .expect("phone timeout");
    let mut handshake = NoiseHandshake::initiator(NoisePattern::PairingXx, phone_private_key, None)
        .expect("phone pairing handshake");
    connection
        .write_frame(&handshake.write_message(b"").expect("XX message 1"))
        .expect("write XX message 1");
    let message_2 = connection.read_frame().expect("read XX message 2");
    let token = decode_pairing_offer(
        &handshake
            .read_message(&message_2)
            .expect("read XX message 2 payload"),
    )
    .expect("decode pairing token");
    let introduction = pairing_introduction(&token, device_name).expect("pairing introduction");
    connection
        .write_frame(
            &handshake
                .write_message(&introduction)
                .expect("XX message 3"),
        )
        .expect("write XX message 3");

    let request = match next_matching_event(service, |event| {
        matches!(event, HostServiceEvent::PairingRequested(_))
    }) {
        HostServiceEvent::PairingRequested(request) => request,
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(request.device_name, device_name);
    service.approve(request.id).expect("approve phone");
    assert!(matches!(
        next_matching_event(service, |event| {
            matches!(event, HostServiceEvent::ConnectionEstablished { .. })
        }),
        HostServiceEvent::ConnectionEstablished { .. }
    ));
    let transport = handshake.into_transport().expect("phone transport");
    (connection, transport)
}

#[test]
#[allow(clippy::too_many_lines)]
fn service_routes_pairing_through_approval_then_dispatches_snapshot() {
    let directory = tempdir().expect("temporary service directory");
    let store = ConfigStore::new(directory.path().join("config.json"));
    let config = HostConfig::new("Test Mac");
    store.save(&config).expect("initial config");
    let coordinator = Arc::new(PairingCoordinator::new(store));
    let host = generate_static_keypair().expect("host key");
    let phone = generate_static_keypair().expect("phone key");
    let listener = DirectTcpListener::bind(0).expect("direct listener");
    let address = listener.local_addr().expect("listener address");
    let host_state = Arc::new(HostState::new(config));
    let mut service = HostService::start_direct(
        listener,
        host.private_key.clone(),
        Arc::clone(&coordinator),
        Arc::clone(&host_state),
        runtime_manager(directory.path()),
    )
    .expect("start host service");

    let mut connection = pix_core::EncryptedConnection::connect(address).expect("connect phone");
    connection
        .set_timeout(Some(Duration::from_secs(3)))
        .expect("phone timeout");
    let mut handshake =
        NoiseHandshake::initiator(NoisePattern::PairingXx, &phone.private_key, None)
            .expect("phone pairing handshake");
    connection
        .write_frame(&handshake.write_message(b"").expect("XX message 1"))
        .expect("write XX message 1");
    let message_2 = connection.read_frame().expect("read XX message 2");
    let token = decode_pairing_offer(
        &handshake
            .read_message(&message_2)
            .expect("read XX message 2 payload"),
    )
    .expect("decode pairing token");
    let introduction = pairing_introduction(&token, "Test iPhone").expect("pairing introduction");
    connection
        .write_frame(
            &handshake
                .write_message(&introduction)
                .expect("XX message 3"),
        )
        .expect("write XX message 3");

    let request = match next_event(&service) {
        HostServiceEvent::PairingRequested(request) => request,
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(request.device_name, "Test iPhone");
    assert_eq!(request.confirmation_code.len(), 6);
    service.approve(request.id).expect("approve phone");
    assert!(matches!(
        next_event(&service),
        HostServiceEvent::ConnectionEstablished { .. }
    ));

    let mut transport = handshake.into_transport().expect("phone transport");
    let request = ClientEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id: 1,
        request: ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    }
    .encode()
    .expect("encode snapshot request");
    for ciphertext in transport
        .encrypt_message(&request)
        .expect("encrypt snapshot request")
    {
        connection
            .write_frame(&ciphertext)
            .expect("write snapshot request");
    }
    let response = loop {
        let ciphertext = connection.read_frame().expect("read snapshot response");
        if let Some(plaintext) = transport
            .decrypt_record(&ciphertext)
            .expect("decrypt response")
        {
            break plaintext;
        }
    };
    let response = ServerEnvelope::decode(&response).expect("decode snapshot response");
    assert_eq!(response.request_id, Some(1));
    assert!(matches!(response.event, ServerEvent::HostSnapshot { .. }));

    drop(connection);

    let _ = next_matching_event(&service, |event| {
        matches!(event, HostServiceEvent::ConnectionClosed { .. })
    });

    let mut reconnect = pix_core::EncryptedConnection::connect(address).expect("reconnect phone");
    reconnect
        .set_timeout(Some(Duration::from_secs(3)))
        .expect("reconnect timeout");
    let mut ik = NoiseHandshake::initiator(
        NoisePattern::ReconnectIk,
        &phone.private_key,
        Some(&host.public_key),
    )
    .expect("phone reconnect handshake");
    reconnect
        .write_frame(&ik.write_message(b"").expect("IK message 1"))
        .expect("write IK message 1");
    let message_2 = reconnect.read_frame().expect("read IK message 2");
    ik.read_message(&message_2).expect("read IK message 2");
    let mut transport = ik.into_transport().expect("reconnect transport");
    let request = ClientEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id: 2,
        request: ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    }
    .encode()
    .expect("encode reconnect snapshot request");
    for ciphertext in transport
        .encrypt_message(&request)
        .expect("encrypt reconnect request")
    {
        reconnect
            .write_frame(&ciphertext)
            .expect("write reconnect request");
    }
    let response = loop {
        let ciphertext = reconnect.read_frame().expect("read reconnect response");
        if let Some(plaintext) = transport
            .decrypt_record(&ciphertext)
            .expect("decrypt reconnect response")
        {
            break plaintext;
        }
    };
    let response = ServerEnvelope::decode(&response).expect("decode reconnect response");
    assert_eq!(response.request_id, Some(2));
    assert!(matches!(response.event, ServerEvent::HostSnapshot { .. }));
    drop(reconnect);
    service.shutdown();
}

#[test]
#[allow(clippy::too_many_lines)]
fn explicit_repair_after_revoke_allows_ik_reconnect() {
    let directory = tempdir().expect("temporary service directory");
    let store = ConfigStore::new(directory.path().join("config.json"));
    let config = HostConfig::new("Test Mac");
    store.save(&config).expect("initial config");
    let coordinator = Arc::new(PairingCoordinator::new(store));
    let host = generate_static_keypair().expect("host key");
    let phone = generate_static_keypair().expect("phone key");
    let listener = DirectTcpListener::bind(0).expect("direct listener");
    let address = listener.local_addr().expect("listener address");
    let host_state = Arc::new(HostState::new(config));
    let mut service = HostService::start_direct(
        listener,
        host.private_key.clone(),
        Arc::clone(&coordinator),
        Arc::clone(&host_state),
        runtime_manager(directory.path()),
    )
    .expect("start host service");

    let (first_connection, _first_transport) =
        pair_and_approve(&service, address, &phone.private_key, "Test iPhone");
    drop(first_connection);
    let _ = next_matching_event(&service, |event| {
        matches!(event, HostServiceEvent::ConnectionClosed { .. })
    });
    let device_id = service
        .paired_devices()
        .expect("list paired devices")
        .into_iter()
        .next()
        .expect("paired device")
        .id;
    service
        .revoke_device(&device_id)
        .expect("revoke paired device");
    assert!(
        service
            .paired_devices()
            .expect("list after revoke")
            .is_empty()
    );

    // Re-pair with the same static identity. Approval is idempotent in the
    // durable store, but must also clear the live registry's revocation mark.
    let (mut connection, mut transport) =
        pair_and_approve(&service, address, &phone.private_key, "Test iPhone");
    let request = ClientEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id: 1,
        request: ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    }
    .encode()
    .expect("encode snapshot request");
    for ciphertext in transport
        .encrypt_message(&request)
        .expect("encrypt snapshot request")
    {
        connection
            .write_frame(&ciphertext)
            .expect("write snapshot request");
    }
    let response = loop {
        let ciphertext = connection.read_frame().expect("read snapshot response");
        if let Some(plaintext) = transport
            .decrypt_record(&ciphertext)
            .expect("decrypt snapshot response")
        {
            break plaintext;
        }
    };
    let response = ServerEnvelope::decode(&response).expect("decode snapshot response");
    assert_eq!(response.request_id, Some(1));
    assert!(matches!(response.event, ServerEvent::HostSnapshot { .. }));
    drop(connection);
    service.shutdown();
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn service_forwards_pi_events_on_the_authenticated_connection() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("temporary service directory");
    let workspace = tempdir().expect("temporary workspace");
    let fake_pi = directory.path().join("fake-pi.sh");
    fs::write(
        &fake_pi,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  type=$(printf '%s' "$line" | sed -n 's/.*"type":"\([^"]*\)".*/\1/p')
  case "$type" in
    get_state)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"sessionId\":\"fake\",\"sessionName\":\"Mobile\",\"model\":null,\"thinkingLevel\":\"medium\",\"isStreaming\":false,\"isCompacting\":false,\"pendingMessageCount\":0}}"
      ;;
    get_messages)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_messages\",\"success\":true,\"data\":{\"messages\":[]}}"
      ;;
    prompt)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hello"}}'
      ;;
    *)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"$type\",\"success\":true}"
      ;;
  esac
done
"#,
    )
    .expect("write fake Pi");
    let mut permissions = fs::metadata(&fake_pi)
        .expect("fake Pi metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_pi, permissions).expect("make fake Pi executable");

    let store = ConfigStore::new(directory.path().join("config.json"));
    let mut config = HostConfig::new("Test Mac");
    let workspace_id = pix_core::WorkspaceRegistry::new(&mut config)
        .add(workspace.path(), Some("Project".to_owned()))
        .expect("authorize workspace")
        .id;
    store.save(&config).expect("initial config");
    let coordinator = Arc::new(PairingCoordinator::new(store));
    let host = generate_static_keypair().expect("host key");
    let phone = generate_static_keypair().expect("phone key");
    let listener = DirectTcpListener::bind(0).expect("direct listener");
    let address = listener.local_addr().expect("listener address");
    let manager = Arc::new(
        RuntimeManager::new(RuntimeManagerOptions {
            executable: fake_pi,
            lock_directory: directory.path().join("locks"),
            max_active_sessions: 4,
            max_concurrent_turns: 4,
            idle_timeout: Duration::from_secs(300),
            request_timeout: Duration::from_secs(2),
            extra_arguments: Vec::new(),
            environment: HostEnvironment::from_process(),
        })
        .expect("runtime manager"),
    );
    let mut service = HostService::start_direct(
        listener,
        host.private_key,
        Arc::clone(&coordinator),
        Arc::new(HostState::new(config)),
        manager,
    )
    .expect("start host service");

    let mut connection = pix_core::EncryptedConnection::connect(address).expect("connect phone");
    connection
        .set_timeout(Some(Duration::from_secs(3)))
        .expect("phone timeout");
    let mut handshake =
        NoiseHandshake::initiator(NoisePattern::PairingXx, &phone.private_key, None)
            .expect("phone pairing handshake");
    connection
        .write_frame(&handshake.write_message(b"").expect("XX message 1"))
        .expect("write XX message 1");
    let message_2 = connection.read_frame().expect("read XX message 2");
    let token = decode_pairing_offer(
        &handshake
            .read_message(&message_2)
            .expect("read XX message 2 payload"),
    )
    .expect("decode pairing token");
    connection
        .write_frame(
            &handshake
                .write_message(
                    &pairing_introduction(&token, "Test iPhone").expect("pairing introduction"),
                )
                .expect("XX message 3"),
        )
        .expect("write XX message 3");
    let approval = match next_event(&service) {
        HostServiceEvent::PairingRequested(request) => request,
        event => panic!("unexpected event: {event:?}"),
    };
    service.approve(approval.id).expect("approve phone");
    let _ = next_matching_event(&service, |event| {
        matches!(event, HostServiceEvent::ConnectionEstablished { .. })
    });
    let mut transport = handshake.into_transport().expect("phone transport");

    let create = ClientEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id: 1,
        request: ClientRequest::SessionCreate {
            workspace_id,
            name: Some("Mobile".to_owned()),
        },
    }
    .encode()
    .expect("encode create");
    for ciphertext in transport.encrypt_message(&create).expect("encrypt create") {
        connection.write_frame(&ciphertext).expect("write create");
    }
    let ack = read_server_envelope(&mut connection, &mut transport);
    assert!(matches!(ack.event, ServerEvent::RequestAck));
    let snapshot = read_server_envelope(&mut connection, &mut transport);
    let session_id = match snapshot.event {
        ServerEvent::SessionSnapshot { snapshot } => snapshot.id,
        event => panic!("expected session snapshot, got {event:?}"),
    };

    let prompt = ClientEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id: 2,
        request: ClientRequest::SessionPrompt {
            session_id,
            content: "Continue".to_owned(),
            attachments: Vec::new(),
        },
    }
    .encode()
    .expect("encode prompt");
    for ciphertext in transport.encrypt_message(&prompt).expect("encrypt prompt") {
        connection.write_frame(&ciphertext).expect("write prompt");
    }
    let ack = read_server_envelope(&mut connection, &mut transport);
    assert_eq!(ack.request_id, Some(2));
    assert!(matches!(ack.event, ServerEvent::RequestAck));
    let streamed = read_server_envelope(&mut connection, &mut transport);
    assert_eq!(streamed.request_id, None);
    assert!(matches!(streamed.event, ServerEvent::AssistantDelta { .. }));

    drop(connection);
    service.shutdown();
}
