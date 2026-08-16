#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use pix_core::{
    HostConfig, HostEnvironment, HostProtocolDispatcher, HostState, RuntimeManager,
    RuntimeManagerOptions, WorkspaceRegistry,
};
use pix_wire::{
    ClientEnvelope, ClientRequest, ErrorCode, PROTOCOL_MAJOR, ServerEvent, SessionState,
};
use tempfile::tempdir;

fn fake_pi_script() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempdir().expect("temporary fake Pi directory");
    let path = directory.path().join("fake-pi.sh");
    fs::write(
        &path,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  type=$(printf '%s' "$line" | sed -n 's/.*"type":"\([^"]*\)".*/\1/p')
  case "$type" in
    get_state)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"sessionId\":\"fake-session\",\"sessionName\":\"Fake session\",\"model\":{\"provider\":\"fake\",\"id\":\"model\",\"name\":\"Fake Model\",\"reasoning\":true},\"thinkingLevel\":\"medium\",\"isStreaming\":false,\"isCompacting\":false,\"pendingMessageCount\":0}}"
      ;;
    get_messages)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_messages\",\"success\":true,\"data\":{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}}"
      ;;
    get_available_models)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_available_models\",\"success\":true,\"data\":{\"models\":[{\"provider\":\"fake\",\"id\":\"model\",\"name\":\"Fake Model\",\"reasoning\":true}]}}"
      ;;
    prompt)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"streamed"}}'
      ;;
    *)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"$type\",\"success\":true}"
      ;;
  esac
done
"#,
    )
    .expect("write fake Pi");
    let mut permissions = fs::metadata(&path).expect("fake Pi metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make fake Pi executable");
    (directory, path)
}

fn request(request_id: u64, request: ClientRequest) -> ClientEnvelope {
    ClientEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id,
        request,
    }
}

