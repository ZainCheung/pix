//! Device pairing commands: offers, pending approvals, approve/reject by
//! request ID or confirmation code, revocation, and the interactive device
//! menu.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;
use serde::{Deserialize, Serialize};

use crate::output::CommandOutput;
use crate::setup_ui::{MenuItem, MenuResult, SetupUi, UiTone};
use crate::status::HostServiceControl;

pub(crate) fn approve_pairing(
    service: &pix_core::HostServiceHandle,
    request: Option<&str>,
    local_request_id: Option<uuid::Uuid>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) -> Result<()> {
    let request = request.context("approve requires request ID")?;
    let request_id = uuid::Uuid::parse_str(request).context("invalid request ID")?;
    service
        .approve(request_id)
        .context("approving pairing request")?;
    emit_event(
        &ServeEvent::PairingRequestHandled {
            request_id,
            action: "approved",
            local_request_id,
        },
        output,
        log,
        control,
    );
    emit_devices(service, output, log, control);
    Ok(())
}

pub(crate) fn reject_pairing(
    service: &pix_core::HostServiceHandle,
    request: Option<&str>,
    local_request_id: Option<uuid::Uuid>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) -> Result<()> {
    let request = request.context("reject requires request ID")?;
    let request_id = uuid::Uuid::parse_str(request).context("invalid request ID")?;
    service
        .reject(request_id)
        .context("rejecting pairing request")?;
    emit_event(
        &ServeEvent::PairingRequestHandled {
            request_id,
            action: "rejected",
            local_request_id,
        },
        output,
        log,
        control,
    );
    Ok(())
}

pub(crate) fn revoke_device(
    service: &pix_core::HostServiceHandle,
    device_id: Option<&str>,
    local_request_id: Option<uuid::Uuid>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) -> Result<pix_core::DeviceRevocation> {
    let device_id = device_id.context("revoke requires device ID")?;
    let revoked = service
        .revoke_device(device_id)
        .context("revoking paired device")?;
    emit_event(
        &ServeEvent::DeviceRevoked {
            device_id: revoked.device.id.clone(),
            device_name: revoked.device.name.clone(),
            local_request_id,
        },
        output,
        log,
        control,
    );
    emit_devices(service, output, log, control);
    Ok(revoked)
}

