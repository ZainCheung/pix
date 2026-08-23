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
