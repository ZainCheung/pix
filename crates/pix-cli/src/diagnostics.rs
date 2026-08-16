//! Privacy-scrubbed diagnostic bundle export.
//!
//! The bundle is intended to be safe to share with a maintainer: workspace
//! paths, device public keys, relay channel secrets, relay URLs, and Pi
//! executable paths are replaced with `[redacted]` before the archive is
//! created. The host private key file is never included.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use chrono::Utc;
use pix_core::{ConfigStore, EnvironmentSource, HostConfig, HostEnvironment, PiProbe};
use tempfile::Builder;

use crate::status::HostServiceStatus;

pub fn export_bundle(store: &ConfigStore, destination: PathBuf) -> Result<()> {
    let archive_path = ensure_tar_gz_path(destination);
    let parent = archive_path
        .parent()
        .context("locating diagnostic bundle parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("creating diagnostic bundle directory {}", parent.display()))?;
    if fs::symlink_metadata(&archive_path).is_ok() {
        anyhow::bail!(
            "refusing to overwrite existing diagnostic bundle {}",
            archive_path.display()
        );
    }

    let temporary = Builder::new()
        .prefix("pix-diagnostics-")
        .tempdir()
        .context("creating diagnostic staging directory")?;
    let staging = temporary.path();

    let config = store.load().ok();
    let config_dir = store
        .path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    write_summary(staging, config.as_ref(), &config_dir)?;
    write_redacted_config(staging, config.as_ref())?;
    write_service_status(staging, store.path())?;
    write_pi_status(staging)?;
    write_sanitized_logs(staging, &config_dir)?;

    let mut archive = Builder::new()
        .prefix(".pix-diagnostics-")
        .suffix(".tar.gz")
        .tempfile_in(parent)
        .with_context(|| format!("creating diagnostic archive in {}", parent.display()))?;
    let temporary_archive_path = archive.path().to_path_buf();
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&temporary_archive_path)
        .arg("-C")
        .arg(staging)
        .args([
            "summary.txt",
            "config.redacted.json",
            "service-status.txt",
            "pi-status.txt",
            "logs",
        ])
        .status()
        .context("running tar to create the diagnostic bundle")?;
    if !status.success() {
        anyhow::bail!("tar exited with {status}");
    }
    archive
        .as_file_mut()
        .sync_all()
        .context("syncing diagnostic archive")?;
    archive.persist_noclobber(&archive_path).map_err(|error| {
        anyhow::anyhow!(
            "persisting diagnostic bundle to {}: {}",
            archive_path.display(),
            error.error
        )
    })?;
    println!("Diagnostic bundle written to {}", archive_path.display());
    Ok(())
}

fn ensure_tar_gz_path(destination: PathBuf) -> PathBuf {
    let name = destination.to_string_lossy();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        destination
    } else {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        destination.join(format!("pix-diagnostics-{timestamp}.tar.gz"))
    }
}

