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
capture="$(dirname "$0")/pi-captured.jsonl"
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
    get_commands)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_commands\",\"success\":true,\"data\":{\"commands\":[{\"name\":\"review\",\"description\":\"Review current changes\",\"source\":\"extension\",\"sourceInfo\":{\"path\":\"/private/fake/extensions/review.ts\",\"source\":\"review\",\"scope\":\"user\",\"origin\":\"top-level\"}},{\"name\":\"fix-tests\",\"source\":\"prompt\",\"sourceInfo\":{\"path\":\"/private/fake/prompts/fix-tests.md\",\"scope\":\"project\",\"origin\":\"top-level\"}}]}}"
      ;;
    get_available_thinking_levels)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_available_thinking_levels\",\"success\":true,\"data\":{\"levels\":[\"off\",\"low\",\"high\"]}}"
      ;;
    get_session_stats)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_session_stats\",\"success\":true,\"data\":{\"sessionFile\":\"/private/fake/sessions/fake.jsonl\",\"sessionId\":\"fake-session\",\"tokens\":{\"input\":10,\"output\":5,\"total\":15},\"cost\":0.5,\"contextUsage\":{\"tokens\":100,\"contextWindow\":1000,\"percent\":10.0}}}"
      ;;
    prompt)
      printf '%s\n' "$line" >> "$capture"
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"streamed"}}'
      ;;
    steer)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"steer\",\"success\":true}"
      printf '%s\n' '{"type":"queue_update","steering":["Focus on error handling"],"followUp":["Then summarize"]}'
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

fn failing_snapshot_script() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempdir().expect("temporary fake Pi directory");
    let path = directory.path().join("failing-pi.sh");
    fs::write(
        &path,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  type=$(printf '%s' "$line" | sed -n 's/.*"type":"\([^"]*\)".*/\1/p')
  if [ "$type" = "get_state" ]; then
    printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":false,\"error\":\"not ready\"}"
  fi
done
"#,
    )
    .expect("write failing Pi");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("make failing Pi executable");
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
            max_concurrent_turns: 4,
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
            attachments: Vec::new(),
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
fn failed_create_snapshot_releases_the_runtime_and_session_lease() {
    let (_script, executable) = failing_snapshot_script();
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
            max_active_sessions: 1,
            max_concurrent_turns: 4,
            idle_timeout: Duration::from_secs(300),
            request_timeout: Duration::from_secs(2),
            extra_arguments: Vec::new(),
            environment: HostEnvironment::from_process(),
        })
        .expect("runtime manager"),
    );
    let mut dispatcher =
        HostProtocolDispatcher::new(Arc::new(HostState::new(config)), Arc::clone(&manager));

    let response = dispatcher.dispatch(request(
        20,
        ClientRequest::SessionCreate {
            workspace_id,
            name: Some("will fail".to_owned()),
        },
    ));
    assert!(matches!(response[0].event, ServerEvent::RequestAck));
    assert!(matches!(
        response[1].event,
        ServerEvent::Error {
            code: ErrorCode::PiUnavailable,
            retryable: true,
            ..
        }
    ));
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        manager.reap_exited().expect("reap after failed create"),
        Vec::new()
    );
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
    let _ = dispatcher.dispatch(request(
        1,
        ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    ));
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
    let snapshot = dispatcher.dispatch(request(
        1,
        ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    ));
    match &snapshot[0].event {
        ServerEvent::HostSnapshot { snapshot } => {
            assert!(
                snapshot.workspaces.iter().all(|item| {
                    item.availability == pix_wire::WorkspaceAvailability::Unavailable
                })
            );
        }
        event => panic!("expected host snapshot, got {event:?}"),
    }
}

