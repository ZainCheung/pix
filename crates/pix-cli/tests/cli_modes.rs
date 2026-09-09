use std::process::{Command, Output};

use chrono::Utc;
use pix_core::config::DeviceRecord;
use pix_core::{ConfigStore, HostConfig};
use tempfile::tempdir;

fn pix(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pix"))
        .args(arguments)
        .output()
        .expect("run pix")
}

fn json_stdout(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "decode Pix stdout as JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn json_stderr(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!(
            "decode Pix stderr as JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn no_arguments_never_waits_when_stdio_is_headless() {
    let output = pix(&[]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: pix [OPTIONS] [COMMAND]"));
    assert!(stdout.contains("workspace"));
    assert!(output.stderr.is_empty());
}

#[test]
fn json_status_is_versioned_and_does_not_create_config() {
    let directory = tempdir().expect("temporary config directory");
    let config = directory.path().join("config.json");
    let output = pix(&[
        "--output",
        "json",
        "--config",
        config.to_str().expect("UTF-8 config path"),
        "status",
    ]);

    assert!(output.status.success());
    let json = json_stdout(&output);
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "status");
    assert_eq!(json["data"]["config_state"], "missing");
    assert!(!config.exists());
}

#[test]
fn workspace_headless_round_trip_is_structured() {
    let directory = tempdir().expect("temporary workspace directory");
    let config = directory.path().join("config.json");
    let workspace = directory.path().join("project");
    std::fs::create_dir(&workspace).expect("create workspace");
    let common = [
        "--output",
        "json",
        "--config",
        config.to_str().expect("UTF-8 config path"),
    ];
    let add = pix(&[
        common[0],
        common[1],
        common[2],
        common[3],
        "workspace",
        "add",
        workspace.to_str().expect("UTF-8 workspace path"),
        "--name",
        "Project",
    ]);
    assert!(add.status.success());
    let added = json_stdout(&add);
    assert_eq!(added["command"], "workspace.add");
    assert_eq!(added["data"]["workspace"]["name"], "Project");

    let list = pix(&[
        common[0],
        common[1],
        common[2],
        common[3],
        "workspace",
        "list",
    ]);
    assert!(list.status.success());
    let listed = json_stdout(&list);
    assert_eq!(listed["command"], "workspace.list");
    assert_eq!(listed["data"]["workspaces"][0]["name"], "Project");
}

#[test]
fn missing_headless_group_action_is_a_usage_error() {
    let directory = tempdir().expect("temporary config directory");
    let config = directory.path().join("config.json");
    let output = pix(&[
        "--output",
        "json",
        "--config",
        config.to_str().expect("UTF-8 config path"),
        "workspace",
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let json = json_stderr(&output);
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "usage");
}

#[test]
fn device_json_never_exposes_trust_material() {
    let directory = tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.json");
    let store = ConfigStore::new(&config_path);
    let mut config = HostConfig::new("Test Mac");
    config.devices.push(DeviceRecord {
        id: "device-fingerprint".to_owned(),
        name: "Test iPhone".to_owned(),
        public_key: "private-to-the-host-output".to_owned(),
        relay_channel: "relay-channel-secret".to_owned(),
        paired_at: Utc::now(),
        unknown: serde_json::Map::new(),
    });
    store.save(&config).expect("save config");

    let output = pix(&[
        "--output",
        "json",
        "--config",
        config_path.to_str().expect("UTF-8 config path"),
        "device",
        "list",
    ]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("private-to-the-host-output"));
    assert!(!stdout.contains("relay-channel-secret"));
    let json = json_stdout(&output);
    assert_eq!(json["data"]["devices"][0]["id"], "device-fingerprint");
    assert_eq!(json["data"]["devices"][0]["name"], "Test iPhone");
}

#[test]
fn human_device_inventory_remains_compatible_with_the_macos_client() {
    let directory = tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.json");
    let store = ConfigStore::new(&config_path);
    let mut config = HostConfig::new("Test Mac");
    config.devices.push(DeviceRecord {
        id: "abcdef".to_owned(),
        name: "Test iPhone".to_owned(),
        public_key: "not-printed".to_owned(),
        relay_channel: "not-printed-either".to_owned(),
        paired_at: Utc::now(),
        unknown: serde_json::Map::new(),
    });
    store.save(&config).expect("save config");

    let output = pix(&[
        "--config",
        config_path.to_str().expect("UTF-8 config path"),
        "device",
        "list",
    ]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("abcdef  Test iPhone\n  paired "));
    assert!(!stdout.contains("not-printed"));
}

#[cfg(unix)]
#[test]
fn pi_show_resolves_metadata_without_running_pi() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.json");
    let counter = directory.path().join("pi-invocations");
    let executable = directory.path().join("pi");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version|--help) printf '%s\\n' \"$1\" >> '{}' ;;\nesac\n",
            counter.display()
        ),
    )
    .expect("write fake Pi");
    let mut permissions = std::fs::metadata(&executable)
        .expect("fake Pi metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable, permissions).expect("make fake Pi executable");

    let store = ConfigStore::new(&config_path);
    let mut config = HostConfig::new("Test Mac");
    config.preferences.pi_executable = Some(executable.clone());
    store.save(&config).expect("save config");

    let output = pix(&[
        "--output",
        "json",
        "--config",
        config_path.to_str().expect("UTF-8 config path"),
        "pi",
        "show",
    ]);

    assert!(output.status.success(), "pi show: {output:?}");
    let json = json_stdout(&output);
    assert_eq!(json["command"], "pi.show");
    assert_eq!(
        json["data"]["executable"],
        executable.to_string_lossy().as_ref()
    );
    assert!(
        !counter.exists(),
        "pi show invoked --version or --help unexpectedly"
    );
}