fn write_summary(staging: &Path, config: Option<&HostConfig>, config_dir: &Path) -> Result<()> {
    let mut summary = String::new();
    summary.push_str("Pix diagnostic bundle\n");
    let _ = writeln!(summary, "  version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        summary,
        "  os: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(
        summary,
        "  created_at: {}",
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    let _ = writeln!(
        summary,
        "  config: {} (present: {})",
        config_dir
            .join("config.json")
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        config.is_some()
    );
    if let Some(config) = config {
        let _ = writeln!(summary, "  workspaces: {}", config.workspaces.len());
        let _ = writeln!(summary, "  paired_devices: {}", config.devices.len());
        let _ = writeln!(
            summary,
            "  relay_enabled: {}",
            config.preferences.relay_enabled
        );
    }
    write_file(staging, "summary.txt", summary.as_bytes())
}

fn write_redacted_config(staging: &Path, config: Option<&HostConfig>) -> Result<()> {
    let Some(config) = config else {
        write_file(staging, "config.redacted.json", b"{}\n")?;
        return Ok(());
    };
    let mut value = serde_json::to_value(config).context("serializing config for diagnostics")?;
    redact_config(&mut value);
    let bytes = serde_json::to_vec_pretty(&value).context("encoding redacted config")?;
    write_file(
        staging,
        "config.redacted.json",
        &[bytes.as_slice(), b"\n"].concat(),
    )
}

fn write_service_status(staging: &Path, config_path: &Path) -> Result<()> {
    let mut status = String::new();
    match HostServiceStatus::current(config_path) {
        Some(current) => {
            let _ = writeln!(
                status,
                "service: running (pid {}, port {}, started_at {})",
                current.pid, current.port, current.started_at
            );
        }
        None => {
            status.push_str("service: not running (no live status file)\n");
        }
    }
    write_file(staging, "service-status.txt", status.as_bytes())
}

fn write_pi_status(staging: &Path) -> Result<()> {
    let environment = HostEnvironment::resolve_for("pi");
    let mut status = String::new();
    let source = match environment.source() {
        EnvironmentSource::LoginShell { .. } => "login shell",
        EnvironmentSource::Process => "process environment",
    };
    let _ = writeln!(status, "environment: {source}");
    match PiProbe::new(None).with_environment(environment).inspect() {
        Ok(installation) => {
            let _ = writeln!(
                status,
                "pi: {} (supported: {})",
                installation.version, installation.supported
            );
            status.push_str("pi_executable: [redacted]\n");
        }
        Err(_) => status.push_str("pi: unavailable (probe failed)\n"),
    }
    write_file(staging, "pi-status.txt", status.as_bytes())
}

fn write_sanitized_logs(staging: &Path, config_dir: &Path) -> Result<()> {
    let log_path = config_dir.join("logs").join("host.jsonl");
    let log_dir = staging.join("logs");
    fs::create_dir_all(&log_dir).context("creating diagnostic log directory")?;
    match fs::symlink_metadata(&log_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            write_file(
                &log_dir,
                "host.jsonl",
                b"(host log is not a regular file)\n",
            )?;
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_file(&log_dir, "host.jsonl", b"(no host service log)\n")?;
            return Ok(());
        }
        Err(_) => {
            write_file(
                &log_dir,
                "host.jsonl",
                b"(host log could not be inspected)\n",
            )?;
            return Ok(());
        }
    }
    let Ok(contents) = read_regular_file(&log_path) else {
        write_file(&log_dir, "host.jsonl", b"(no host service log)\n")?;
        return Ok(());
    };
    let scrubbed = contents
        .lines()
        .map(sanitize_log_line)
        .collect::<Vec<_>>()
        .join("\n");
    write_file(&log_dir, "host.jsonl", format!("{scrubbed}\n").as_bytes())
}

fn read_regular_file(path: &Path) -> std::io::Result<String> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;

        let mut options = OpenOptions::new();
        options.read(true).custom_flags(libc::O_NOFOLLOW);
        let mut file = options.open(path)?;
        if !file.metadata()?.file_type().is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        Ok(contents)
    }
    #[cfg(not(unix))]
    {
        fs::read_to_string(path)
    }
}

fn sanitize_log_line(line: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return "{\"kind\":\"redacted\",\"body\":\"log entry omitted\"}".to_owned();
    };
    let Some(object) = value.as_object() else {
        return "{\"kind\":\"redacted\",\"body\":\"log entry omitted\"}".to_owned();
    };
    let timestamp = object
        .get("ts")
        .and_then(serde_json::Value::as_str)
        .filter(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
        .unwrap_or("[redacted]");
    let raw_kind = object
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("redacted");
    let kind = match raw_kind {
        "event" | "lifecycle" | "panic" | "host.defaults" | "host.snapshot" | "session.list" => {
            raw_kind
        }
        _ => "redacted",
    };
    let body = match kind {
        "event" => sanitize_event_body(object.get("body")),
        "lifecycle" => sanitize_lifecycle_body(object.get("body")),
        "panic" => serde_json::json!({"message": "panic (details redacted)"}),
        "host.defaults" | "host.snapshot" | "session.list" => {
            sanitize_numeric_body(object.get("body"))
        }
        _ => serde_json::json!("[redacted]"),
    };
    serde_json::json!({"ts": timestamp, "kind": kind, "body": body}).to_string()
}

fn sanitize_event_body(value: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        return serde_json::json!("[redacted]");
    };
    let mut safe = serde_json::Map::new();
    for key in [
        "type",
        "port",
        "path_entries",
        "expires_at",
        "state",
        "count",
    ] {
        if let Some(value) = object.get(key) {
            let allowed = match key {
                "type" => value.as_str().is_some_and(is_allowed_event_type),
                "state" => value.as_str().is_some_and(is_allowed_relay_state),
                _ => value.is_number(),
            };
            if allowed {
                safe.insert(key.to_owned(), value.clone());
            }
        }
    }
    if object.get("type").and_then(serde_json::Value::as_str) == Some("remote_pairing_ready") {
        safe.insert(
            "qr_payload".to_owned(),
            serde_json::Value::String("[redacted]".to_owned()),
        );
        safe.insert(
            "join_code".to_owned(),
            serde_json::Value::String("[redacted]".to_owned()),
        );
    }
    serde_json::Value::Object(safe)
}