#[cfg(unix)]
#[test]
fn availability_cache_does_not_bypass_session_list_authorization() {
    use std::os::unix::fs::symlink;

    let (_script, workspace, _locks, mut dispatcher, _manager, workspace_id) = setup();
    let first = dispatcher.dispatch(request(
        1,
        ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    ));
    match &first[0].event {
        ServerEvent::HostSnapshot { snapshot } => {
            assert!(
                snapshot.workspaces.iter().all(|item| {
                    item.availability == pix_wire::WorkspaceAvailability::Available
                })
            );
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

fn attached_session_id(
    dispatcher: &mut HostProtocolDispatcher,
    workspace_id: uuid::Uuid,
) -> String {
    let created = dispatcher.dispatch(request(
        90,
        ClientRequest::SessionCreate {
            workspace_id,
            name: None,
        },
    ));
    match &created[1].event {
        ServerEvent::SessionSnapshot { snapshot } => snapshot.id.clone(),
        event => panic!("expected snapshot, got {event:?}"),
    }
}

#[test]
fn host_snapshot_advertises_capabilities_and_gates_session_enrichment() {
    let (script, _workspace, _locks, mut dispatcher, _manager, workspace_id) = setup();

    let snapshot = dispatcher.dispatch(request(
        1,
        ClientRequest::HostSnapshot {
            capabilities: Vec::new(),
        },
    ));
    match &snapshot[0].event {
        ServerEvent::HostSnapshot { snapshot } => {
            assert!(snapshot.capabilities.contains(&"commands.v1".to_owned()));
            assert!(snapshot.capabilities.contains(&"queue.v1".to_owned()));
            assert!(snapshot.capabilities.contains(&"attachments.v1".to_owned()));
            assert!(snapshot.capabilities.contains(&"usage.v1".to_owned()));
            assert!(
                snapshot
                    .capabilities
                    .contains(&"thinking_levels.v1".to_owned())
            );
        }
        event => panic!("expected host snapshot, got {event:?}"),
    }

    let session_id = attached_session_id(&mut dispatcher, workspace_id);
    // The connection never declared capabilities, so the snapshot omits every
    // extension field and keeps the inferred thinking-level fallback.
    let snapshot = dispatcher.dispatch(request(
        2,
        ClientRequest::SessionAttach {
            session_id: session_id.clone(),
        },
    ));
    match &snapshot[0].event {
        ServerEvent::SessionSnapshot { snapshot } => {
            assert!(snapshot.commands.is_empty());
            assert!(snapshot.queue.is_none());
            assert!(snapshot.usage.is_none());
            assert!(
                snapshot
                    .model
                    .as_ref()
                    .expect("model")
                    .thinking_levels
                    .len()
                    > 1
            );
        }
        event => panic!("expected snapshot, got {event:?}"),
    }
    drop(script);
}

#[test]
fn declared_capabilities_enrich_snapshots_with_pi_authority() {
    let (script, _workspace, _locks, mut dispatcher, _manager, workspace_id) = setup();
    let _ = dispatcher.dispatch(request(
        1,
        ClientRequest::HostSnapshot {
            capabilities: vec![
                "commands.v1".to_owned(),
                "usage.v1".to_owned(),
                "thinking_levels.v1".to_owned(),
            ],
        },
    ));

    let session_id = attached_session_id(&mut dispatcher, workspace_id);
    let snapshot = dispatcher.dispatch(request(
        2,
        ClientRequest::SessionAttach {
            session_id: session_id.clone(),
        },
    ));
    match &snapshot[0].event {
        ServerEvent::SessionSnapshot { snapshot } => {
            let commands = &snapshot.commands;
            assert_eq!(commands.len(), 2);
            assert_eq!(commands[0].name, "review");
            assert_eq!(commands[0].source, pix_wire::CommandSource::Extension);
            assert_eq!(commands[0].scope, Some(pix_wire::CommandScope::User));
            let encoded = serde_json::to_string(&commands).expect("encode commands");
            assert!(
                !encoded.contains("/private/fake"),
                "host paths must not reach clients: {encoded}"
            );

            let usage = snapshot.usage.as_ref().expect("usage");
            assert_eq!(usage.tokens_total, 15);
            assert!((usage.cost - 0.5).abs() < f64::EPSILON);
            assert!(
                usage
                    .context_percent
                    .is_some_and(|percent| (percent - 10.0).abs() < f64::EPSILON)
            );
            assert!(
                !serde_json::to_string(&usage)
                    .expect("encode usage")
                    .contains("/private/fake"),
                "host paths must not reach clients"
            );

            assert_eq!(
                snapshot.model.as_ref().expect("model").thinking_levels,
                vec![
                    pix_wire::ThinkingLevel::Off,
                    pix_wire::ThinkingLevel::Low,
                    pix_wire::ThinkingLevel::High
                ]
            );
        }
        event => panic!("expected snapshot, got {event:?}"),
    }
    drop(script);
}

#[test]
fn queue_updates_are_gated_live_and_cached_for_reconnecting_clients() {
    let (script, workspace, _locks, mut legacy, manager, workspace_id) = setup();
    // Share the authorized HostState so the reconnecting dispatcher sees the
    // same workspace registry as the first connection.
    let mut shared_config = HostConfig::new("Test Mac");
    WorkspaceRegistry::new(&mut shared_config)
        .add(workspace.path(), Some("Project".to_owned()))
        .expect("authorize workspace");
    let session_id = attached_session_id(&mut legacy, workspace_id);

    // A connection without `queue.v1` triggers Pi's queue_update but must not
    // receive the event on the wire.
    let _ = legacy.dispatch(request(
        2,
        ClientRequest::SessionSteer {
            session_id: session_id.clone(),
            content: "Focus on error handling".to_owned(),
            attachments: Vec::new(),
        },
    ));
    let events = legacy.drain_events();
    assert!(
        events.is_empty(),
        "queue events must be gated behind queue.v1: {events:?}"
    );

    // A reconnecting client that declares queue.v1 recovers the queue text
    // from the runtime cache without another Pi turn.
    let mut modern = HostProtocolDispatcher::new(
        Arc::new(HostState::new(shared_config)),
        Arc::clone(&manager),
    );
    let _ = modern.dispatch(request(
        1,
        ClientRequest::HostSnapshot {
            capabilities: vec!["queue.v1".to_owned()],
        },
    ));
    let snapshot = modern.dispatch(request(
        2,
        ClientRequest::SessionAttach {
            session_id: session_id.clone(),
        },
    ));
    match &snapshot[0].event {
        ServerEvent::SessionSnapshot { snapshot } => {
            let queue = snapshot.queue.as_ref().expect("cached queue");
            assert_eq!(queue.steering, ["Focus on error handling"]);
            assert_eq!(queue.follow_up, ["Then summarize"]);
        }
        event => panic!("expected snapshot, got {event:?}"),
    }

    // The same modern connection also receives live queue events.
    let _ = modern.dispatch(request(
        3,
        ClientRequest::SessionSteer {
            session_id,
            content: "Also check types".to_owned(),
            attachments: Vec::new(),
        },
    ));
    let events = modern.drain_events();
    assert!(
        events
            .iter()
            .any(|envelope| matches!(envelope.event, ServerEvent::SessionQueue { .. })),
        "expected a session queue event: {events:?}"
    );
    drop(script);
}

#[test]
fn attachment_uploads_assemble_into_pi_prompt_images() {
    let (script, _workspace, _locks, mut dispatcher, _manager, workspace_id) = setup();
    let _ = dispatcher.dispatch(request(
        1,
        ClientRequest::HostSnapshot {
            capabilities: vec!["attachments.v1".to_owned()],
        },
    ));
    let session_id = attached_session_id(&mut dispatcher, workspace_id);

    let begin = dispatcher.dispatch(request(
        2,
        ClientRequest::AttachmentBegin {
            session_id: session_id.clone(),
            attachment_id: "att-1".to_owned(),
            mime_type: "image/png".to_owned(),
            size: 5,
        },
    ));
    assert!(matches!(begin[0].event, ServerEvent::RequestAck));

    let chunk = dispatcher.dispatch(request(
        3,
        ClientRequest::AttachmentChunk {
            attachment_id: "att-1".to_owned(),
            data: "aGVsbG8=".to_owned(),
        },
    ));
    assert!(matches!(chunk[0].event, ServerEvent::RequestAck));

    let finish = dispatcher.dispatch(request(
        4,
        ClientRequest::AttachmentFinish {
            attachment_id: "att-1".to_owned(),
        },
    ));
    assert!(matches!(finish[0].event, ServerEvent::RequestAck));

    let prompt = dispatcher.dispatch(request(
        5,
        ClientRequest::SessionPrompt {
            session_id,
            content: "what is this".to_owned(),
            attachments: vec!["att-1".to_owned()],
        },
    ));
    assert!(matches!(prompt[0].event, ServerEvent::RequestAck));

    let capture = fs::read_to_string(script.path().join("pi-captured.jsonl")).expect("capture");
    let prompt_record = capture
        .lines()
        .rev()
        .find(|line| line.contains("\"type\":\"prompt\""))
        .expect("captured prompt");
    let record: serde_json::Value = serde_json::from_str(prompt_record).expect("prompt json");
    let images = record["images"].as_array().expect("images array");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["type"], "image");
    assert_eq!(images[0]["mimeType"], "image/png");
    assert_eq!(images[0]["data"], "aGVsbG8=");
}

#[test]
fn attachment_transfers_fail_closed_without_capability_or_finish() {
    let (script, _workspace, _locks, mut dispatcher, _manager, workspace_id) = setup();
    let session_id = attached_session_id(&mut dispatcher, workspace_id);

    // Without the attachments.v1 declaration even begin is rejected.
    let rejected = dispatcher.dispatch(request(
        1,
        ClientRequest::AttachmentBegin {
            session_id: session_id.clone(),
            attachment_id: "att-1".to_owned(),
            mime_type: "image/png".to_owned(),
            size: 5,
        },
    ));
    assert!(matches!(
        rejected[0].event,
        ServerEvent::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));

    let _ = dispatcher.dispatch(request(
        2,
        ClientRequest::HostSnapshot {
            capabilities: vec!["attachments.v1".to_owned()],
        },
    ));

    // Referencing an upload that never finished fails and does not prompt Pi.
    let unfinished = dispatcher.dispatch(request(
        3,
        ClientRequest::SessionPrompt {
            session_id: session_id.clone(),
            content: "no image".to_owned(),
            attachments: vec!["att-1".to_owned()],
        },
    ));
    assert!(matches!(
        unfinished[0].event,
        ServerEvent::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));

    // A chunk overflow removes the staging entry instead of buffering
    // unbounded bytes.
    let _ = dispatcher.dispatch(request(
        4,
        ClientRequest::AttachmentBegin {
            session_id: session_id.clone(),
            attachment_id: "att-2".to_owned(),
            mime_type: "image/png".to_owned(),
            size: 2,
        },
    ));
    let overflow = dispatcher.dispatch(request(
        5,
        ClientRequest::AttachmentChunk {
            attachment_id: "att-2".to_owned(),
            data: "aGVsbG8=".to_owned(),
        },
    ));
    assert!(matches!(
        overflow[0].event,
        ServerEvent::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));
    let missing = dispatcher.dispatch(request(
        6,
        ClientRequest::AttachmentFinish {
            attachment_id: "att-2".to_owned(),
        },
    ));
    assert!(matches!(
        missing[0].event,
        ServerEvent::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));
    drop(script);
}
