//! Versioned request/response client for the private local Host control socket.
//!
//! Lifecycle notifications remain on the event socket for native UI clients.
//! Headless CLI commands use this channel so success means the requested
//! operation completed, not merely that it entered the service queue.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use pix_core::{ConfigStore, PiCompatibilityReport};

const CONTROL_SCHEMA_VERSION: u32 = 1;

/// Returns the Pix version reported by a running host process.
///
/// The version is intentionally read through the private local control
/// channel rather than the supervision status file. This lets a newly started
/// CLI detect an old long-lived host after Sparkle or another installer has
/// replaced the executable on disk.
pub(crate) fn running_host_version(store: &ConfigStore) -> Result<String> {
    let data = request_event(
        store,
        "capabilities",
        "capabilities",
        Duration::from_secs(2),
    )?;
    pix_version_from_capabilities(&data).map(ToOwned::to_owned)
}

/// Reads the compatibility snapshot captured by a running Host lifecycle.
///
/// `Ok(None)` is deliberately distinct from an RPC failure: a pre-versioned
/// Host (or a Host that predates the report field) is still usable, but the
/// caller must fall back to one local compatibility probe for an accurate
/// status.  The report itself never crosses the public phone protocol.
pub(crate) fn running_host_pi_report(store: &ConfigStore) -> Result<Option<PiCompatibilityReport>> {
    let data = request_event(
        store,
        "capabilities",
        "capabilities",
        // A Host binds its local sockets before the compatibility preflight
        // completes. Keep this request open long enough for that one probe so
        // a concurrent `pix status` waits for the authoritative snapshot
        // instead of starting a duplicate Pi process. Older Hosts answer
        // immediately without the additive field and still take the fallback
        // path below.
        Duration::from_secs(30),
    )?;
    // A cached report is authoritative only for the matching Host binary.
    // Version-mismatched processes are handled by the existing compatibility
    // fallback so a stale snapshot cannot be mistaken for this CLI's Host.
    if pix_version_from_capabilities(&data).ok() != Some(env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    pi_report_from_capabilities(&data)
}

fn pi_report_from_capabilities(data: &serde_json::Value) -> Result<Option<PiCompatibilityReport>> {
    let Some(report) = data.get("pi_compatibility").or_else(|| data.get("pi")) else {
        return Ok(None);
    };
    serde_json::from_value(report.clone())
        .context("Pix host returned an invalid Pi compatibility report")
        .map(Some)
}

pub(crate) fn verify_control_compatibility(store: &ConfigStore) -> Result<()> {
    let running_version = running_host_version(store)?;
    let current_version = env!("CARGO_PKG_VERSION");
    if running_version != current_version {
        bail!("running Pix host is Pix {running_version}, but this CLI is Pix {current_version}");
    }
    Ok(())
}

/// Returns whether a running Host is definitively older than this CLI's
/// versioned control surface. Transient connection failures (including a
/// Host that has bound its socket but is still in startup preflight) are not
/// evidence of an upgrade requirement and must not trigger a service restart.
pub(crate) fn control_upgrade_required(store: &ConfigStore) -> bool {
    match running_host_version(store) {
        Ok(version) => version != env!("CARGO_PKG_VERSION"),
        Err(error) => {
            let message = error.to_string();
            message.contains("predates versioned control responses")
                || message.contains("missing pix_version")
        }
    }
}

fn pix_version_from_capabilities(data: &serde_json::Value) -> Result<&str> {
    data.get("pix_version")
        .and_then(serde_json::Value::as_str)
        .filter(|version| !version.trim().is_empty())
        .context("Pix host capabilities are missing pix_version")
}

pub(crate) fn request_event(
    store: &ConfigStore,
    legacy_command: &str,
    expected_type: &str,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let (command, args) = rpc_request_for(legacy_command)?;
    let request_id = uuid::Uuid::new_v4();
    let request = serde_json::json!({
        "schema_version": CONTROL_SCHEMA_VERSION,
        "request_id": request_id,
        "command": command,
        "args": args,
    });
    let response = crate::status::request_control_rpc(store.path(), &request, timeout)?;
    if response
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(u64::from(CONTROL_SCHEMA_VERSION))
    {
        bail!("Pix host returned an unsupported control response version");
    }
    let response_id = response
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .context("Pix host response is missing a request ID")?;
    if response_id != request_id.to_string() {
        bail!("Pix host returned a mismatched control response");
    }
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let code = response
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("command_failed");
        let message = response
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Pix host command failed");
        bail!("Pix host {code}: {message}");
    }
    let data = response
        .get("data")
        .cloned()
        .context("Pix host response is missing data")?;
    if data.get("type").and_then(serde_json::Value::as_str) != Some(expected_type) {
        bail!("Pix host returned an unexpected response type");
    }
    Ok(data)
}

