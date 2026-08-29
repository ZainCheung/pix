#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use pix_core::{
    ConfigStore, DirectTcpListener, DiscoveredSession, HostConfig, HostEnvironment, HostService,
    HostState, PairingCoordinator, ProcessIdentity, RuntimeBackend, RuntimeManager,
    RuntimeManagerError, RuntimeManagerOptions, SessionId, SessionSummary, TuiBridgeEventFrame,
    TuiBridgeHarness, TuiBridgePeer, TuiBridgeRegister, WorkspaceRegistry, owner_uid,
};
use pix_wire::generate_static_keypair;
use tempfile::tempdir;

fn fake_pi_script() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("fake Pi directory");
    let path = directory.path().join("fake-pi.sh");
    fs::write(
        &path,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  type=$(printf '%s' "$line" | sed -n 's/.*"type":"\([^"]*\)".*/\1/p')
  if [ "$type" = "get_state" ]; then
    printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"sessionId\":\"fake\",\"sessionName\":\"Fake\",\"model\":{\"provider\":\"fake\",\"id\":\"model\"},\"thinkingLevel\":\"medium\",\"isStreaming\":false,\"isCompacting\":false,\"pendingMessageCount\":0}}"
  fi
done
"#,
    )
    .expect("write fake Pi");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make Pi executable");
    (directory, path)
}