fn is_allowed_event_type(value: &str) -> bool {
    matches!(
        value,
        "ready"
            | "environment"
            | "pairing_requested"
            | "connection_established"
            | "connection_closed"
            | "connection_failed"
            | "device_list"
            | "device_revoked"
            | "session_list"
            | "session_released"
            | "relay_configured"
            | "relay_channel"
            | "remote_pairing_ready"
            | "command_error"
    )
}

fn is_allowed_relay_state(value: &str) -> bool {
    matches!(
        value,
        "waiting"
            | "peer_joined"
            | "peer_left"
            | "stopped"
            | "failed_connect"
            | "failed_join"
            | "failed_transport"
            | "failed_localbridge"
    )
}

fn sanitize_lifecycle_body(value: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(text) = value.and_then(serde_json::Value::as_str) else {
        return serde_json::json!("[redacted]");
    };
    match text {
        "serve starting" | "serve stopping (quit command)" | "serve stopping (stdin closed)" => {
            serde_json::Value::String(text.to_owned())
        }
        _ => serde_json::json!("[redacted]"),
    }
}

fn sanitize_numeric_body(value: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        return serde_json::json!("[redacted]");
    };
    let mut safe = serde_json::Map::new();
    for key in [
        "model_present",
        "model_count",
        "thinking_present",
        "validation_ms",
        "workspace_count",
        "response_bytes",
        "enumerate_ms",
        "scan_ms",
        "file_count",
        "session_count",
        "parsed_count",
        "reused_count",
    ] {
        if let Some(value) = object.get(key)
            && value.is_number()
        {
            safe.insert(key.to_owned(), value.clone());
        }
    }
    serde_json::Value::Object(safe)
}

fn redact_config(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *item = serde_json::Value::String("[redacted]".to_owned());
                } else {
                    redact_config(item);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_config(item);
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized == "path"
        || normalized.ends_with("_path")
        || normalized == "name"
        || normalized == "display_name"
        || normalized == "public_key"
        || normalized == "relay_channel"
        || normalized == "relay_url"
        || normalized == "pi_executable"
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("credential")
        || normalized.contains("private_key")
}