fn rpc_request_for(command: &str) -> Result<(&'static str, serde_json::Value)> {
    let mut words = command.split_whitespace();
    let verb = words.next().context("host control command is empty")?;
    let result = match verb {
        "capabilities" => ("capabilities", serde_json::json!({})),
        "pending-list" => ("pairing.pending", serde_json::json!({})),
        "pair-remote" => ("pairing.remote_offer", serde_json::json!({})),
        "sessions" => ("session.list", serde_json::json!({})),
        "refresh" => ("config.refresh", serde_json::json!({})),
        "approve" => (
            "pairing.approve",
            serde_json::json!({"request_id": required_token(&mut words, "request ID")?}),
        ),
        "reject" => (
            "pairing.reject",
            serde_json::json!({"request_id": required_token(&mut words, "request ID")?}),
        ),
        "revoke" => (
            "device.revoke",
            serde_json::json!({"device_id": required_token(&mut words, "device ID")?}),
        ),
        "release" => (
            "session.release",
            serde_json::json!({"session_id": required_token(&mut words, "session ID")?}),
        ),
        _ => bail!("unsupported versioned host control command: {verb}"),
    };
    if words.next().is_some() {
        bail!("host control command has unexpected arguments");
    }
    Ok(result)
}

fn required_token<'a>(words: &mut impl Iterator<Item = &'a str>, label: &str) -> Result<&'a str> {
    words.next().with_context(|| format!("missing {label}"))
}

#[cfg(test)]
mod tests {
    use super::{pi_report_from_capabilities, pix_version_from_capabilities, rpc_request_for};

    #[test]
    fn reads_host_version_from_capabilities() {
        let data = serde_json::json!({
            "type": "capabilities",
            "control_schema_version": 1,
            "pix_version": "0.6.0",
        });
        assert_eq!(
            pix_version_from_capabilities(&data).expect("host version"),
            "0.6.0"
        );
    }

    #[test]
    fn rejects_missing_host_version() {
        let data = serde_json::json!({
            "type": "capabilities",
            "control_schema_version": 1,
        });
        assert!(pix_version_from_capabilities(&data).is_err());
    }

    #[test]
    fn accepts_capabilities_without_a_pi_report_for_old_hosts() {
        let data = serde_json::json!({
            "type": "capabilities",
            "control_schema_version": 1,
            "pix_version": "0.6.0",
        });
        assert!(
            pi_report_from_capabilities(&data)
                .expect("parse capabilities")
                .is_none()
        );
    }

    #[test]
    fn parses_the_host_pi_compatibility_snapshot() {
        let data = serde_json::json!({
            "type": "capabilities",
            "pi_compatibility": {
                "executable": "/opt/homebrew/bin/pi",
                "version": "0.85.1",
                "compatibility": "compatible"
            }
        });
        let report = pi_report_from_capabilities(&data)
            .expect("parse report")
            .expect("report present");
        assert_eq!(report.version.as_deref(), Some("0.85.1"));
        assert_eq!(
            report.compatibility,
            pix_core::PiCompatibilityStatus::Compatible
        );
    }

    #[test]
    fn maps_cli_operations_to_typed_control_requests() {
        let (command, args) = rpc_request_for("revoke device-1").expect("map command");
        assert_eq!(command, "device.revoke");
        assert_eq!(args["device_id"], "device-1");
    }

    #[test]
    fn rejects_extra_control_arguments() {
        assert!(rpc_request_for("sessions unexpected").is_err());
    }
}
