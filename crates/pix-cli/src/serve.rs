//! The foreground `pix serve` host loop: command dispatch, the JSONL event
//! bridge, remote pairing offers, and the local control RPC responder.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use pix_core::{
    ConfigStore, HostEnvironment, HostService, HostServiceEvent, HostState, PairingCoordinator,
    RuntimeManager, RuntimeManagerOptions,
};
use qrcode::{QrCode, render::unicode};
use serde::Serialize;
use tempfile::{Builder as TempFileBuilder, NamedTempFile};

use crate::setup_ui::{SetupUi, clamp_text};
use crate::status::{
    HostControlCommand, HostControlResponder, HostServiceControl, HostServiceStatus,
    HostServiceStatusGuard,
};

pub(crate) const PI_CONTEXT_GUARD_SOURCE: &str = include_str!("../resources/pi-context-guard.mjs");

/// Payload-free, append-only host service log.
///
/// Every service event, command error, and panic lands here with a
/// timestamp, so a dead or misbehaving host process leaves a trace even
/// when nothing was watching its stdout. Entries never contain prompts,
/// messages, keys, tokens, or channel secrets — the same rule as stdout
/// events.
#[derive(Clone)]
pub(crate) struct HostLog {
    path: PathBuf,
    file: std::sync::Arc<std::sync::Mutex<Option<std::fs::File>>>,
}

pub(crate) const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