fn write_file(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = directory.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("creating diagnostic staging subdirectory")?;
    }
    let mut file =
        fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{ensure_tar_gz_path, export_bundle, redact_config, write_sanitized_logs};

    #[test]
    fn archive_path_is_deterministic_for_explicit_tar_gz() {
        let path = std::path::PathBuf::from("/tmp/pix-bundle.tar.gz");
        assert_eq!(ensure_tar_gz_path(path.clone()), path);
    }

    #[test]
    fn redaction_covers_paths_and_credentials() {
        let mut value = serde_json::json!({
            "version": 1,
            "host": {"id": "host-1", "display_name": "Zain's MacBook"},
            "workspaces": [{"id": "ws-1", "name": "Subuddy", "path": "/Users/zain/private"}],
            "devices": [{
                "id": "device-1",
                "name": "Zain's iPhone",
                "public_key": "AAAA",
                "relay_channel": "BBBB"
            }],
            "preferences": {
                "relay_url": "wss://relay.example.invalid",
                "pi_executable": "/opt/pi/bin/pi"
            }
        });
        redact_config(&mut value);
        assert_eq!(value["host"]["display_name"], "[redacted]");
        assert_eq!(value["workspaces"][0]["path"], "[redacted]");
        assert_eq!(value["workspaces"][0]["name"], "[redacted]");
        assert_eq!(value["devices"][0]["public_key"], "[redacted]");
        assert_eq!(value["devices"][0]["relay_channel"], "[redacted]");
        assert_eq!(value["preferences"]["relay_url"], "[redacted]");
        assert_eq!(value["preferences"]["pi_executable"], "[redacted]");
    }

    #[test]
    fn redaction_covers_unknown_secret_fields() {
        let mut value = serde_json::json!({
            "unknown": {
                "api_token": "TOP-SECRET",
                "private_key": "PRIVATE",
                "safe_counter": 3
            }
        });
        redact_config(&mut value);
        assert_eq!(value["unknown"]["api_token"], "[redacted]");
        assert_eq!(value["unknown"]["private_key"], "[redacted]");
        assert_eq!(value["unknown"]["safe_counter"], 3);
    }

    #[test]
    fn export_refuses_to_overwrite_an_existing_archive() {
        let directory = tempdir().expect("diagnostic directory");
        let destination = directory.path().join("bundle.tar.gz");
        fs::write(&destination, b"sentinel").expect("sentinel archive");
        let store = pix_core::ConfigStore::new(directory.path().join("config.json"));

        let error = export_bundle(&store, destination.clone()).expect_err("existing archive");
        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            fs::read(destination).expect("sentinel remains"),
            b"sentinel"
        );
    }

    #[test]
    fn export_writes_a_private_archive_with_scrubbed_logs() {
        let directory = tempdir().expect("diagnostic directory");
        let config_path = directory.path().join("config/config.json");
        let store = pix_core::ConfigStore::new(&config_path);
        store
            .save(&pix_core::HostConfig::new("Test Host"))
            .expect("config");
        let log_directory = config_path.parent().expect("config parent").join("logs");
        fs::create_dir_all(&log_directory).expect("log directory");
        fs::write(
            log_directory.join("host.jsonl"),
            r#"{"ts":"2026-08-16T00:00:00Z","kind":"event","body":{"type":"environment","pi_executable":"/mnt/private/pi","path_entries":4}}"#,
        )
        .expect("host log");
        let destination = directory.path().join("bundle.tar.gz");

        export_bundle(&store, destination.clone()).expect("export bundle");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&destination)
                .expect("archive metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let output = std::process::Command::new("tar")
            .args([
                "-xOf",
                destination.to_str().expect("destination"),
                "logs/host.jsonl",
            ])
            .output()
            .expect("read archive log");
        assert!(output.status.success());
        let rendered = String::from_utf8(output.stdout).expect("archive log text");
        assert!(rendered.contains("environment"));
        assert!(!rendered.contains("/mnt/private"));
        assert!(!rendered.contains("pi_executable"));
    }

    #[cfg(unix)]
    #[test]
    fn sanitized_logs_skip_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("diagnostic directory");
        let log_directory = directory.path().join("logs");
        let staging = directory.path().join("staging");
        fs::create_dir_all(&log_directory).expect("log directory");
        fs::write(directory.path().join("private.txt"), b"private").expect("private file");
        symlink(
            directory.path().join("private.txt"),
            log_directory.join("host.jsonl"),
        )
        .expect("log symlink");

        write_sanitized_logs(&staging, directory.path()).expect("sanitize logs");
        let sanitized =
            fs::read_to_string(staging.join("logs/host.jsonl")).expect("sanitized log output");
        assert!(!sanitized.contains("private"));
        assert!(sanitized.contains("not a regular file"));
    }

    #[test]
    fn sanitized_logs_drop_unknown_paths_and_payloads() {
        let directory = tempdir().expect("diagnostic directory");
        let log_directory = directory.path().join("logs");
        let staging = directory.path().join("staging");
        fs::create_dir_all(&log_directory).expect("log directory");
        fs::write(
            log_directory.join("host.jsonl"),
            "{\"ts\":\"2026-08-16T00:00:00Z\",\"kind\":\"event\",\"body\":{\"type\":\"environment\",\"pi_executable\":\"/mnt/private/pi\",\"secret\":\"top-secret\",\"path_entries\":4}}\n{\"ts\":\"TOPSECRET\",\"kind\":\"secret/path\",\"body\":{\"type\":\"secret/path\"}}",
        )
        .expect("host log");

        write_sanitized_logs(&staging, directory.path()).expect("sanitize logs");
        let sanitized =
            fs::read_to_string(staging.join("logs/host.jsonl")).expect("sanitized log output");
        assert!(sanitized.contains("environment"));
        assert!(sanitized.contains("path_entries"));
        assert!(!sanitized.contains("/mnt/private"));
        assert!(!sanitized.contains("top-secret"));
        assert!(!sanitized.contains("TOPSECRET"));
        assert!(!sanitized.contains("secret/path"));
        assert!(!sanitized.contains("pi_executable"));
    }
}
