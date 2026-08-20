use std::fs;
use std::process::Command;
use std::time::Duration;

use pix_core::pi_rpc::PiCommand;
use pix_core::{
    HostEnvironment, PiRuntime, PiRuntimeOptions, SessionId, SessionLaunch, SessionLease,
};
use tempfile::tempdir;

#[cfg(unix)]
fn fake_pi_script() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("temporary fake Pi directory");
    let path = directory.path().join("fake-pi.sh");
    fs::write(
        &path,
        r#"#!/bin/sh
printf '%s\n' '{"type":"extension_ui_request","id":"status-1","method":"setStatus","statusKey":"fake","statusText":"ready"}'
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  type=$(printf '%s' "$line" | sed -n 's/.*"type":"\([^"]*\)".*/\1/p')
  case "$type" in
    get_state)
      printf '%s\n' "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"before after\"}}"
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"sessionId\":\"fake-session\",\"sessionName\":\"Fake session\",\"model\":{\"provider\":\"fake\",\"id\":\"model\"},\"thinkingLevel\":\"medium\",\"isStreaming\":false,\"isCompacting\":false,\"pendingMessageCount\":0}}"
      ;;
    get_messages)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_messages\",\"success\":true,\"data\":{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}}"
      ;;
    prompt)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      ;;
    abort)
      printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"abort\",\"success\":false,\"error\":\"nothing to abort\"}"
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

#[cfg(unix)]
fn exiting_pi_script() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("temporary fake Pi directory");
    let path = directory.path().join("exiting-pi.sh");
    fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write exiting Pi");
    let mut permissions = fs::metadata(&path)
        .expect("exiting Pi metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make exiting Pi executable");
    (directory, path)
}

