use std::path::{Path, PathBuf};
use std::time::Duration;

use pix_core::pi_bridge;
use pix_core::pi_rpc::PiCommand;
use pix_core::{
    HostEnvironment, PiProbe, PiRuntime, PiRuntimeOptions, SessionId, SessionLaunch,
    SessionSnapshot,
};
use tempfile::{NamedTempFile, tempdir};

const PI_CONTEXT_GUARD_SOURCE: &str = include_str!("../../pix-cli/resources/pi-context-guard.mjs");

fn test_environment(home: &Path) -> HostEnvironment {
    HostEnvironment::resolve_for("pi")
        .with_override("HOME", home.as_os_str().to_owned())
        .with_override(
            "PI_CODING_AGENT_DIR",
            home.join(".pi/agent").as_os_str().to_owned(),
        )
}

fn test_installation(environment: &HostEnvironment) -> pix_core::PiInstallation {
    let explicit = std::env::var_os("PIX_REAL_PI_EXECUTABLE").map(PathBuf::from);
    PiProbe::new(explicit)
        .with_environment(environment.clone())
        .inspect_compatibility()
        .expect("compatible Pi installation")
}

fn context_guard(home: &Path) -> NamedTempFile {
    let mut file = tempfile::Builder::new()
        .prefix("pix-pi-context-guard-")
        .suffix(".mjs")
        .tempfile_in(home)
        .expect("context guard fixture");
    std::io::Write::write_all(&mut file, PI_CONTEXT_GUARD_SOURCE.as_bytes())
        .expect("write context guard fixture");
    file
}

fn context_guard_arguments(guard: &NamedTempFile) -> Vec<String> {
    vec!["--extension".to_owned(), guard.path().display().to_string()]
}

#[test]
#[ignore = "requires the locally installed, compatible Pi binary"]
fn installed_pi_accepts_adapter_get_state() {
    // Mirror production: discover and run Pi inside the same resolved
    // environment, so version-manager installs (mise, nvm, bun) also work.
    let home = tempdir().expect("temporary Pi home");
    let environment = test_environment(home.path());
    let installation = test_installation(&environment);
    let guard = context_guard(home.path());
    let workspace = tempdir().expect("temporary workspace");
    let sessions = tempdir().expect("temporary Pi session directory");
    let locks = tempdir().expect("temporary Pix lock directory");
    let runtime = PiRuntime::start(&PiRuntimeOptions {
        executable: installation.executable,
        workspace: workspace.path().to_path_buf(),
        lock_directory: locks.path().to_path_buf(),
        launch: SessionLaunch::Create {
            id: SessionId::new(),
            name: Some("Pix compatibility probe".to_owned()),
        },
        extra_arguments: {
            let mut arguments = vec![
                "--session-dir".to_owned(),
                sessions.path().display().to_string(),
                "--offline".to_owned(),
                "--no-skills".to_owned(),
                "--no-prompt-templates".to_owned(),
            ];
            arguments.extend(context_guard_arguments(&guard));
            arguments
        },
        environment,
    })
    .expect("start installed Pi");

    let response = runtime
        .rpc()
        .request(&PiCommand::GetState, Duration::from_secs(10))
        .expect("Pi get_state response");
    let data = response.data.expect("Pi state data");
    assert!(data["sessionId"].is_string());
    assert_eq!(data["isStreaming"], false);
    let snapshot = SessionSnapshot::read(runtime.rpc(), Duration::from_secs(10))
        .expect("authoritative Pi snapshot");
    assert!(!snapshot.is_streaming);
    assert!(snapshot.messages.is_empty());
    runtime.stop().expect("stop installed Pi");
}