fn manager_setup() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<RuntimeManager>,
    TuiBridgeHarness,
    TuiBridgePeer,
    SessionId,
) {
    let (fake_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("workspace");
    let locks = tempdir().expect("locks");
    let manager = Arc::new(
        RuntimeManager::new(RuntimeManagerOptions {
            executable,
            lock_directory: locks.path().to_path_buf(),
            max_active_sessions: 1,
            max_concurrent_turns: 1,
            idle_timeout: Duration::ZERO,
            request_timeout: Duration::from_secs(2),
            extra_arguments: Vec::new(),
            environment: HostEnvironment::from_process(),
        })
        .expect("manager"),
    );
    let mut authorized = std::collections::HashSet::new();
    authorized.insert(workspace.path().to_path_buf());
    manager.configure_tui_bridge(authorized, owner_uid(workspace.path()));
    let process = ProcessIdentity::current().expect("process identity");
    let peer = TuiBridgePeer::new(owner_uid(workspace.path()).unwrap_or_default(), process);
    let harness = TuiBridgeHarness::new(manager.tui_bridge());
    let session_id = SessionId::new();
    (
        fake_directory,
        workspace,
        locks,
        manager,
        harness,
        peer,
        session_id,
    )
}

#[test]
fn tui_placeholder_blocks_rpc_without_consuming_rpc_capacity() {
    let (_fake_directory, workspace, _locks, manager, harness, peer, session_id) = manager_setup();
    let frame = serde_json::to_vec(&TuiBridgeRegister::new(
        session_id,
        workspace.path(),
        uuid::Uuid::new_v4(),
    ))
    .expect("register frame");
    let registration = harness.register_frame(&frame, &peer).expect("register TUI");

    assert_eq!(manager.active_count(), 0, "TUI is not an RPC child");
    assert!(manager.is_active(session_id));
    assert_eq!(
        manager.session_state(session_id),
        Some(pix_wire::SessionState::Idle)
    );
    assert_eq!(manager.client_count(session_id), Some(0));
    assert_eq!(
        manager.workspace(session_id),
        Some(workspace.path().canonicalize().expect("workspace"))
    );

    let discovered = DiscoveredSession {
        summary: SessionSummary {
            id: session_id,
            name: None,
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            message_count: 0,
            first_user_message: None,
        },
        path: workspace.path().join("not-created.jsonl"),
    };
    assert!(matches!(
        manager.open(workspace.path(), workspace.path(), &discovered),
        Err(RuntimeManagerError::TuiOwned(id)) if id == session_id
    ));
    assert_eq!(manager.active_count(), 0);
    assert!(matches!(
        manager.sweep_idle(),
        Ok(ref sessions) if sessions.is_empty()
    ));
    assert!(matches!(
        manager.release(session_id),
        Err(RuntimeManagerError::TuiOwned(id)) if id == session_id
    ));

    harness
        .disconnect(&registration.token)
        .expect("disconnect bridge");
    assert_eq!(
        manager.session_state(session_id),
        Some(pix_wire::SessionState::Unavailable)
    );
    assert!(matches!(
        manager.open(workspace.path(), workspace.path(), &discovered),
        Err(RuntimeManagerError::TuiUnavailable(id)) if id == session_id
    ));
    harness.release(&registration.token).expect("release owner");
    assert!(!manager.is_active(session_id));
}

#[test]
fn active_session_summary_exposes_tui_unreachable_without_wire_backend_identity() {
    let (_fake_directory, workspace, _locks, manager, harness, peer, session_id) = manager_setup();
    let bridge_instance_id = uuid::Uuid::new_v4();
    let frame = serde_json::to_vec(&TuiBridgeRegister::new(
        session_id,
        workspace.path(),
        bridge_instance_id,
    ))
    .expect("register frame");
    let registration = harness.register_frame(&frame, &peer).expect("register TUI");
    harness.disconnect(&registration.token).expect("disconnect");
    let summaries = manager.active_sessions();
    let summary = summaries
        .iter()
        .find(|summary| summary.session_id == session_id)
        .expect("TUI summary");
    assert_eq!(summary.state, pix_wire::SessionState::Unavailable);
    assert_eq!(summary.backend, RuntimeBackend::Tui);
    assert!(!summary.completed);
    assert_eq!(summary.client_count, 0);
    harness.release(&registration.token).expect("release");
}

#[test]
fn startup_restore_installs_unreachable_placeholder_before_rpc_open() {
    let (_fake_directory, workspace, locks, manager, harness, peer, session_id) = manager_setup();
    let lease = pix_core::SessionLease::acquire_for_tui(
        locks.path(),
        session_id,
        workspace.path(),
        &peer.process,
        uuid::Uuid::new_v4(),
    )
    .expect("external lease");
    let owner_path = lease.owner_path().to_path_buf();
    drop(lease);
    let fingerprints = [pix_core::workspace_fingerprint(workspace.path()).expect("fingerprint")]
        .into_iter()
        .collect();
    let recovery = pix_core::SessionLockStore::new(locks.path())
        .recover(&fingerprints)
        .expect("recovery");
    assert_eq!(recovery.owners().len(), 1);
    assert_eq!(
        recovery.owners()[0].state,
        pix_core::SessionRecoveryState::TuiUnreachable
    );

    manager
        .restore_tui_owner(&recovery.owners()[0], workspace.path())
        .expect("restore placeholder");
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        manager.session_state(session_id),
        Some(pix_wire::SessionState::Unavailable)
    );
    let discovered = DiscoveredSession {
        summary: SessionSummary {
            id: session_id,
            name: None,
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            message_count: 0,
            first_user_message: None,
        },
        path: workspace.path().join("not-created.jsonl"),
    };
    assert!(matches!(
        manager.open(workspace.path(), workspace.path(), &discovered),
        Err(RuntimeManagerError::TuiUnavailable(id)) if id == session_id
    ));
    assert!(owner_path.is_file());
    let owner = manager.tui_bridge().owner(session_id).expect("owner");
    harness
        .release(&owner.token)
        .expect("release restored owner");
    assert!(!owner_path.exists());
}

#[test]
fn rpc_first_claim_rejects_tui_register_on_the_same_session() {
    let (_fake_directory, workspace, _locks, manager, harness, peer, _placeholder_id) =
        manager_setup();
    let session_id = manager
        .create(workspace.path(), Some("RPC first".to_owned()))
        .expect("RPC session");
    let frame = serde_json::to_vec(&TuiBridgeRegister::new(
        session_id,
        workspace.path(),
        uuid::Uuid::new_v4(),
    ))
    .expect("register frame");
    assert!(matches!(
        harness.register_frame(&frame, &peer),
        Err(pix_core::TuiBridgeError::OwnerConflict(id)) if id == session_id
    ));
    manager.release(session_id).expect("release RPC");
}