pub(crate) fn open_log_file(path: &std::path::Path) -> Option<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.custom_flags(libc::O_NOFOLLOW);
        options.mode(0o600);
        let file = options.open(path).ok()?;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        Some(file)
    }
    #[cfg(not(unix))]
    {
        options.open(path).ok()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ServeOutput {
    json_events: bool,
    stdout: bool,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn serve(store: &ConfigStore, json_events: bool, service_mode: bool) -> Result<()> {
    let output = ServeOutput {
        json_events,
        // A systemd journal must never receive the raw event stream. The
        // daemon keeps the payload-free file log, while the native UI keeps
        // its existing JSONL stdout bridge in non-service mode.
        stdout: !service_mode,
    };
    if let Some(current) = HostServiceStatus::current(store.path()) {
        bail!(
            "Pix host service is already running (pid {}, port {})",
            current.pid,
            current.port
        );
    }
    let log = HostLog::open(store.path());
    log.install_panic_hook();
    log.append_text("lifecycle", "serve starting");
    {
        let log = log.clone();
        pix_core::install_diagnostic_sink(move |event, body| {
            log.append(event, body);
        });
    }
    // Hold the cross-process config lock until the control sockets are bound.
    // An offline CLI mutation can therefore either complete before this
    // snapshot is loaded or observe the running service and request a refresh;
    // it cannot race into an unobservable stale in-memory authorization view.
    let startup_config = store.transaction()?;
    let config = startup_config
        .load_or_create(default_host_name())
        .context("loading Pix configuration")?;
    let identity = load_host_identity(store, config.host.id).context("loading host identity")?;
    let config_directory = store
        .path()
        .parent()
        .context("locating Pix configuration directory")?;
    let context_guard = create_pi_context_guard(config_directory)?;
    let context_guard_path = context_guard.path().display().to_string();
    let endpoint = pix_core::LanEndpoint::start(
        0,
        config.host.display_name.clone(),
        &identity.public_key,
        config.host.id,
    )
    .context("starting Bonjour host endpoint")?;
    let port = endpoint
        .local_addr()
        .context("inspecting host endpoint")?
        .port();
    let mut control =
        HostServiceControl::bind(store.path()).context("starting host service control")?;
    // Published after the listener is bound and removed when this process
    // exits; `pix status` and the diagnostic bundle use it for liveness.
    let _status_guard = HostServiceStatusGuard::create(store.path(), port)
        .context("writing host service status")?;
    drop(startup_config);
    let environment = HostEnvironment::resolve_for("pi");
    let executable = configured_pi_executable(&config, &environment);
    let pi_executable = executable.display().to_string();
    let runtime_manager = std::sync::Arc::new(
        RuntimeManager::new(RuntimeManagerOptions {
            executable,
            lock_directory: config_directory.join("locks"),
            max_active_sessions: config.preferences.max_active_sessions,
            max_concurrent_turns: config.preferences.max_concurrent_turns,
            idle_timeout: std::time::Duration::from_secs(config.preferences.idle_timeout_seconds),
            request_timeout: std::time::Duration::from_secs(30),
            // Keep Pi's normal extension discovery intact. The Pix-owned
            // compatibility extension only projects idle hidden custom
            // notifications out of model context; it does not disable or
            // replace any user extension or active-turn context injection.
            extra_arguments: vec!["--extension".to_owned(), context_guard_path],
            environment: environment.clone(),
        })
        .context("starting Pi runtime manager")?,
    );
    let coordinator = std::sync::Arc::new(PairingCoordinator::new(store.clone()));
    let host_fingerprint = pix_core::host_public_key_fingerprint(&identity.public_key);
    let relay_url = config.preferences.active_relay_url().map(str::to_owned);
    let mut service = HostService::start(
        endpoint,
        identity.private_key,
        coordinator,
        std::sync::Arc::new(HostState::new(config)),
        std::sync::Arc::clone(&runtime_manager),
    )
    .context("starting Pix host service")?;
    let (relay_events_tx, relay_events) = mpsc::channel();
    let relay = match &relay_url {
        Some(url) => {
            let manager = pix_core::RelayManager::new(url.clone(), port, relay_events_tx);
            sync_relay_devices(&manager, store);
            emit_event(
                &ServeEvent::RelayConfigured { url: url.clone() },
                output,
                &log,
                &mut control,
            );
            Some(manager)
        }
        None => None,
    };
    emit_event(
        &ServeEvent::Ready {
            port,
            fingerprint: host_fingerprint.clone(),
        },
        output,
        &log,
        &mut control,
    );
    emit_event(
        &ServeEvent::Environment {
            source: environment.describe(),
            path_entries: environment.path_entry_count(),
            pi_executable,
        },
        output,
        &log,
        &mut control,
    );
    emit_devices(&service, output, &log, &mut control);
    emit_sessions(&service, output, &log, &mut control);

    let (command_tx, command_rx) = mpsc::channel::<String>();
    if !service_mode {
        thread::Builder::new()
            .name("pix-cli-stdin-control".to_owned())
            .spawn(move || {
                let stdin = std::io::stdin();
                for line in stdin.lock().lines().map_while(Result::ok) {
                    if command_tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .context("starting service command reader")?;
    }

    let mut should_stop = false;
    let mut commands_disconnected = false;
    // QR payload is held until the pairing agent is actually waiting on the
    // relay. Emitting it at `pair-remote` time lets a phone scan a channel
    // the host has not joined yet; the handshake then hangs and the Mac
    // never sees a pairing request.
    let mut pending_remote_pairing: Option<PendingRemotePairing> = None;
    let mut last_runtime_maintenance = Instant::now();
    loop {
        if last_runtime_maintenance.elapsed() >= Duration::from_secs(1) {
            last_runtime_maintenance = Instant::now();
            match service.refresh_config() {
                Ok(report) if report.cleanup_pending || report.connection_cleanup_failed => {
                    log.append_text("config", "authorization applied; cleanup will retry");
                }
                Ok(_) => {}
                Err(error) => {
                    log.append_text("config", &format!("automatic refresh failed: {error}"));
                }
            }
            let _ = service.expire_pending_requests();
            if let Some(manager) = &relay {
                sync_relay_devices(manager, store);
            }
            if let Err(error) = runtime_manager.reap_exited() {
                log.append_text("runtime", &format!("reap failed: {error}"));
            }
            if let Err(error) = runtime_manager.sweep_idle() {
                log.append_text("runtime", &format!("idle sweep failed: {error}"));
            }
        }
        control
            .poll_event_subscribers()
            .context("accepting host event subscribers")?;
        while let Some(event) = service.try_next_event().context("reading host event")? {
            if let HostServiceEvent::PairingRequested(request) = &event
                && request.peer_addr.ip().is_loopback()
                && let Some(pending) = pending_remote_pairing.as_mut()
                && pending.in_use
            {
                pending.request_id = Some(request.id);
            }
            let resync_relay = matches!(
                &event,
                HostServiceEvent::ConnectionEstablished { .. }
                    | HostServiceEvent::ConnectionClosed { .. }
            );
            emit_event(&ServeEvent::from(event), output, &log, &mut control);
            // Pairing approval and revocation both surface as connection
            // events; reconcile standing relay channels with durable trust.
            if resync_relay && let Some(manager) = &relay {
                sync_relay_devices(manager, store);
            }
        }
        while let Ok(event) = relay_events.try_recv() {
            let serve_event = ServeEvent::from(event);
            let pairing_waiting = matches!(
                &serve_event,
                ServeEvent::RelayChannel { label, state, .. }
                    if label == "pairing" && state == "waiting"
            );
            let pairing_failed = matches!(
                &serve_event,
                ServeEvent::RelayChannel { label, state, .. }
                    if label == "pairing" && state.starts_with("failed")
            );
            let pairing_peer_joined = matches!(
                &serve_event,
                ServeEvent::RelayChannel { label, state, .. }
                    if label == "pairing" && state == "peer_joined"
            );
            emit_event(&serve_event, output, &log, &mut control);
            if pairing_peer_joined && let Some(pending) = pending_remote_pairing.as_mut() {
                pending.in_use = true;
            }
            if pairing_waiting && let Some(pending) = pending_remote_pairing.as_mut() {
                if !pending.ready {
                    let ready = ServeEvent::RemotePairingReady {
                        qr_payload: pending.qr_payload.clone(),
                        join_code: pending.join_code.clone(),
                        expires_at: pending.expires_at,
                        local_request_id: pending.local_request_id,
                    };
                    emit_event(&ready, output, &log, &mut control);
                    if let Some(responder) = pending.responder.take()
                        && let Err(error) =
                            responder.success(&serde_json::to_value(&ready).unwrap_or_else(
                                |_| serde_json::json!({"type": "remote_pairing_ready"}),
                            ))
                    {
                        log.append_text("control", &format!("response failed: {error}"));
                    }
                    pending.ready = true;
                }
            } else if pairing_failed && let Some(pending) = pending_remote_pairing.take() {
                let error = anyhow::anyhow!("remote pairing channel failed to join the relay");
                emit_command_error_for(
                    &error,
                    pending.local_request_id,
                    output,
                    &log,
                    &mut control,
                );
                if let Some(responder) = pending.responder {
                    let _ = responder.error("relay_unavailable", &error.to_string());
                }
            }
        }
        // Commands from the local control socket and foreground stdin share
        // exactly the same dispatcher. This is what lets the menu app and
        // `pix device pair` operate while the one persistent service remains
        // alive.
        let mut incoming_commands = Vec::new();
        while let Some(command) = control
            .try_next_command()
            .context("reading host service control")?
        {
            incoming_commands.push(command);
        }
        loop {
            match command_rx.try_recv() {
                Ok(line) => incoming_commands.push(HostControlCommand::Legacy(line)),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    commands_disconnected = true;
                    break;
                }
            }
        }
        for incoming in incoming_commands {
            let (line, mut rpc_responder) = match incoming {
                HostControlCommand::Legacy(line) => (line, None),
                HostControlCommand::Rpc {
                    command,
                    args,
                    responder,
                } => match rpc_command_line(&command, &args) {
                    Ok(line) => (line, Some(responder)),
                    Err(error) => {
                        let _ = responder.error("invalid_request", &format!("{error:#}"));
                        continue;
                    }
                },
            };
            let mut words = line.split_whitespace();
            let first = words.next();
            let (local_request_id, command) = match first {
                Some(token) if token.starts_with('@') => {
                    let request_id = token
                        .strip_prefix('@')
                        .and_then(|value| uuid::Uuid::parse_str(value).ok());
                    let Some(request_id) = request_id else {
                        emit_command_error(
                            &anyhow::anyhow!("invalid local request correlation ID"),
                            output,
                            &log,
                            &mut control,
                        );
                        continue;
                    };
                    (Some(request_id), words.next())
                }
                command => (None, command),
            };
            match command {
                Some("approve") => {
                    let request_id = words.next().map(str::to_owned);
                    if let Err(error) = approve_pairing(
                        &service,
                        request_id.as_deref(),
                        local_request_id,
                        output,
                        &log,
                        &mut control,
                    ) {
                        emit_command_error_for(
                            &error,
                            local_request_id,
                            output,
                            &log,
                            &mut control,
                        );
                        respond_rpc_error(&mut rpc_responder, &error, &log);
                    } else {
                        if let Some(manager) = &relay {
                            sync_relay_devices(manager, store);
                        }
                        if pending_remote_pairing.as_ref().is_some_and(|pending| {
                            pending.request_id.map(|id| id.to_string()) == request_id
                        }) {
                            pending_remote_pairing = None;
                        }
                        respond_rpc_success(
                            &mut rpc_responder,
                            serde_json::json!({
                                "type": "pairing_request_handled",
                                "request_id": request_id,
                                "action": "approved",
                            }),
                            &log,
                        );
                    }
                }
                Some("reject") => {
                    let request_id = words.next().map(str::to_owned);
                    if let Err(error) = reject_pairing(
                        &service,
                        request_id.as_deref(),
                        local_request_id,
                        output,
                        &log,
                        &mut control,
                    ) {
                        emit_command_error_for(
                            &error,
                            local_request_id,
                            output,
                            &log,
                            &mut control,
                        );
                        respond_rpc_error(&mut rpc_responder, &error, &log);
                    } else {
                        if pending_remote_pairing.as_ref().is_some_and(|pending| {
                            pending.request_id.map(|id| id.to_string()) == request_id
                        }) {
                            pending_remote_pairing = None;
                        }
                        respond_rpc_success(
                            &mut rpc_responder,
                            serde_json::json!({
                                "type": "pairing_request_handled",
                                "request_id": request_id,
                                "action": "rejected",
                            }),
                            &log,
                        );
                    }
                }
                Some("revoke") => {
                    let device_id = words.next().map(str::to_owned);
                    match revoke_device(
                        &service,
                        device_id.as_deref(),
                        local_request_id,
                        output,
                        &log,
                        &mut control,
                    ) {
                        Err(error) => {
                            emit_command_error_for(
                                &error,
                                local_request_id,
                                output,
                                &log,
                                &mut control,
                            );
                            respond_rpc_error(&mut rpc_responder, &error, &log);
                        }
                        Ok(revoked) => {
                            if let Some(manager) = &relay {
                                sync_relay_devices(manager, store);
                            }
                            respond_rpc_success(
                                &mut rpc_responder,
                                serde_json::json!({
                                    "type": "device_revoked",
                                    "device_id": device_id,
                                    "closed_connections": revoked.closed_connections,
                                    "connection_cleanup_failed": revoked.connection_cleanup_failed,
                                }),
                                &log,
                            );
                        }
                    }
                }
                Some("devices") => {
                    emit_devices(&service, output, &log, &mut control);
                    respond_rpc_success(
                        &mut rpc_responder,
                        serde_json::json!({"type": "device_list"}),
                        &log,
                    );
                }
                Some("sessions") => {
                    emit_sessions_for(&service, local_request_id, output, &log, &mut control);
                    respond_rpc_success(&mut rpc_responder, session_list_json(&service), &log);
                }
                Some("refresh") => match service.refresh_config() {
                    Ok(report) => {
                        emit_event(
                            &ServeEvent::ConfigRefreshed {
                                authorization_changed: report.authorization_changed,
                                released_sessions: report.released_sessions,
                                cleanup_pending: report.cleanup_pending,
                                connection_cleanup_failed: report.connection_cleanup_failed,
                                local_request_id,
                            },
                            output,
                            &log,
                            &mut control,
                        );
                        emit_devices(&service, output, &log, &mut control);
                        emit_sessions(&service, output, &log, &mut control);
                        respond_rpc_success(
                            &mut rpc_responder,
                            serde_json::json!({
                                "type": "config_refreshed",
                                "authorization_applied": true,
                                "authorization_changed": report.authorization_changed,
                                "released_sessions": report.released_sessions,
                                "cleanup_pending": report.cleanup_pending,
                                "connection_cleanup_failed": report.connection_cleanup_failed,
                            }),
                            &log,
                        );
                    }
                    Err(error) => {
                        let error = anyhow::Error::new(error);
                        emit_command_error_for(
                            &error,
                            local_request_id,
                            output,
                            &log,
                            &mut control,
                        );
                        respond_rpc_error(&mut rpc_responder, &error, &log);
                    }
                },
                Some("release") => {
                    let session_id = words.next().map(str::to_owned);
                    if let Err(error) = release_session(
                        &service,
                        session_id.as_deref(),
                        local_request_id,
                        output,
                        &log,
                        &mut control,
                    ) {
                        emit_command_error_for(
                            &error,
                            local_request_id,
                            output,
                            &log,
                            &mut control,
                        );
                        respond_rpc_error(&mut rpc_responder, &error, &log);
                    } else {
                        respond_rpc_success(
                            &mut rpc_responder,
                            serde_json::json!({
                                "type": "session_released",
                                "session_id": session_id,
                            }),
                            &log,
                        );
                    }
                }
                Some("pair-remote") => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    // A control-socket caller may be retrying a lost response:
                    // replay an unused standing offer and protect one a phone
                    // has already joined. Interactive stdin commands are
                    // deliberate and always replace the standing offer.
                    if rpc_responder.is_some()
                        && let Some(pending) = pending_remote_pairing.as_ref()
                        && pending.expires_at > now
                    {
                        if pending.ready && !pending.in_use {
                            let ready = ServeEvent::RemotePairingReady {
                                qr_payload: pending.qr_payload.clone(),
                                join_code: pending.join_code.clone(),
                                expires_at: pending.expires_at,
                                local_request_id,
                            };
                            emit_event(&ready, output, &log, &mut control);
                            respond_rpc_success(
                                &mut rpc_responder,
                                serde_json::to_value(&ready).unwrap_or_else(
                                    |_| serde_json::json!({"type": "remote_pairing_ready"}),
                                ),
                                &log,
                            );
                        } else {
                            respond_rpc_error_message(
                                &mut rpc_responder,
                                "conflict",
                                "a remote pairing offer is already active",
                                &log,
                            );
                        }
                        continue;
                    }
                    if let Some(superseded) = pending_remote_pairing.take()
                        && let Some(responder) = superseded.responder
                    {
                        let _ = responder.error(
                            "superseded",
                            "a newer pair-remote command replaced this pairing offer",
                        );
                    }
                    match prepare_remote_pairing(
                        relay.as_ref(),
                        relay_url.as_deref(),
                        &host_fingerprint,
                        local_request_id,
                    ) {
                        Ok(mut pending) => {
                            pending.responder = rpc_responder.take();
                            pending_remote_pairing = Some(pending);
                        }
                        Err(error) => {
                            emit_command_error_for(
                                &error,
                                local_request_id,
                                output,
                                &log,
                                &mut control,
                            );
                            respond_rpc_error(&mut rpc_responder, &error, &log);
                        }
                    }
                }
                Some("pending") => {
                    for request in service.pending_requests() {
                        emit_event(
                            &ServeEvent::PairingRequested {
                                id: request.id,
                                device_name: request.device_name,
                                confirmation_code: request.confirmation_code,
                                expires_at: request
                                    .expires_at
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            },
                            output,
                            &log,
                            &mut control,
                        );
                    }
                }
                Some("pending-list") => {
                    let requests = service
                        .pending_requests()
                        .into_iter()
                        .map(|request| PendingPairingEvent {
                            id: request.id,
                            device_name: request.device_name,
                            confirmation_code: request.confirmation_code,
                            expires_at: request
                                .expires_at
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        })
                        .collect();
                    let event = ServeEvent::PairingRequestList {
                        requests,
                        local_request_id,
                    };
                    let response = serde_json::to_value(&event)
                        .unwrap_or_else(|_| serde_json::json!({"type": "pairing_request_list"}));
                    emit_event(&event, output, &log, &mut control);
                    respond_rpc_success(&mut rpc_responder, response, &log);
                }
                Some("capabilities") => {
                    respond_rpc_success(
                        &mut rpc_responder,
                        serde_json::json!({
                            "type": "capabilities",
                            "control_schema_version": 1,
                        }),
                        &log,
                    );
                }
                Some("quit" | "exit") => {
                    should_stop = true;
                    break;
                }
                _ => {
                    if let Some(responder) = rpc_responder.take() {
                        let _ = responder.error("unknown_command", "unknown host control command");
                    }
                }
            }
        }
        if should_stop || commands_disconnected {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    log.append_text(
        "lifecycle",
        if should_stop {
            "serve stopping (quit command)"
        } else if service_mode {
            "serve stopping (control command)"
        } else {
            "serve stopping (stdin closed)"
        },
    );
    if let Some(manager) = &relay {
        manager.shutdown();
    }
    service.shutdown();
    Ok(())
}

pub(crate) fn rpc_command_line(command: &str, args: &serde_json::Value) -> Result<String> {
    let no_args = |legacy: &str| -> Result<String> {
        if args.is_null() || args.as_object().is_some_and(serde_json::Map::is_empty) {
            Ok(legacy.to_owned())
        } else {
            bail!("{command} does not accept arguments")
        }
    };
    match command {
        "capabilities" => no_args("capabilities"),
        "pairing.pending" => no_args("pending-list"),
        "pairing.remote_offer" => no_args("pair-remote"),
        "session.list" => no_args("sessions"),
        "config.refresh" => no_args("refresh"),
        "pairing.approve" => Ok(format!(
            "approve {}",
            rpc_token_arg(args, "request_id", 64)?
        )),
        "pairing.reject" => Ok(format!("reject {}", rpc_token_arg(args, "request_id", 64)?)),
        "device.revoke" => Ok(format!("revoke {}", rpc_token_arg(args, "device_id", 256)?)),
        "session.release" => Ok(format!(
            "release {}",
            rpc_token_arg(args, "session_id", 128)?
        )),
        _ => bail!("unknown host control command: {command}"),
    }
}

pub(crate) fn rpc_token_arg<'a>(
    args: &'a serde_json::Value,
    name: &str,
    max_len: usize,
) -> Result<&'a str> {
    let value = args
        .get(name)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("missing string argument {name}"))?;
    if value.is_empty()
        || value.len() > max_len
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        bail!("invalid host control argument {name}");
    }
    Ok(value)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn respond_rpc_success(
    responder: &mut Option<HostControlResponder>,
    data: serde_json::Value,
    log: &HostLog,
) {
    if let Some(responder) = responder.take()
        && let Err(error) = responder.success(&data)
    {
        log.append_text("control", &format!("response failed: {error}"));
    }
}

pub(crate) fn respond_rpc_error(
    responder: &mut Option<HostControlResponder>,
    error: &anyhow::Error,
    log: &HostLog,
) {
    respond_rpc_error_message(responder, "command_failed", &format!("{error:#}"), log);
}

pub(crate) fn respond_rpc_error_message(
    responder: &mut Option<HostControlResponder>,
    code: &str,
    message: &str,
    log: &HostLog,
) {
    if let Some(responder) = responder.take()
        && let Err(error) = responder.error(code, message)
    {
        log.append_text("control", &format!("error response failed: {error}"));
    }
}

pub(crate) fn session_list_json(service: &pix_core::HostServiceHandle) -> serde_json::Value {
    let sessions = service
        .active_sessions()
        .into_iter()
        .map(|session| {
            serde_json::json!({
                "id": session.session_id.to_string(),
                "workspace": session.workspace,
                "clients": session.client_count,
                "state": if session.completed { "idle" } else { "running" },
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({"type": "session_list", "sessions": sessions})
}

/// Materializes the small Pix context projection extension next to the host
/// config for the lifetime of `pix serve`. `pi -e` accepts a file path, while
/// embedding the source here keeps `cargo install` and app-bundled binaries
/// self-contained without requiring a second installed extension package.
pub(crate) fn create_pi_context_guard(directory: &std::path::Path) -> Result<NamedTempFile> {
    let mut file = TempFileBuilder::new()
        .prefix(".pix-pi-context-guard-")
        .suffix(".mjs")
        .tempfile_in(directory)
        .context("creating Pi context guard")?;
    file.write_all(PI_CONTEXT_GUARD_SOURCE.as_bytes())
        .context("writing Pi context guard")?;
    file.flush().context("flushing Pi context guard")?;
    Ok(file)
}

/// Reconciles standing relay channels with the durable paired-device list.
/// Failures are payload-free and non-fatal: the LAN path stays available.
pub(crate) fn sync_relay_devices(manager: &pix_core::RelayManager, store: &ConfigStore) {
    let Ok(config) = store.load() else { return };
    let _ = manager.sync_devices(
        config
            .devices
            .iter()
            .map(|device| (device.id.as_str(), device.relay_channel.as_str())),
    );
}

pub(crate) const REMOTE_PAIRING_TTL: Duration = Duration::from_secs(120);

pub(crate) struct PendingRemotePairing {
    qr_payload: String,
    join_code: String,
    expires_at: u64,
    local_request_id: Option<uuid::Uuid>,
    responder: Option<HostControlResponder>,
    ready: bool,
    in_use: bool,
    request_id: Option<uuid::Uuid>,
}

pub(crate) fn prepare_remote_pairing(
    relay: Option<&pix_core::RelayManager>,
    relay_url: Option<&str>,
    host_fingerprint: &str,
    local_request_id: Option<uuid::Uuid>,
) -> Result<PendingRemotePairing> {
    let (Some(manager), Some(url)) = (relay, relay_url) else {
        bail!("remote pairing requires a configured relay (`pix relay set <url>`)");
    };
    let offer = manager
        .start_remote_pairing(REMOTE_PAIRING_TTL)
        .context("starting remote pairing channel")?;
    let payload = format!(
        "pix://pair?v=1&relay={}&secret={}&fp={host_fingerprint}",
        percent_encode(url),
        offer.channel_secret,
    );
    let expires_at = std::time::SystemTime::now()
        .checked_add(offer.expires_in)
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs());
    Ok(PendingRemotePairing {
        qr_payload: payload,
        join_code: offer.join_code,
        expires_at,
        local_request_id,
        responder: None,
        ready: false,
        in_use: false,
        request_id: None,
    })
}

/// RFC 3986 percent-encoding for QR query values; unreserved bytes pass.
pub(crate) fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => {
                use std::fmt::Write as _;
                write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
            }
        }
    }
    encoded
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServeEvent {
    Ready {
        port: u16,
        fingerprint: String,
    },
    /// How the host located Pi; names and values of the captured variables
    /// are deliberately absent.
    Environment {
        source: String,
        path_entries: usize,
        pi_executable: String,
    },
    PairingRequested {
        id: uuid::Uuid,
        device_name: String,
        confirmation_code: String,
        expires_at: u64,
    },
    PairingRequestList {
        requests: Vec<PendingPairingEvent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        local_request_id: Option<uuid::Uuid>,
    },
    PairingRequestHandled {
        request_id: uuid::Uuid,
        action: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        local_request_id: Option<uuid::Uuid>,
    },
    ConnectionEstablished {
        connection_id: String,
        device_id: String,
        device_name: String,
    },
    ConnectionClosed {
        connection_id: String,
        device_id: String,
    },
    ConnectionFailed {
        stage: String,
    },
    DeviceList {
        devices: Vec<DeviceEvent>,
    },
    DeviceRevoked {
        device_id: String,
        device_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        local_request_id: Option<uuid::Uuid>,
    },
    SessionList {
        sessions: Vec<SessionEvent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        local_request_id: Option<uuid::Uuid>,
    },
    SessionReleased {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        local_request_id: Option<uuid::Uuid>,
    },
    ConfigRefreshed {
        authorization_changed: bool,
        released_sessions: usize,
        cleanup_pending: bool,
        connection_cleanup_failed: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        local_request_id: Option<uuid::Uuid>,
    },
    /// Relay transport is configured; standing channels are being kept.
    RelayConfigured {
        url: String,
    },
    /// Payload-free relay channel lifecycle for one device label.
    RelayChannel {
        label: String,
        state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A remote pairing QR payload is ready to display.
    RemotePairingReady {
        qr_payload: String,
        join_code: String,
        expires_at: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        local_request_id: Option<uuid::Uuid>,
    },
    CommandError {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        local_request_id: Option<uuid::Uuid>,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct DeviceEvent {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) paired_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PendingPairingEvent {
    id: uuid::Uuid,
    device_name: String,
    confirmation_code: String,
    expires_at: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionEvent {
    pub(crate) id: String,
    pub(crate) workspace: String,
    pub(crate) clients: usize,
    pub(crate) state: &'static str,
}

/// Emits one service event to the log file (always) and stdout (best
/// effort).
///
/// A broken or stalled stdout pipe — the native app paused in a debugger,
/// or gone entirely — must never terminate or panic the host service. The
/// service exits through stdin EOF or `quit`, and the log file keeps the
/// record either way.
pub(crate) fn emit_event(
    event: &ServeEvent,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) {
    let logged = loggable_event(event);
    log.append("event", &logged);

    // The local event socket is the durable service bridge for the CLI and
    // native menu app. It always receives JSON, including when launchd or
    // systemd intentionally suppresses stdout.
    if let Ok(line) = serde_json::to_string(event) {
        let _ = control.publish_event(&line);
    }

    if !output.stdout {
        return;
    }

    let line = if output.json_events {
        serde_json::to_string(event).unwrap_or_else(|_| {
            r#"{"type":"command_error","message":"event encoding failed"}"#.to_owned()
        })
    } else {
        human_event(event)
    };
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "{line}");
    if !line.ends_with('\n') {
        let _ = writeln!(stdout);
    }
    let _ = stdout.flush();
}

/// Stable, product-facing rendering for the foreground host. The JSON event
/// stream remains the native UI/automation contract; this renderer deliberately
/// does not expose Rust debug structs, UUIDs, fingerprints, or relay secrets.
pub(crate) fn human_event(event: &ServeEvent) -> String {
    match event {
        ServeEvent::Ready { .. } => "✓ Pix host is ready\n".to_owned(),
        ServeEvent::Environment { .. } => "✓ Pi environment ready\n".to_owned(),
        ServeEvent::PairingRequested {
            device_name,
            confirmation_code,
            ..
        } => format!(
            "\n{} wants to pair.\nVerify this code on your phone: {}\n\n",
            terminal_label(device_name),
            format_confirmation_code(confirmation_code)
        ),
        ServeEvent::PairingRequestList { requests, .. } => format!(
            "○ {} pairing request{} pending\n",
            requests.len(),
            plural(requests.len())
        ),
        ServeEvent::PairingRequestHandled { action, .. } => {
            format!("✓ Pairing request {action}\n")
        }
        ServeEvent::ConnectionEstablished { device_name, .. } => {
            format!("✓ {} paired and connected\n", terminal_label(device_name))
        }
        ServeEvent::ConnectionClosed { .. } => "○ Device disconnected\n".to_owned(),
        ServeEvent::ConnectionFailed { .. } => "✕ Device connection failed\n".to_owned(),
        ServeEvent::DeviceList { devices } => {
            format!(
                "✓ {} paired device{}\n",
                devices.len(),
                plural(devices.len())
            )
        }
        ServeEvent::DeviceRevoked { device_name, .. } => {
            format!("✓ Revoked {}\n", terminal_label(device_name))
        }
        ServeEvent::SessionList { sessions, .. } => {
            format!(
                "✓ {} active session{}\n",
                sessions.len(),
                plural(sessions.len())
            )
        }
        ServeEvent::SessionReleased { .. } => "✓ Session released\n".to_owned(),
        ServeEvent::ConfigRefreshed { .. } => "✓ Configuration refreshed\n".to_owned(),
        ServeEvent::RelayConfigured { .. } => "✓ Relay configured\n".to_owned(),
        ServeEvent::RelayChannel { label, state, .. } => match (label.as_str(), state.as_str()) {
            ("pairing", "waiting") => "◐ Waiting for a device…\n".to_owned(),
            ("pairing", "peer_joined") => "✓ Device connected to relay\n".to_owned(),
            (_, "peer_joined") => "✓ Remote connection established\n".to_owned(),
            (_, "peer_left") => "○ Remote device disconnected\n".to_owned(),
            (_, state) if state.starts_with("failed") => {
                "✕ Relay connection failed; LAN remains available\n".to_owned()
            }
            (_, "stopped") => "○ Relay connection stopped\n".to_owned(),
            _ => "○ Relay status changed\n".to_owned(),
        },
        ServeEvent::RemotePairingReady {
            qr_payload,
            join_code,
            expires_at,
            ..
        } => render_remote_pairing(qr_payload, join_code, *expires_at),
        ServeEvent::CommandError { message, .. } => format!("✕ {message}\n"),
    }
}

pub(crate) fn pairing_instructions(remote: bool) -> &'static str {
    if remote {
        "Open Pix on your iPhone and scan this QR code."
    } else {
        "Open Pix on your iPhone and choose this Mac from nearby hosts."
    }
}

pub(crate) fn render_remote_pairing(qr_payload: &str, join_code: &str, expires_at: u64) -> String {
    use std::fmt::Write as _;

    let terminal_width = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width > 0)
        .unwrap_or(80);
    let terminal_lines = std::env::var("LINES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(32);
    let mut output = String::from("\nScan this QR code with Pix:\n\n");
    match QrCode::new(qr_payload.as_bytes()) {
        Ok(code) => {
            // Keep the quiet zone whenever the terminal has room for it. On
            // compact terminals the reduced version avoids making the QR the
            // whole screen while preserving a scannable module grid.
            let image = code
                .render::<unicode::Dense1x2>()
                .quiet_zone(terminal_lines >= 28 && terminal_width >= 52)
                .build();
            for line in image.lines() {
                let width = line.chars().count();
                let padding = terminal_width.saturating_sub(width) / 2;
                let _ = writeln!(output, "{}{}", " ".repeat(padding), line);
            }
        }
        Err(_) => {
            // A QR renderer failure must not expose the encoded secret. The
            // machine-readable interface still contains the full payload.
            output.push_str("(QR rendering is unavailable in this terminal)\n");
        }
    }
    output.push_str("\nPairing code\n\n");
    let _ = writeln!(output, "{}", clamp_text(join_code, 32));
    if expires_at > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let remaining = expires_at.saturating_sub(now);
        let _ = writeln!(
            output,
            "Expires in {}:{:02}",
            remaining / 60,
            remaining % 60
        );
    }
    output.push('\n');
    output
}

pub(crate) fn render_remote_pairing_for_ui(
    ui: SetupUi,
    qr_payload: &str,
    join_code: &str,
    expires_at: u64,
) -> String {
    let rendered = render_remote_pairing(qr_payload, join_code, expires_at);
    rendered.replace(join_code, &ui.cyan(join_code, true))
}

pub(crate) fn loggable_event(event: &ServeEvent) -> serde_json::Value {
    match event {
        ServeEvent::Ready { port, .. } => serde_json::json!({
            "type": "ready",
            "port": port,
        }),
        ServeEvent::Environment { path_entries, .. } => serde_json::json!({
            "type": "environment",
            "path_entries": path_entries,
        }),
        ServeEvent::PairingRequested { expires_at, .. } => serde_json::json!({
            "type": "pairing_requested",
            "expires_at": expires_at,
        }),
        ServeEvent::PairingRequestList { requests, .. } => serde_json::json!({
            "type": "pairing_request_list",
            "count": requests.len(),
        }),
        ServeEvent::PairingRequestHandled { action, .. } => serde_json::json!({
            "type": "pairing_request_handled",
            "action": action,
        }),
        ServeEvent::ConnectionEstablished { .. } => {
            serde_json::json!({"type": "connection_established"})
        }
        ServeEvent::ConnectionClosed { .. } => serde_json::json!({"type": "connection_closed"}),
        ServeEvent::ConnectionFailed { stage } => serde_json::json!({
            "type": "connection_failed",
            "stage": stage,
        }),
        ServeEvent::DeviceList { devices } => serde_json::json!({
            "type": "device_list",
            "count": devices.len(),
        }),
        ServeEvent::DeviceRevoked { .. } => serde_json::json!({"type": "device_revoked"}),
        ServeEvent::SessionList { sessions, .. } => serde_json::json!({
            "type": "session_list",
            "count": sessions.len(),
        }),
        ServeEvent::SessionReleased { .. } => serde_json::json!({"type": "session_released"}),
        ServeEvent::ConfigRefreshed { .. } => serde_json::json!({"type": "config_refreshed"}),
        ServeEvent::RelayConfigured { .. } => serde_json::json!({"type": "relay_configured"}),
        ServeEvent::RelayChannel { state, .. } => serde_json::json!({
            "type": "relay_channel",
            "state": state,
        }),
        ServeEvent::RemotePairingReady { expires_at, .. } => serde_json::json!({
            "type": "remote_pairing_ready",
            "qr_payload": "[redacted]",
            "join_code": "[redacted]",
            "expires_at": expires_at,
        }),
        ServeEvent::CommandError { .. } => serde_json::json!({"type": "command_error"}),
    }
}

pub(crate) fn emit_command_error(
    error: &anyhow::Error,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) {
    emit_command_error_for(error, None, output, log, control);
}

pub(crate) fn emit_command_error_for(
    error: &anyhow::Error,
    local_request_id: Option<uuid::Uuid>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) {
    emit_event(
        &ServeEvent::CommandError {
            message: format!("{error:#}"),
            local_request_id,
        },
        output,
        log,
        control,
    );
}

use crate::commands::device::{approve_pairing, emit_devices, reject_pairing, revoke_device};
use crate::commands::pi::configured_pi_executable;
use crate::commands::session::{emit_sessions, emit_sessions_for, release_session};
use crate::commands::shared::{
    default_host_name, format_confirmation_code, load_host_identity, plural, terminal_label,
};

impl HostLog {
    pub(crate) fn open(config_path: &std::path::Path) -> Self {
        let path = Self::path_for(config_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = open_log_file(&path);
        Self {
            path,
            file: std::sync::Arc::new(std::sync::Mutex::new(file)),
        }
    }

    pub(crate) fn path_for(config_path: &std::path::Path) -> PathBuf {
        config_path
            .parent()
            .map_or_else(|| PathBuf::from("logs"), |dir| dir.join("logs"))
            .join("host.jsonl")
    }

    /// Appends one structured entry; logging failures never disturb the
    /// host service.
    pub(crate) fn append(&self, kind: &str, body: &serde_json::Value) {
        let entry = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "kind": kind,
            "body": body,
        });
        let mut guard = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(file) = guard.as_mut() {
            let _ = writeln!(file, "{entry}");
            let _ = file.flush();
        }
        drop(guard);
        self.rotate_if_needed();
    }

    pub(crate) fn append_text(&self, kind: &str, text: &str) {
        self.append(kind, &serde_json::Value::String(text.to_owned()));
    }

    pub(crate) fn rotate_if_needed(&self) {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return;
        };
        if metadata.len() < LOG_ROTATE_BYTES {
            return;
        }
        let mut guard = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rotated = self.path.with_extension("jsonl.1");
        let _ = std::fs::rename(&self.path, rotated);
        *guard = open_log_file(&self.path);
    }

    /// Routes panics from any host thread into the log before the default
    /// handler runs, so crashes are diagnosable after the fact.
    pub(crate) fn install_panic_hook(&self) {
        let log = self.clone();
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Panic payloads may contain paths or request data. Keep the
            // persistent log useful without copying those details to disk.
            log.append(
                "panic",
                &serde_json::json!({"message": "panic (details redacted)"}),
            );
            default_hook(info);
        }));
    }
}