pub(crate) fn emit_devices(
    service: &pix_core::HostServiceHandle,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) {
    let devices = match service.paired_devices() {
        Ok(devices) => devices
            .into_iter()
            .map(|device| DeviceEvent {
                id: device.id,
                name: device.name,
                paired_at: device.paired_at.to_rfc3339(),
            })
            .collect(),
        Err(error) => {
            emit_command_error(&anyhow::Error::new(error), output, log, control);
            return;
        }
    };
    emit_event(&ServeEvent::DeviceList { devices }, output, log, control);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingPairing {
    id: uuid::Uuid,
    device_name: String,
    confirmation_code: String,
    expires_at: u64,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn device(
    store: &ConfigStore,
    command: Option<DeviceCommand>,
    output: CommandOutput,
    interactive: bool,
) -> Result<()> {
    let Some(command) = command else {
        if !interactive {
            return Err(usage_error(
                "a device command is required outside an interactive terminal",
            ));
        }
        return device_menu(store, output);
    };
    match command {
        DeviceCommand::Pair {
            remote: force_remote,
        } => {
            let config = store
                .load_or_create(default_host_name())
                .context("loading Pix configuration")?;
            let remote = force_remote || config.preferences.active_relay_url().is_some();
            if force_remote && config.preferences.active_relay_url().is_none() {
                bail!("remote pairing requires an enabled relay; run `pix relay set <url>`");
            }
            if !interactive {
                return headless_pair_offer(store, remote, output);
            }
            run_setup_pairing(
                store,
                SetupPairingOptions {
                    remote,
                    yes: false,
                    interactive: true,
                    ui: SetupUi::new(true, false),
                    keep_service: true,
                },
            )
        }
        DeviceCommand::List => {
            let config = store.load().context("loading Pix configuration")?;
            if output.is_json() {
                let devices = config
                    .devices
                    .iter()
                    .map(|device| {
                        serde_json::json!({
                            "id": device.id,
                            "name": device.name,
                            "paired_at": device.paired_at,
                        })
                    })
                    .collect::<Vec<_>>();
                return output.success("device.list", &serde_json::json!({"devices": devices}));
            }
            if config.devices.is_empty() {
                println!("No paired devices.");
                return Ok(());
            }
            for device in config.devices {
                println!("{}  {}", device.id, terminal_label(&device.name));
                println!("  paired {}", device.paired_at.to_rfc3339());
            }
            Ok(())
        }
        DeviceCommand::Pending => {
            let requests = pending_pairings(store)?;
            if output.is_json() {
                return output
                    .success("device.pending", &serde_json::json!({"requests": requests}));
            }
            if requests.is_empty() {
                println!("No pairing requests are waiting for approval.");
                return Ok(());
            }
            for request in requests {
                println!("{}  {}", request.id, terminal_label(&request.device_name));
                println!(
                    "  code {}  expires_at {}",
                    format_confirmation_code(&request.confirmation_code),
                    request.expires_at
                );
            }
            Ok(())
        }
        DeviceCommand::Approve { request, code } => handle_pairing_request(
            store,
            request,
            code.as_deref(),
            "approve",
            output,
            interactive,
        ),
        DeviceCommand::Reject { request, code } => handle_pairing_request(
            store,
            request,
            code.as_deref(),
            "reject",
            output,
            interactive,
        ),
        DeviceCommand::Revoke { id } => {
            let config = store.load().context("loading Pix configuration")?;
            let confirm = interactive && id.is_none();
            let id = select_device_id(&config, id, interactive)?;
            let index = config
                .devices
                .iter()
                .position(|device| device.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown device: {id}"))?;
            let removed = config.devices[index].clone();
            if confirm {
                let ui = SetupUi::new(true, false);
                let choices = vec!["Revoke device".to_owned(), "Cancel".to_owned()];
                if ui.select(
                    &format!(
                        "Revoke {} and close its connections?",
                        terminal_label(&removed.name)
                    ),
                    &choices,
                    1,
                )? != 0
                {
                    return Ok(());
                }
            }
            let mut service_cleanup = None;
            if host_service_control_live(store)? {
                let event = service_client::request_event(
                    store,
                    &format!("revoke {id}"),
                    "device_revoked",
                    Duration::from_secs(5),
                )?;
                if event.get("device_id").and_then(serde_json::Value::as_str) != Some(id.as_str()) {
                    bail!("Pix host returned a mismatched device revocation event");
                }
                service_cleanup = Some(serde_json::json!({
                    "closed_connections": event.get("closed_connections").cloned().unwrap_or(serde_json::json!(0)),
                    "connection_cleanup_failed": event.get("connection_cleanup_failed").cloned().unwrap_or(serde_json::json!(false)),
                }));
            } else {
                let transaction = store.transaction()?;
                if host_service_control_live(store)? {
                    drop(transaction);
                    let event = service_client::request_event(
                        store,
                        &format!("revoke {id}"),
                        "device_revoked",
                        Duration::from_secs(5),
                    )?;
                    if event.get("device_id").and_then(serde_json::Value::as_str)
                        != Some(id.as_str())
                    {
                        bail!("Pix host returned a mismatched device revocation response");
                    }
                    service_cleanup = Some(serde_json::json!({
                        "closed_connections": event.get("closed_connections").cloned().unwrap_or(serde_json::json!(0)),
                        "connection_cleanup_failed": event.get("connection_cleanup_failed").cloned().unwrap_or(serde_json::json!(false)),
                    }));
                } else {
                    let mut current = transaction
                        .load()
                        .context("loading current Pix configuration")?;
                    let index = current
                        .devices
                        .iter()
                        .position(|device| device.id == id)
                        .ok_or_else(|| anyhow::anyhow!("unknown device: {id}"))?;
                    current.devices.remove(index);
                    transaction
                        .save(&current)
                        .context("saving Pix configuration")?;
                }
            }
            if output.is_json() {
                return output.success(
                    "device.revoke",
                    &serde_json::json!({
                        "device": {
                            "id": removed.id,
                            "name": removed.name,
                            "paired_at": removed.paired_at,
                        },
                        "service_cleanup": service_cleanup,
                    }),
                );
            }
            println!("Revoked {} ({})", terminal_label(&removed.name), removed.id);
            if service_cleanup.as_ref().is_some_and(|cleanup| {
                cleanup["connection_cleanup_failed"] == serde_json::json!(true)
            }) {
                println!("  Device trust is revoked; a socket cleanup warning was recorded.");
            }
            Ok(())
        }
    }
}

pub(crate) fn pending_pairings(store: &ConfigStore) -> Result<Vec<PendingPairing>> {
    let event = service_client::request_event(
        store,
        "pending-list",
        "pairing_request_list",
        Duration::from_secs(5),
    )?;
    serde_json::from_value(
        event
            .get("requests")
            .cloned()
            .context("Pix host omitted pending pairing requests")?,
    )
    .context("decoding pending pairing requests")
}

pub(crate) fn headless_pair_offer(
    store: &ConfigStore,
    remote: bool,
    output: CommandOutput,
) -> Result<()> {
    service::ensure_running(store)?;
    if !remote {
        let data = serde_json::json!({
            "transport": "lan",
            "state": "waiting_for_device",
            "next": "Run `pix --output json device pending`, then approve by request ID or code.",
        });
        if output.is_json() {
            return output.success("device.pair", &data);
        }
        println!("Pix is waiting for a device on the local network.");
        println!("Run `pix device pending` to review the confirmation code.");
        return Ok(());
    }

    let event = service_client::request_event(
        store,
        "pair-remote",
        "remote_pairing_ready",
        Duration::from_secs(10),
    )?;
    let data = serde_json::json!({
        "transport": "relay",
        "state": "waiting_for_device",
        "qr_payload": event.get("qr_payload").cloned().unwrap_or(serde_json::Value::Null),
        "join_code": event.get("join_code").cloned().unwrap_or(serde_json::Value::Null),
        "expires_at": event.get("expires_at").cloned().unwrap_or(serde_json::Value::Null),
        "next": "Present the offer to the user, then run `pix --output json device pending` and approve the matching code.",
    });
    if output.is_json() {
        return output.success("device.pair", &data);
    }
    println!("Remote pairing offer ready.");
    if let Some(code) = event.get("join_code").and_then(serde_json::Value::as_str) {
        println!("  code: {code}");
    }
    println!("  The encoded pairing secret is available only with `--output json`.");
    println!("Run `pix device pending` to review the confirmation code.");
    Ok(())
}

pub(crate) fn handle_pairing_request(
    store: &ConfigStore,
    request_id: Option<uuid::Uuid>,
    code: Option<&str>,
    action: &'static str,
    output: CommandOutput,
    interactive: bool,
) -> Result<()> {
    let request = if request_id.is_some() {
        None
    } else {
        let requests = pending_pairings(store)?;
        Some(select_pending_pairing(&requests, None, code, interactive)?)
    };
    let request_id = request_id
        .or_else(|| request.as_ref().map(|request| request.id))
        .context("pairing request ID is required")?;
    let event = service_client::request_event(
        store,
        &format!("{action} {request_id}"),
        "pairing_request_handled",
        Duration::from_secs(5),
    )?;
    let expected_action = if action == "approve" {
        "approved"
    } else {
        "rejected"
    };
    if event.get("request_id").and_then(serde_json::Value::as_str)
        != Some(request_id.to_string().as_str())
        || event.get("action").and_then(serde_json::Value::as_str) != Some(expected_action)
    {
        bail!("Pix host returned a mismatched pairing completion event");
    }
    if output.is_json() {
        return output.success(
            &format!("device.{action}"),
            &serde_json::json!({
                "request": request.as_ref().map_or_else(
                    || serde_json::json!({"id": request_id}),
                    |request| serde_json::json!({
                        "id": request.id,
                        "device_name": request.device_name,
                        "confirmation_code": request.confirmation_code,
                        "expires_at": request.expires_at,
                    })
                ),
                "action": expected_action,
            }),
        );
    }
    if let Some(request) = request {
        println!(
            "Pairing request {expected_action} for {}.",
            terminal_label(&request.device_name)
        );
    } else {
        println!("Pairing request {request_id} {expected_action}.");
    }
    Ok(())
}

pub(crate) fn select_pending_pairing(
    requests: &[PendingPairing],
    request_id: Option<uuid::Uuid>,
    code: Option<&str>,
    interactive: bool,
) -> Result<PendingPairing> {
    if requests.is_empty() {
        bail!("no pairing requests are waiting for approval");
    }
    if let Some(request_id) = request_id {
        return requests
            .iter()
            .find(|request| request.id == request_id)
            .cloned()
            .with_context(|| format!("unknown pairing request: {request_id}"));
    }
    if let Some(code) = code {
        let code = normalize_confirmation_code(code)?;
        let matches = requests
            .iter()
            .filter(|request| request.confirmation_code == code)
            .cloned()
            .collect::<Vec<_>>();
        return match matches.as_slice() {
            [request] => Ok(request.clone()),
            [] => bail!("no pending pairing request matches code {code}"),
            _ => bail!("multiple pending pairing requests match code {code}; use --request"),
        };
    }
    if !interactive {
        return Err(usage_error(
            "--request or --code is required outside an interactive terminal",
        ));
    }
    let ui = SetupUi::new(true, false);
    let options = requests
        .iter()
        .map(|request| {
            format!(
                "{}  {}",
                terminal_label(&request.device_name),
                format_confirmation_code(&request.confirmation_code)
            )
        })
        .collect::<Vec<_>>();
    let selected = ui.select("Choose a pairing request", &options, 0)?;
    Ok(requests[selected].clone())
}

pub(crate) fn normalize_confirmation_code(value: &str) -> Result<String> {
    let code = value
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '-')
        .collect::<String>();
    if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(code)
    } else {
        Err(usage_error("pairing code must contain exactly six digits"))
    }
}

pub(crate) fn device_menu(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let config = load_or_ephemeral_config(store)?;
    let ui = SetupUi::new(true, false);
    ui.crumb_header("Devices");
    ui.status_row(
        "paired",
        &format!(
            "{} device{}",
            config.devices.len(),
            plural(config.devices.len())
        ),
        if config.devices.is_empty() {
            UiTone::Warning
        } else {
            UiTone::Default
        },
    );
    println!();
    let mut actions = vec![
        (
            Some(DeviceCommand::Pair { remote: false }),
            MenuItem::new("Pair a device", "Connect another iPhone"),
        ),
        (
            Some(DeviceCommand::Approve {
                request: None,
                code: None,
            }),
            MenuItem::new("Approve pairing", "Review a waiting confirmation code"),
        ),
        (
            Some(DeviceCommand::Reject {
                request: None,
                code: None,
            }),
            MenuItem::new("Reject pairing", "Deny a waiting pairing request"),
        ),
    ];
    if !config.devices.is_empty() {
        actions.push((
            Some(DeviceCommand::Revoke { id: None }),
            MenuItem::new("Revoke a device", "Remove host access immediately"),
        ));
        actions.push((
            Some(DeviceCommand::List),
            MenuItem::new("List devices", "Show paired device details"),
        ));
    }
    actions.push((None, MenuItem::new("Back", "Return to the shell")));
    let items = actions.iter().map(|(_, item)| *item).collect::<Vec<_>>();
    match ui.menu("Actions", &items, 0)? {
        MenuResult::Selected(index) => match actions.swap_remove(index).0 {
            Some(command) => device(store, Some(command), output, true),
            None => Ok(()),
        },
        MenuResult::Help => print_cli_help(),
        MenuResult::Quit => Ok(()),
    }
}

pub(crate) fn select_device_id(
    config: &pix_core::HostConfig,
    id: Option<String>,
    interactive: bool,
) -> Result<String> {
    if let Some(id) = id {
        return Ok(id);
    }
    if config.devices.is_empty() {
        bail!("no paired devices");
    }
    if !interactive {
        return Err(usage_error(
            "device ID is required outside an interactive terminal",
        ));
    }
    let ui = SetupUi::new(true, false);
    let options = config
        .devices
        .iter()
        .map(|device| format!("{}  {}", terminal_label(&device.name), short_id(&device.id)))
        .collect::<Vec<_>>();
    let selected = ui.select("Choose a device to revoke", &options, 0)?;
    Ok(config.devices[selected].id.clone())
}

use crate::DeviceCommand;
use crate::commands::setup::{SetupPairingOptions, run_setup_pairing};
use crate::commands::shared::{
    default_host_name, format_confirmation_code, host_service_control_live,
    load_or_ephemeral_config, plural, short_id, terminal_label,
};
use crate::serve::DeviceEvent;
use crate::serve::{HostLog, ServeEvent, ServeOutput, emit_command_error, emit_event};
use crate::{print_cli_help, usage_error};
use crate::{service, service_client};