#[test]
#[allow(clippy::too_many_lines)]
fn host_service_accepts_register_and_marks_socket_disconnect_unreachable() {
    let (_fake_directory, workspace, _locks, manager, _harness, _peer, session_id) =
        manager_setup();
    let service_directory = tempdir().expect("service directory");
    let config_path = service_directory.path().join("config.json");
    let mut config = HostConfig::new("TUI test host");
    WorkspaceRegistry::new(&mut config)
        .add(workspace.path(), Some("Project".to_owned()))
        .expect("authorize workspace");
    let store = ConfigStore::new(config_path);
    store.save(&config).expect("save config");
    let coordinator = Arc::new(PairingCoordinator::new(store));
    let host = generate_static_keypair().expect("host key");
    let listener = DirectTcpListener::bind(0).expect("direct listener");
    let socket_directory = tempdir().expect("bridge socket directory");
    let socket_path = socket_directory.path().join("tui-bridge.sock");
    let tui_socket = pix_core::TuiBridgeUnixSocket::bind(&socket_path).expect("bind bridge");
    let mut service = HostService::start_direct_with_tui_socket(
        listener,
        host.private_key,
        coordinator,
        Arc::new(HostState::with_asset_root(
            config,
            workspace.path().join(".pix-attachments"),
        )),
        Arc::clone(&manager),
        tui_socket,
    )
    .expect("start host service");

    let mut stream = UnixStream::connect(&socket_path).expect("connect bridge");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set bridge timeout");
    let bridge_instance_id = uuid::Uuid::new_v4();
    let frame = serde_json::to_vec(&TuiBridgeRegister::new(
        session_id,
        workspace.path(),
        bridge_instance_id,
    ))
    .expect("register frame");
    stream.write_all(&frame).expect("write register");
    stream.write_all(b"\n").expect("write register newline");
    stream.flush().expect("flush register");
    let mut response = String::new();
    BufReader::new(stream.try_clone().expect("clone bridge stream"))
        .read_line(&mut response)
        .expect("read register response");
    let response = serde_json::from_str::<serde_json::Value>(&response).expect("response JSON");
    assert_eq!(response["type"], "register_result");
    assert_eq!(response["granted"], true);
    assert_eq!(response["state"], "attached");
    assert!(manager.is_active(session_id));
    assert_eq!(
        manager.session_state(session_id),
        Some(pix_wire::SessionState::Idle)
    );
    let receiver = manager.subscribe(session_id).expect("subscribe TUI events");
    let event = TuiBridgeEventFrame::new(
        session_id,
        bridge_instance_id,
        1,
        "agent_start",
        serde_json::json!({}),
    );
    stream
        .write_all(&serde_json::to_vec(&event).expect("event frame"))
        .expect("write event");
    stream.write_all(b"\n").expect("write event newline");
    stream.flush().expect("flush event");
    assert!(matches!(
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("receive TUI event"),
        pix_core::pi_rpc::PiEvent::Event { event_type, .. } if event_type == "agent_start"
    ));
    let snapshot_manager = Arc::clone(&manager);
    let snapshot_thread = std::thread::spawn(move || {
        snapshot_manager.snapshot_with_timeout_and_cursor(session_id, Duration::from_secs(2))
    });
    let mut request = String::new();
    BufReader::new(stream.try_clone().expect("clone request stream"))
        .read_line(&mut request)
        .expect("read snapshot request");
    let request = serde_json::from_str::<serde_json::Value>(&request).expect("request JSON");
    assert_eq!(request["type"], "request");
    assert_eq!(request["command"], "snapshot");
    assert_eq!(request["sessionId"], session_id.to_string());
    let snapshot_response = serde_json::json!({
        "version": 1,
        "type": "response",
        "requestId": request["requestId"],
        "sessionId": session_id.to_string(),
        "command": "snapshot",
        "success": true,
        "snapshot": {
            "sessionId": session_id.to_string(),
            "sessionName": "TUI snapshot",
            "model": null,
            "thinkingLevel": "high",
            "isStreaming": true,
            "isCompacting": false,
            "pendingMessageCount": 0,
            "messages": [{"role": "user", "content": "hi"}],
            "inflightAssistant": null,
            "activeTools": [],
            "throughSequence": 1
        }
    });
    stream
        .write_all(snapshot_response.to_string().as_bytes())
        .expect("write snapshot response");
    stream.write_all(b"\n").expect("write snapshot newline");
    stream.flush().expect("flush snapshot response");
    let (snapshot, through_sequence) = snapshot_thread
        .join()
        .expect("snapshot thread")
        .expect("snapshot response");
    assert_eq!(through_sequence, Some(1));
    assert_eq!(snapshot.session_name.as_deref(), Some("TUI snapshot"));
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(
        manager.session_state(session_id),
        Some(pix_wire::SessionState::Running)
    );
    drop(stream);

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline
        && manager.session_state(session_id) != Some(pix_wire::SessionState::Unavailable)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        manager.session_state(session_id),
        Some(pix_wire::SessionState::Unavailable)
    );
    service.shutdown();
    assert!(!socket_path.exists());
    let owner = manager.tui_bridge().owner(session_id).expect("owner");
    manager
        .tui_bridge()
        .release(&owner.token)
        .expect("release owner");
    assert!(!manager.is_active(session_id));
}

