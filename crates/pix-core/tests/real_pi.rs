use std::time::Duration;

use pix_core::pi_bridge;
use pix_core::pi_rpc::PiCommand;
use pix_core::{
    HostEnvironment, PiProbe, PiRuntime, PiRuntimeOptions, SessionId, SessionLaunch,
    SessionSnapshot,
};
use tempfile::tempdir;

#[test]
#[ignore = "requires the locally installed, supported Pi binary"]
fn installed_pi_accepts_adapter_get_state() {
    // Mirror production: discover and run Pi inside the same resolved
    // environment, so version-manager installs (mise, nvm, bun) also work.
    let environment = HostEnvironment::resolve_for("pi");
    let installation = PiProbe::new(None)
        .with_environment(environment.clone())
        .inspect()
        .expect("supported Pi installation");
    assert!(
        installation.supported,
        "installed Pi must be in the verified range"
    );
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
        extra_arguments: vec![
            "--session-dir".to_owned(),
            sessions.path().display().to_string(),
            "--offline".to_owned(),
            "--no-extensions".to_owned(),
            "--no-skills".to_owned(),
            "--no-prompt-templates".to_owned(),
        ],
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
#[ignore = "requires the locally installed, supported Pi binary"]
fn installed_pi_serves_commands_thinking_levels_and_stats() {
    let environment = HostEnvironment::resolve_for("pi");
    let installation = PiProbe::new(None)
        .with_environment(environment.clone())
        .inspect()
        .expect("supported Pi installation");
    assert!(installation.supported);
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
        extra_arguments: vec![
            "--session-dir".to_owned(),
            sessions.path().display().to_string(),
            "--offline".to_owned(),
        ],
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