fn setup() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    HostProtocolDispatcher,
    Arc<RuntimeManager>,
    uuid::Uuid,
) {
    let (script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("workspace");
    let locks = tempdir().expect("lock directory");
    let mut config = HostConfig::new("Test Mac");
    let workspace_id = WorkspaceRegistry::new(&mut config)
        .add(workspace.path(), Some("Project".to_owned()))
        .expect("authorize workspace")
        .id;
    let manager = Arc::new(
        RuntimeManager::new(RuntimeManagerOptions {
            executable,
            lock_directory: locks.path().to_path_buf(),
            max_active_sessions: 4,
            idle_timeout: Duration::from_secs(300),
            request_timeout: Duration::from_secs(2),
            extra_arguments: Vec::new(),
            environment: HostEnvironment::from_process(),
        })
        .expect("runtime manager"),
    );
    let dispatcher =
        HostProtocolDispatcher::new(Arc::new(HostState::new(config)), Arc::clone(&manager));
    (
        script_directory,
        workspace,
        locks,
        dispatcher,
        manager,
        workspace_id,
    )
}

#[test]
fn host_defaults_are_read_without_starting_a_pi_session() {
    let (_script, _workspace, _locks, mut dispatcher, manager, _workspace_id) = setup();

    let response = dispatcher.dispatch(request(9, ClientRequest::HostDefaults));
    assert_eq!(response.len(), 1);
    match &response[0].event {
        ServerEvent::HostDefaults { defaults } => {
            if let Some(model) = &defaults.model {
                assert!(
                    defaults
                        .models
                        .iter()
                        .any(|candidate| candidate.provider == model.provider
                            && candidate.id == model.id)
                );
            }
        }
        event => panic!("expected host defaults, got {event:?}"),
    }
    assert_eq!(manager.active_count(), 0);
}

#[test]
fn dispatches_create_prompt_models_and_live_events_without_duplicate_history() {
    let (_script, _workspace, _locks, mut dispatcher, manager, workspace_id) = setup();
    let created = dispatcher.dispatch(request(
        10,
        ClientRequest::SessionCreate {
            workspace_id,
            name: Some("Mobile session".to_owned()),
        },
    ));
    assert_eq!(created.len(), 2);
    assert!(matches!(created[0].event, ServerEvent::RequestAck));
    let session_id = match &created[1].event {
        ServerEvent::SessionSnapshot { snapshot } => {
            assert_eq!(snapshot.state, SessionState::Idle);
            assert_eq!(snapshot.messages.len(), 1);
            snapshot.id.clone()
        }
        event => panic!("expected snapshot, got {event:?}"),
    };
    assert_eq!(manager.active_count(), 1);
    assert_eq!(
        manager.client_count(session_id.parse().expect("session ID")),
        Some(1)
    );

    let prompt = dispatcher.dispatch(request(
        11,
        ClientRequest::SessionPrompt {
            session_id: session_id.clone(),
            content: "continue".to_owned(),
        },
    ));
    assert!(matches!(prompt[0].event, ServerEvent::RequestAck));
    assert_eq!(prompt[0].request_id, Some(11));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mapped = loop {
        if let Some(event) = dispatcher.drain_events().into_iter().next() {
            break event;
        }
        assert!(std::time::Instant::now() < deadline, "stream event timeout");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(matches!(mapped.event, ServerEvent::AssistantDelta { .. }));
    assert_eq!(mapped.request_id, None);

    let models = dispatcher.dispatch(request(
        12,
        ClientRequest::ModelList {
            session_id: session_id.clone(),
        },
    ));
    match &models[0].event {
        ServerEvent::ModelList { models, .. } => {
            assert_eq!(models[0].name, "Fake Model");
            assert!(models[0].reasoning);
        }
        event => panic!("expected models, got {event:?}"),
    }

    dispatcher.disconnect();
    assert_eq!(
        manager.client_count(session_id.parse().expect("session ID")),
        Some(0)
    );
    assert_eq!(manager.active_count(), 1, "disconnect must not stop Pi");
    manager
        .release(session_id.parse().expect("session ID"))
        .expect("release runtime");
}

#[test]
fn creates_an_untitled_session_without_a_display_name() {
    let (_script, _workspace, _locks, mut dispatcher, manager, workspace_id) = setup();
    let created = dispatcher.dispatch(request(
        10,
        ClientRequest::SessionCreate {
            workspace_id,
            name: None,
        },
    ));
    assert!(matches!(created[0].event, ServerEvent::RequestAck));
    let session_id = match &created[1].event {
        ServerEvent::SessionSnapshot { snapshot } => snapshot.id.clone(),
        event => panic!("expected snapshot, got {event:?}"),
    };
    manager
        .release(session_id.parse().expect("session ID"))
        .expect("release runtime");
}

#[test]
fn rejects_unknown_workspaces_and_commands_before_attach() {
    let (_script, _workspace, _locks, mut dispatcher, _manager, _workspace_id) = setup();
    let unknown = dispatcher.dispatch(request(
        1,
        ClientRequest::SessionCreate {
            workspace_id: uuid::Uuid::new_v4(),
            name: Some("Nope".to_owned()),
        },
    ));
    assert!(matches!(
        unknown[0].event,
        ServerEvent::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));

    let unattached = dispatcher.dispatch(request(
        2,
        ClientRequest::SessionAbort {
            session_id: uuid::Uuid::new_v4().to_string(),
        },
    ));
    assert!(matches!(
        unattached[0].event,
        ServerEvent::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
}

#[test]
fn catalog_requests_emit_payload_free_timings() {
    let (tx, rx) = std::sync::mpsc::channel();
    pix_core::install_diagnostic_sink(move |event, body| {
        let _ = tx.send((event.to_string(), body.clone()));
    });
    let (_script, workspace, _locks, mut dispatcher, _manager, workspace_id) = setup();
    let _ = dispatcher.dispatch(request(1, ClientRequest::HostSnapshot));
    let listed = dispatcher.dispatch(request(
        2,
        ClientRequest::SessionList {
            workspace_id,
            limit: None,
        },
    ));
    assert!(matches!(listed[0].event, ServerEvent::SessionList { .. }));

    let mut records = Vec::new();
    while let Ok(record) = rx.try_recv() {
        records.push(record);
    }
    assert!(
        records.iter().any(|(event, _)| event == "host.snapshot"),
        "missing host.snapshot timing: {records:?}"
    );
    assert!(
        records.iter().any(|(event, _)| event == "session.list"),
        "missing session.list timing: {records:?}"
    );

    let workspace_path = workspace.path().display().to_string();
    for (event, body) in records {
        let rendered = body.to_string();
        assert!(
            !rendered.contains(&workspace_path),
            "{event} leaked a workspace path: {rendered}"
        );
        for forbidden in ["secret", "prompt", "message", "cwd", "token", "proof"] {
            assert!(
                !rendered.contains(forbidden),
                "{event} leaked {forbidden}: {rendered}"
            );
        }
        if event == "host.snapshot" {
            assert!(body.get("validation_ms").is_some());
            assert!(body.get("workspace_count").is_some());
            assert!(body.get("response_bytes").is_some());
        }
        if event == "session.list" {
            assert!(body.get("enumerate_ms").is_some());
            assert!(body.get("scan_ms").is_some());
            assert!(body.get("session_count").is_some());
            assert!(body.get("response_bytes").is_some());
        }
    }
}

#[test]
fn snapshot_marks_a_missing_workspace_unavailable() {
    let (_script, workspace, _locks, mut dispatcher, _manager, _workspace_id) = setup();
    std::fs::remove_dir_all(workspace.path()).expect("remove workspace");
    let snapshot = dispatcher.dispatch(request(1, ClientRequest::HostSnapshot));
    match &snapshot[0].event {
        ServerEvent::HostSnapshot { snapshot } => {
            assert!(snapshot.workspaces.iter().all(|item| {
                item.availability == pix_wire::WorkspaceAvailability::Unavailable
            }));
        }
        event => panic!("expected host snapshot, got {event:?}"),
    }
}

#[cfg(unix)]
#[test]
fn availability_cache_does_not_bypass_session_list_authorization() {
    use std::os::unix::fs::symlink;

    let (_script, workspace, _locks, mut dispatcher, _manager, workspace_id) = setup();
    let first = dispatcher.dispatch(request(1, ClientRequest::HostSnapshot));
    match &first[0].event {
        ServerEvent::HostSnapshot { snapshot } => {
            assert!(snapshot.workspaces.iter().all(|item| {
                item.availability == pix_wire::WorkspaceAvailability::Available
            }));
        }
        event => panic!("expected host snapshot, got {event:?}"),
    }

    let original = workspace.path().to_path_buf();
    let replacement = original.with_extension("replacement");
    std::fs::create_dir(&replacement).expect("replacement");
    std::fs::remove_dir_all(&original).expect("remove original");
    symlink(&replacement, &original).expect("replace with symlink");

    let listed = dispatcher.dispatch(request(
        2,
        ClientRequest::SessionList {
            workspace_id,
            limit: Some(10),
        },
    ));
    assert!(matches!(
        listed[0].event,
        ServerEvent::Error {
            code: ErrorCode::Unauthorized,
            ..
        }
    ));
}