#[test]
fn host_service_denies_register_when_rpc_owns_session() {
    let (_fake_directory, workspace, _locks, manager, _harness, _peer, _placeholder_id) =
        manager_setup();
    let session_id = manager
        .create(workspace.path(), Some("RPC owner".to_owned()))
        .expect("RPC session");
    let service_directory = tempdir().expect("service directory");
    let config_path = service_directory.path().join("config.json");
    let mut config = HostConfig::new("TUI conflict test host");
    WorkspaceRegistry::new(&mut config)
        .add(workspace.path(), Some("Project".to_owned()))
        .expect("authorize workspace");
    let store = ConfigStore::new(config_path);
    store.save(&config).expect("save config");
    let coordinator = Arc::new(PairingCoordinator::new(store));
    let host = generate_static_keypair().expect("host key");
    let listener = DirectTcpListener::bind(0).expect("direct listener");
    let socket_directory = tempdir().expect("bridge socket directory");
    let socket_path = socket_directory.path().join("tui-bridge.sock");
    let tui_socket = pix_core::TuiBridgeUnixSocket::bind(&socket_path).expect("bind bridge");
    let mut service = HostService::start_direct_with_tui_socket(
        listener,
        host.private_key,
        coordinator,
        Arc::new(HostState::with_asset_root(
            config,
            workspace.path().join(".pix-attachments"),
        )),
        Arc::clone(&manager),
        tui_socket,
    )
    .expect("start host service");

    let mut stream = UnixStream::connect(&socket_path).expect("connect bridge");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set bridge timeout");
    let frame = serde_json::to_vec(&TuiBridgeRegister::new(
        session_id,
        workspace.path(),
        uuid::Uuid::new_v4(),
    ))
    .expect("register frame");
    stream.write_all(&frame).expect("write register");
    stream.write_all(b"\n").expect("write register newline");
    stream.flush().expect("flush register");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read register response");
    let response = serde_json::from_str::<serde_json::Value>(&response).expect("response JSON");
    assert_eq!(response["type"], "register_result");
    assert_eq!(response["granted"], false);
    assert_eq!(response["error"], "conflict");
    assert!(manager.is_active(session_id));

    service.shutdown();
    manager.release(session_id).expect("release RPC owner");
    assert!(!manager.is_active(session_id));
}