#[cfg(unix)]
#[test]
fn status_without_a_running_host_uses_one_compatibility_probe() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.json");
    let counter = directory.path().join("pi-invocations");
    let executable = directory.path().join("pi");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) printf '0.84.1\\n'; printf '%s\\n' version >> '{}' ;;\n  --help) printf '%s\\n' '--mode <mode> --approve --session <path|id> --session-id <id>'; printf '%s\\n' help >> '{}' ;;\nesac\n",
            counter.display(),
            counter.display()
        ),
    )
    .expect("write fake Pi");
    let mut permissions = std::fs::metadata(&executable)
        .expect("fake Pi metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable, permissions).expect("make fake Pi executable");

    let store = ConfigStore::new(&config_path);
    let mut config = HostConfig::new("Test Mac");
    config.preferences.pi_executable = Some(executable);
    store.save(&config).expect("save config");

    let output = pix(&[
        "--output",
        "json",
        "--config",
        config_path.to_str().expect("UTF-8 config path"),
        "status",
    ]);

    assert!(output.status.success(), "status: {output:?}");
    let json = json_stdout(&output);
    assert_eq!(json["data"]["pi"]["compatibility"], "compatible");
    let calls = std::fs::read_to_string(counter).expect("read Pi invocation counter");
    assert_eq!(calls.lines().count(), 2, "status should probe Pi once");
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn status_reuses_the_running_hosts_cached_pi_report() {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    let directory = tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.json");
    let counter = directory.path().join("pi-invocations");
    let executable = directory.path().join("pi");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) printf '0.84.1\\n'; printf '%s\\n' version >> '{}' ;;\n  --help) printf '%s\\n' '--mode <mode> --approve --session <path|id> --session-id <id>'; printf '%s\\n' help >> '{}' ;;\nesac\n",
            counter.display(),
            counter.display()
        ),
    )
    .expect("write fake Pi");
    let mut permissions = std::fs::metadata(&executable)
        .expect("fake Pi metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable, permissions).expect("make fake Pi executable");

    let store = ConfigStore::new(&config_path);
    let mut config = HostConfig::new("Test Mac");
    config.preferences.pi_executable = Some(executable);
    store.save(&config).expect("save config");

    let mut host = Command::new(env!("CARGO_BIN_EXE_pix"))
        .arg("--config")
        .arg(&config_path)
        .args(["serve", "--service"])
        .env("PIX_DISABLE_KEYCHAIN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn host service");

    let status_path = directory.path().join("run/host-service.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut status = None;
    while Instant::now() < deadline {
        if status_path.exists() {
            let output = pix(&[
                "--output",
                "json",
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
                "status",
            ]);
            if output.status.success() {
                let json = json_stdout(&output);
                if json["data"]["pi"]["compatibility"] == "compatible" {
                    status = Some(json);
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        status.is_some(),
        "running host did not expose its Pi report"
    );

    let before = std::fs::read_to_string(&counter).expect("read Pi invocation counter");
    assert_eq!(
        before.lines().count(),
        2,
        "Host should probe Pi exactly once"
    );

    let output = pix(&[
        "--output",
        "json",
        "--config",
        config_path.to_str().expect("UTF-8 config path"),
        "status",
    ]);
    assert!(output.status.success(), "status: {output:?}");
    let after = std::fs::read_to_string(&counter).expect("read Pi invocation counter");
    assert_eq!(after, before, "status launched Pi despite cached report");

    let mut control = UnixStream::connect(directory.path().join("run/host-service.sock"))
        .expect("connect host control socket");
    control.write_all(b"quit\n").expect("send host quit");
    control
        .shutdown(std::net::Shutdown::Write)
        .expect("shutdown control");
    let mut response = String::new();
    control
        .read_to_string(&mut response)
        .expect("read host quit response");
    assert_eq!(response.trim(), "ok");

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if host.try_wait().expect("poll host").is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = host.kill();
    let output = host.wait_with_output().expect("wait host");
    panic!("host did not stop after quit: {:?}", output.stderr);
}
