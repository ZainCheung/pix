//! Versioned request/response client for the private local Host control socket.
//!
//! Lifecycle notifications remain on the event socket for native UI clients.
//! Headless CLI commands use this channel so success means the requested
//! operation completed, not merely that it entered the service queue.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;

const CONTROL_SCHEMA_VERSION: u32 = 1;

pub(crate) fn verify_control_compatibility(store: &ConfigStore) -> Result<()> {
    request_event(
        store,
        "capabilities",
        "capabilities",
        Duration::from_secs(2),
    )?;
    Ok(())
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
    use super::rpc_request_for;

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