#[cfg(unix)]
#[test]
fn correlates_responses_while_streaming_events() {
    let (_script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let locks = tempdir().expect("temporary lock directory");
    let session_id = SessionId::new();
    let runtime = PiRuntime::start(&PiRuntimeOptions {
        executable,
        workspace: workspace.path().to_path_buf(),
        lock_directory: locks.path().to_path_buf(),
        launch: SessionLaunch::Create {
            id: session_id,
            name: Some("Fake session".to_owned()),
        },
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    })
    .expect("start fake Pi");
    let events = runtime.rpc().subscribe();

    let response = runtime
        .rpc()
        .request(&PiCommand::GetState, Duration::from_secs(2))
        .expect("get state response");
    assert_eq!(response.command, "get_state");
    assert_eq!(
        response.data.expect("state data")["sessionId"],
        "fake-session"
    );

    let received: Vec<_> = (0..2)
        .map(|_| {
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("Pi event")
        })
        .collect();
    assert!(
        received
            .iter()
            .any(|event| format!("{event:?}").contains("extension_ui_request"))
    );
    assert!(received.iter().any(
        |event| format!("{event:?}").contains("before\\u{2028}after")
            || format!("{event:?}").contains("before\u{2028}after")
    ));

    let error = runtime
        .rpc()
        .request(&PiCommand::Abort, Duration::from_secs(2))
        .expect_err("fake Pi rejects abort");
    assert!(error.to_string().contains("nothing to abort"));
    runtime.stop().expect("stop fake Pi");
    SessionLease::acquire(locks.path(), session_id).expect("lease released after stop");
}

#[cfg(unix)]
#[test]
fn refuses_two_runtime_writers_for_one_session() {
    let (_script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let locks = tempdir().expect("temporary lock directory");
    let session_id = SessionId::new();
    let options = PiRuntimeOptions {
        executable,
        workspace: workspace.path().to_path_buf(),
        lock_directory: locks.path().to_path_buf(),
        launch: SessionLaunch::Existing {
            id: session_id,
            reference: "fake-session".to_owned(),
        },
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    };
    let first = PiRuntime::start(&options).expect("first runtime");
    let Err(second) = PiRuntime::start(&options) else {
        panic!("second writer must fail");
    };
    assert!(second.to_string().contains("already owned"));
    first.stop().expect("stop first runtime");
}

#[cfg(unix)]
#[test]
fn launch_arguments_do_not_use_a_shell() {
    let (_script_directory, executable) = fake_pi_script();
    let output = Command::new(executable)
        .arg("--literal=$(touch should-not-exist)")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run fake Pi directly");
    assert!(output.status.success());
    assert!(!std::path::Path::new("should-not-exist").exists());
}

#[cfg(unix)]
#[test]
fn runtime_manager_snapshots_and_sweeps_completed_idle_sessions() {
    use pix_core::{RuntimeManager, RuntimeManagerOptions};

    let (_script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let locks = tempdir().expect("temporary lock directory");
    let manager = RuntimeManager::new(RuntimeManagerOptions {
        executable,
        lock_directory: locks.path().to_path_buf(),
        max_active_sessions: 1,
        max_concurrent_turns: 4,
        idle_timeout: Duration::ZERO,
        request_timeout: Duration::from_secs(2),
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    })
    .expect("runtime manager");
    let session_id = manager
        .create(workspace.path(), Some("Managed session".to_owned()))
        .expect("create managed session");
    assert_eq!(
        manager.session_state(session_id),
        Some(pix_wire::SessionState::Starting)
    );

    let snapshot = manager.snapshot(session_id).expect("session snapshot");
    assert_eq!(snapshot.session_id, "fake-session");
    assert_eq!(snapshot.session_name.as_deref(), Some("Fake session"));
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(
        manager.session_state(session_id),
        Some(pix_wire::SessionState::Idle)
    );
    manager.detach(session_id).expect("detach client");
    assert_eq!(manager.sweep_idle().expect("sweep idle"), vec![session_id]);
    assert_eq!(manager.active_count(), 0);
}

#[cfg(unix)]
#[test]
fn runtime_manager_never_evicts_an_attached_session_for_capacity() {
    use pix_core::{RuntimeManager, RuntimeManagerOptions};

    let (_script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let locks = tempdir().expect("temporary lock directory");
    let manager = RuntimeManager::new(RuntimeManagerOptions {
        executable,
        lock_directory: locks.path().to_path_buf(),
        max_active_sessions: 1,
        max_concurrent_turns: 4,
        idle_timeout: Duration::ZERO,
        request_timeout: Duration::from_secs(2),
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    })
    .expect("runtime manager");
    let first = manager
        .create(workspace.path(), Some("First".to_owned()))
        .expect("first session");

    let error = manager
        .create(workspace.path(), Some("Second".to_owned()))
        .expect_err("attached session must retain capacity");
    assert!(error.to_string().contains("limit 1 reached"));
    assert_eq!(manager.active_count(), 1);
    manager.release(first).expect("release first session");
}

#[cfg(unix)]
#[test]
fn runtime_manager_evicts_the_completed_unattached_runtime_at_capacity() {
    use pix_core::{RuntimeManager, RuntimeManagerOptions};

    let (_script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let locks = tempdir().expect("temporary lock directory");
    let manager = RuntimeManager::new(RuntimeManagerOptions {
        executable,
        lock_directory: locks.path().to_path_buf(),
        max_active_sessions: 1,
        max_concurrent_turns: 4,
        idle_timeout: Duration::from_secs(300),
        request_timeout: Duration::from_secs(2),
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    })
    .expect("runtime manager");
    let first = manager
        .create(workspace.path(), Some("First".to_owned()))
        .expect("first session");
    manager.snapshot(first).expect("completed first snapshot");
    manager.detach(first).expect("detach first session");

    let second = manager
        .create(workspace.path(), Some("Second".to_owned()))
        .expect("second session replaces idle first");
    assert_eq!(manager.active_count(), 1);
    assert_eq!(manager.client_count(first), None);
    assert_eq!(manager.client_count(second), Some(1));
    manager.release(second).expect("release second session");
}

#[cfg(unix)]
#[test]
fn runtime_manager_rechecks_an_unattached_runtime_even_when_cache_says_running() {
    use pix_core::{RuntimeManager, RuntimeManagerOptions};

    let (_script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let locks = tempdir().expect("temporary lock directory");
    let manager = RuntimeManager::new(RuntimeManagerOptions {
        executable,
        lock_directory: locks.path().to_path_buf(),
        max_active_sessions: 1,
        max_concurrent_turns: 4,
        idle_timeout: Duration::from_secs(300),
        request_timeout: Duration::from_secs(2),
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    })
    .expect("runtime manager");
    let first = manager
        .create(workspace.path(), Some("First".to_owned()))
        .expect("first session");
    manager.mark_completed(first, false);
    manager.detach(first).expect("detach first session");

    let second = manager
        .create(workspace.path(), Some("Second".to_owned()))
        .expect("second session replaces stale cached state");
    assert_eq!(manager.active_count(), 1);
    assert_eq!(manager.client_count(first), None);
    assert_eq!(manager.client_count(second), Some(1));
    manager.release(second).expect("release second session");
}

#[cfg(unix)]
#[test]
fn runtime_manager_reaps_an_exited_pi_child() {
    use pix_core::{RuntimeManager, RuntimeManagerOptions};

    let (_script_directory, executable) = exiting_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let locks = tempdir().expect("temporary lock directory");
    let manager = RuntimeManager::new(RuntimeManagerOptions {
        executable,
        lock_directory: locks.path().to_path_buf(),
        max_active_sessions: 1,
        max_concurrent_turns: 4,
        idle_timeout: Duration::from_secs(300),
        request_timeout: Duration::from_secs(2),
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    })
    .expect("runtime manager");
    let session_id = manager
        .create(workspace.path(), Some("Exiting".to_owned()))
        .expect("create session");

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut reaped = Vec::new();
    while std::time::Instant::now() < deadline {
        reaped = manager.reap_exited().expect("reap exited");
        if !reaped.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(reaped, vec![session_id]);
    assert_eq!(manager.active_count(), 0);
}

#[cfg(unix)]
#[test]
fn runtime_manager_limits_turns_separately_from_resident_runtimes() {
    use pix_core::{RuntimeManager, RuntimeManagerOptions};

    let (_script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let locks = tempdir().expect("temporary lock directory");
    let manager = RuntimeManager::new(RuntimeManagerOptions {
        executable,
        lock_directory: locks.path().to_path_buf(),
        max_active_sessions: 2,
        max_concurrent_turns: 1,
        idle_timeout: Duration::from_secs(300),
        request_timeout: Duration::from_secs(2),
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    })
    .expect("runtime manager");
    let first = manager
        .create(workspace.path(), Some("First".to_owned()))
        .expect("first session");
    let second = manager
        .create(workspace.path(), Some("Second".to_owned()))
        .expect("second session");

    manager
        .request(
            first,
            &PiCommand::Prompt {
                message: "first".to_owned(),
                streaming_behavior: None,
            },
        )
        .expect("first turn admission");
    let error = manager
        .request(
            second,
            &PiCommand::Prompt {
                message: "second".to_owned(),
                streaming_behavior: None,
            },
        )
        .expect_err("second turn exceeds the independent limit");
    assert!(error.to_string().contains("concurrent Pi turn limit 1"));
    assert_eq!(manager.active_count(), 2);

    manager.mark_state(first, pix_wire::SessionState::Idle);
    manager
        .request(
            second,
            &PiCommand::Prompt {
                message: "second".to_owned(),
                streaming_behavior: None,
            },
        )
        .expect("second turn after first settles");
    manager.release(first).expect("release first session");
    manager.release(second).expect("release second session");
}

#[cfg(unix)]
#[test]
fn runtime_manager_reuses_an_open_session() {
    use chrono::Utc;
    use pix_core::{DiscoveredSession, RuntimeManager, RuntimeManagerOptions, SessionSummary};

    let (_script_directory, executable) = fake_pi_script();
    let workspace = tempdir().expect("temporary workspace");
    let sessions = tempdir().expect("temporary session directory");
    let locks = tempdir().expect("temporary lock directory");
    let session_id = SessionId::new();
    let session_path = sessions.path().join("existing.jsonl");
    fs::write(&session_path, "{}\n").expect("session fixture");
    let discovered = DiscoveredSession {
        summary: SessionSummary {
            id: session_id,
            name: Some("Existing".to_owned()),
            created_at: Utc::now(),
            modified_at: Utc::now(),
            message_count: 0,
            first_user_message: None,
        },
        path: session_path,
    };
    let manager = RuntimeManager::new(RuntimeManagerOptions {
        executable,
        lock_directory: locks.path().to_path_buf(),
        max_active_sessions: 4,
        max_concurrent_turns: 4,
        idle_timeout: Duration::from_secs(300),
        request_timeout: Duration::from_secs(2),
        extra_arguments: Vec::new(),
        environment: HostEnvironment::from_process(),
    })
    .expect("runtime manager");

    manager
        .open(workspace.path(), sessions.path(), &discovered)
        .expect("first open");
    manager
        .open(workspace.path(), sessions.path(), &discovered)
        .expect("second open reuses runtime");
    assert_eq!(manager.active_count(), 1);
    assert_eq!(manager.client_count(session_id), Some(2));
    manager.detach(session_id).expect("detach first client");
    manager.detach(session_id).expect("detach second client");
    manager.release(session_id).expect("release session");
}