#[test]
#[ignore = "requires the locally installed, compatible Pi binary"]
fn installed_pi_serves_commands_thinking_levels_and_stats() {
    let home = tempdir().expect("temporary Pi home");
    let environment = test_environment(home.path());
    let installation = test_installation(&environment);
    let guard = context_guard(home.path());
    let workspace = tempdir().expect("temporary workspace");
    let sessions = tempdir().expect("temporary Pi session directory");
    let locks = tempdir().expect("temporary Pix lock directory");
    let runtime = PiRuntime::start(&PiRuntimeOptions {
        executable: installation.executable,
        workspace: workspace.path().to_path_buf(),
        lock_directory: locks.path().to_path_buf(),
        launch: SessionLaunch::Create {
            id: SessionId::new(),
            name: Some("Pix capability probe".to_owned()),
        },
        extra_arguments: {
            let mut arguments = vec![
                "--session-dir".to_owned(),
                sessions.path().display().to_string(),
                "--offline".to_owned(),
            ];
            arguments.extend(context_guard_arguments(&guard));
            arguments
        },
        environment,
    })
    .expect("start installed Pi");

    // get_commands must answer with the wire shape the bridge expects. With a
    // default user installation this may legitimately be empty; the mapping
    // must still decode.
    let commands_response = runtime
        .rpc()
        .request(&PiCommand::GetCommands, Duration::from_secs(10))
        .expect("Pi get_commands response");
    let commands = pi_bridge::commands(&commands_response).expect("mapped commands");
    for command in &commands {
        assert!(!command.name.is_empty());
    }

    let levels_response = runtime
        .rpc()
        .request(
            &PiCommand::GetAvailableThinkingLevels,
            Duration::from_secs(10),
        )
        .expect("Pi thinking levels response");
    let levels = pi_bridge::thinking_levels(&levels_response).expect("mapped levels");
    assert!(!levels.is_empty(), "Pi reports the current model's levels");

    let stats_response = runtime
        .rpc()
        .request(&PiCommand::GetSessionStats, Duration::from_secs(10))
        .expect("Pi session stats response");
    let usage = pi_bridge::usage(&stats_response).expect("mapped usage");
    assert_eq!(usage.tokens_total, 0);
    assert!((usage.cost - 0.0_f64).abs() < f64::EPSILON);

    runtime.stop().expect("stop installed Pi");
}

#[test]
#[ignore = "requires the locally installed, compatible Pi binary"]
fn installed_pi_resumes_existing_session_with_context_guard() {
    let home = tempdir().expect("temporary Pi home");
    let environment = test_environment(home.path());
    let installation = test_installation(&environment);
    let guard = context_guard(home.path());
    let workspace = tempdir().expect("temporary workspace");
    let sessions = tempdir().expect("temporary Pi session directory");
    let locks = tempdir().expect("temporary Pix lock directory");
    let session_id = SessionId::new();
    let mut extra_arguments = vec![
        "--session-dir".to_owned(),
        sessions.path().display().to_string(),
        "--offline".to_owned(),
    ];
    extra_arguments.extend(context_guard_arguments(&guard));

    let runtime = PiRuntime::start(&PiRuntimeOptions {
        executable: installation.executable.clone(),
        workspace: workspace.path().to_path_buf(),
        lock_directory: locks.path().to_path_buf(),
        launch: SessionLaunch::Create {
            id: session_id,
            name: Some("Pix resume compatibility probe".to_owned()),
        },
        extra_arguments: extra_arguments.clone(),
        environment: environment.clone(),
    })
    .expect("start installed Pi");
    let state = runtime
        .rpc()
        .request(&PiCommand::GetState, Duration::from_secs(10))
        .expect("Pi get_state response")
        .data
        .expect("Pi state data");
    let session_file = state["sessionFile"]
        .as_str()
        .expect("Pi reports the native session file")
        .to_owned();
    SessionSnapshot::read(runtime.rpc(), Duration::from_secs(10)).expect("initial snapshot");
    runtime.stop().expect("stop initial Pi");

    let resumed = PiRuntime::start(&PiRuntimeOptions {
        executable: installation.executable,
        workspace: workspace.path().to_path_buf(),
        lock_directory: locks.path().to_path_buf(),
        launch: SessionLaunch::Existing {
            id: SessionId::new(),
            reference: session_file,
        },
        extra_arguments,
        environment,
    })
    .expect("resume installed Pi session");
    let snapshot =
        SessionSnapshot::read(resumed.rpc(), Duration::from_secs(10)).expect("resumed Pi snapshot");
    assert!(!snapshot.session_id.is_empty());
    assert!(!snapshot.thinking_level.is_empty());
    resumed.stop().expect("stop resumed Pi");
}