impl From<HostServiceEvent> for ServeEvent {
    fn from(event: HostServiceEvent) -> Self {
        match event {
            HostServiceEvent::PairingRequested(request) => Self::PairingRequested {
                id: request.id,
                device_name: request.device_name,
                confirmation_code: request.confirmation_code,
                expires_at: request
                    .expires_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
            HostServiceEvent::ConnectionEstablished {
                connection_id,
                device_id,
                device_name,
            } => Self::ConnectionEstablished {
                connection_id: connection_id.to_string(),
                device_id,
                device_name,
            },
            HostServiceEvent::ConnectionClosed {
                connection_id,
                device_id,
            } => Self::ConnectionClosed {
                connection_id: connection_id.to_string(),
                device_id,
            },
            HostServiceEvent::ConnectionFailed { stage, .. } => Self::ConnectionFailed {
                stage: format!("{stage:?}"),
            },
        }
    }
}

impl From<pix_core::RelayServiceEvent> for ServeEvent {
    fn from(event: pix_core::RelayServiceEvent) -> Self {
        use pix_core::RelayServiceEvent as Relay;
        match event {
            Relay::ChannelWaiting { label } => Self::RelayChannel {
                label,
                state: "waiting".to_owned(),
                detail: None,
            },
            Relay::PeerJoined { label } => Self::RelayChannel {
                label,
                state: "peer_joined".to_owned(),
                detail: None,
            },
            Relay::PeerLeft { label } => Self::RelayChannel {
                label,
                state: "peer_left".to_owned(),
                detail: None,
            },
            Relay::ChannelFailed {
                label,
                stage,
                detail,
            } => Self::RelayChannel {
                label,
                state: format!("failed_{stage:?}").to_lowercase(),
                detail: Some(detail),
            },
            Relay::ChannelStopped { label } => Self::RelayChannel {
                label,
                state: "stopped".to_owned(),
                detail: None,
            },
        }
    }
}
