use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pix_core::{
    ConfigStore, HostEnvironment, HostIdentityStore, HostService, HostServiceEvent, HostState,
    PairingCoordinator, PiProbe, RuntimeManager, RuntimeManagerOptions, WorkspaceRegistry,
};
use qrcode::{QrCode, render::unicode};
use serde::Serialize;
use tempfile::{Builder as TempFileBuilder, NamedTempFile};

mod diagnostics;
mod service;
mod setup_ui;
mod status;

use crate::service::ServiceCommand;
use crate::setup_ui::{SetupUi, clamp_text};
use crate::status::{HostServiceControl, HostServiceStatus, HostServiceStatusGuard};

const PI_CONTEXT_GUARD_SOURCE: &str = include_str!("../resources/pi-context-guard.mjs");
const DEFAULT_RELAY_URL: &str = "wss://pix-relay.zaincheung-255.workers.dev";

#[derive(Debug, Parser)]
#[command(name = "pix", version, about = "Pix remote access for Pi")]
struct Cli {
    /// Override the platform Pix configuration path.
    #[arg(long, global = true, env = "PIX_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Guide a new host through Pi checks, workspace authorization, pairing,
    /// and background-service setup.
    Setup {
        /// Relay WebSocket endpoint. Omit it to use LAN pairing or to answer
        /// the interactive relay prompt.
        #[arg(long, visible_alias = "relay-url", env = "PIX_RELAY_URL")]
        relay: Option<String>,
        /// Workspace root to authorize. Omit it to answer the interactive
        /// workspace prompt.
        #[arg(long, value_name = "PATH", env = "PIX_WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Friendly name for the workspace supplied with `--workspace`.
        #[arg(long, visible_alias = "name")]
        workspace_name: Option<String>,
        /// Do not start a pairing flow. Useful for preparing a host in CI.
        #[arg(long)]
        no_pair: bool,
        /// Do not install the platform user service after setup.
        #[arg(long)]
        no_service: bool,
        /// Accept setup prompts that have a safe default.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Never prompt; all required values must come from flags or the
        /// existing configuration.
        #[arg(long)]
        non_interactive: bool,
        /// Show extra local diagnostics while setup runs.
        #[arg(long)]
        verbose: bool,
    },
    /// Check local configuration and Pi RPC prerequisites.
    Doctor {
        /// Use a specific Pi executable instead of searching PATH.
        #[arg(long)]
        pi: Option<PathBuf>,
        /// Include local paths and implementation details useful for support.
        #[arg(long)]
        verbose: bool,
    },
    /// Manage explicitly authorized host folders.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Manage paired iOS devices.
    Device {
        #[command(subcommand)]
        command: DeviceCommand,
    },
    /// Remember or inspect the Pi executable used by this host.
    Pi {
        #[command(subcommand)]
        command: PiCommand,
    },
    /// Configure the encrypted relay used for remote access.
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
    /// Show the host service log location and its most recent entries.
    Logs {
        /// Number of trailing log lines to print.
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
    /// Show configuration and host-service runtime status.
    Status,
    /// Install, control, and inspect the platform user service.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Export a privacy-scrubbed diagnostic bundle.
    Diagnostics {
        #[command(subcommand)]
        command: DiagnosticsCommand,
    },
    /// Run the Bonjour-advertised host core until a quit command is received.
    Serve {
        /// Emit machine-readable JSONL events for a native UI bridge.
        #[arg(long)]
        json_events: bool,
        /// Run under a service manager. Lifecycle is controlled through the
        /// private local control socket instead of stdin.
        #[arg(long, hide = true)]
        service: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RelayCommand {
    /// Show the configured relay endpoint and whether it is active.
    Show,
    /// Set the relay WebSocket endpoint, e.g. `wss://relay.example.com`.
    Set { url: String },
    /// Remove the relay endpoint and stop using relay transport.
    Clear,
    /// Re-enable relay transport with the stored endpoint.
    Enable,
    /// Keep the endpoint but stop relay transport.
    Disable,
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// Authorize an existing folder on this host.
    Add {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// List authorized folders. Full paths are printed only on the host.
    List,
    /// Remove an explicitly authorized folder by ID, or choose one
    /// interactively when the ID is omitted.
    Remove { id: Option<uuid::Uuid> },
}

#[derive(Debug, Subcommand)]
enum DeviceCommand {
    /// Start the host in pairing mode and follow approval prompts.
    Pair,
    /// List paired phones. Public keys are never printed.
    List,
    /// Revoke a paired phone by ID, or choose one interactively when omitted.
    Revoke { id: Option<String> },
}

#[derive(Debug, Subcommand)]
enum PiCommand {
    /// Show the configured or auto-detected Pi executable.
    Show,
    /// Persist an explicit Pi executable for later `pix serve` launches.
    Set { path: PathBuf },
    /// Forget the saved Pi executable and return to PATH discovery.
    Clear,
}

#[derive(Debug, Subcommand)]
enum DiagnosticsCommand {
    /// Write a privacy-scrubbed `pix-diagnostics-*.tar.gz` bundle.
    Export {
        /// Destination file ending in `.tar.gz` or a directory to contain it.
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = match cli.config {
        Some(path) => path,
        None => ConfigStore::default_path().context("locating Pix configuration directory")?,
    };
    let store = ConfigStore::new(config_path);

    match cli.command {
        Command::Setup {
            relay,
            workspace,
            workspace_name,
            no_pair,
            no_service,
            yes,
            non_interactive,
            verbose,
        } => setup(
            &store,
            &SetupOptions {
                relay,
                workspace,
                workspace_name,
                no_pair,
                no_service,
                yes,
                non_interactive,
                verbose,
            },
        ),
        Command::Doctor { pi, verbose } => doctor(&store, pi, verbose),
        Command::Workspace { command } => workspace(&store, command),
        Command::Device { command } => device(&store, command),
        Command::Pi { command } => pi_command(&store, command),
        Command::Relay { command } => relay_command(&store, command),
        Command::Logs { tail } => show_logs(&store, tail),
        Command::Status => status_command(&store),
        Command::Service { command } => service::run(&store, &command),
        Command::Diagnostics { command } => diagnostics_command(&store, command),
        Command::Serve {
            json_events,
            service,
        } => serve(&store, json_events, service),
    }
}

/// Payload-free, append-only host service log.
///
/// Every service event, command error, and panic lands here with a
/// timestamp, so a dead or misbehaving host process leaves a trace even
/// when nothing was watching its stdout. Entries never contain prompts,
/// messages, keys, tokens, or channel secrets — the same rule as stdout
/// events.
#[derive(Clone)]
struct HostLog {
    path: PathBuf,
    file: std::sync::Arc<std::sync::Mutex<Option<std::fs::File>>>,
}

const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

impl HostLog {
    fn open(config_path: &std::path::Path) -> Self {
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

    fn path_for(config_path: &std::path::Path) -> PathBuf {
        config_path
            .parent()
            .map_or_else(|| PathBuf::from("logs"), |dir| dir.join("logs"))
            .join("host.jsonl")
    }

    /// Appends one structured entry; logging failures never disturb the
    /// host service.
    fn append(&self, kind: &str, body: &serde_json::Value) {
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

    fn append_text(&self, kind: &str, text: &str) {
        self.append(kind, &serde_json::Value::String(text.to_owned()));
    }

    fn rotate_if_needed(&self) {
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
    fn install_panic_hook(&self) {
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

fn open_log_file(path: &std::path::Path) -> Option<std::fs::File> {
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

fn show_logs(store: &ConfigStore, tail: usize) -> Result<()> {
    let path = HostLog::path_for(store.path());
    println!("log file: {}", path.display());
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(tail);
            for line in &lines[start..] {
                println!("{line}");
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("(no log entries yet)");
            Ok(())
        }
        Err(error) => Err(error).context("reading host log"),
    }
}

fn status_command(store: &ConfigStore) -> Result<()> {
    println!("Pix status");
    println!("  config: {}", store.path().display());
    match store.load() {
        Ok(config) => {
            println!("  host: {}", config.host.display_name);
            println!(
                "  host config: ok ({} workspace{}, {} paired device{})",
                config.workspaces.len(),
                plural(config.workspaces.len()),
                config.devices.len(),
                plural(config.devices.len())
            );
            match &config.preferences.relay_url {
                Some(url) if config.preferences.relay_enabled => {
                    println!("  relay: {url} (enabled)");
                }
                Some(url) => println!("  relay: {url} (disabled)"),
                None => println!("  relay: not configured"),
            }
            if let Some(pi) = &config.preferences.pi_executable {
                println!("  pi: configured ({})", pi.display());
            } else {
                println!("  pi: PATH discovery");
            }
        }
        Err(pix_core::config::ConfigError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            println!("  host config: not created yet");
        }
        Err(error) => bail!("host config: {error}"),
    }

    if let Some(current) = crate::status::HostServiceStatus::current(store.path()) {
        println!(
            "  service: running (pid {}, port {}, started_at {})",
            current.pid, current.port, current.started_at
        );
    } else {
        let installed = service::managed_service_installed(store).unwrap_or(false);
        let active = service::managed_service_active(store).unwrap_or(false);
        if active {
            println!("  service: manager active (host status is not ready yet)");
        } else if installed {
            println!("  service: installed but not running");
        } else {
            println!("  service: not running");
        }
    }
    Ok(())
}

fn diagnostics_command(store: &ConfigStore, command: DiagnosticsCommand) -> Result<()> {
    match command {
        DiagnosticsCommand::Export { path } => diagnostics::export_bundle(store, path),
    }
}

fn relay_command(store: &ConfigStore, command: RelayCommand) -> Result<()> {
    match command {
        RelayCommand::Show => {
            let config = store.load_or_create(default_host_name())?;
            match &config.preferences.relay_url {
                Some(url) if config.preferences.relay_enabled => {
                    println!("relay: {url} (enabled)");
                }
                Some(url) => println!("relay: {url} (disabled)"),
                None => println!("relay: not configured"),
            }
            Ok(())
        }
        RelayCommand::Set { url } => {
            let mut config = store.load_or_create(default_host_name())?;
            config.preferences.relay_url = Some(url.clone());
            config.preferences.relay_enabled = true;
            store.save(&config).context("saving Pix configuration")?;
            println!("relay: {url} (enabled)");
            Ok(())
        }
        RelayCommand::Clear => {
            let mut config = store.load().context("loading Pix configuration")?;
            config.preferences.relay_url = None;
            store.save(&config).context("saving Pix configuration")?;
            println!("relay: not configured");
            Ok(())
        }
        RelayCommand::Enable | RelayCommand::Disable => {
            let enable = matches!(command, RelayCommand::Enable);
            let mut config = store.load().context("loading Pix configuration")?;
            config.preferences.relay_enabled = enable;
            store.save(&config).context("saving Pix configuration")?;
            match (&config.preferences.relay_url, enable) {
                (Some(url), true) => println!("relay: {url} (enabled)"),
                (Some(url), false) => println!("relay: {url} (disabled)"),
                (None, _) => println!("relay: not configured"),
            }
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
struct ServeOutput {
    json_events: bool,
    stdout: bool,
}

#[allow(clippy::too_many_lines)]
fn serve(store: &ConfigStore, json_events: bool, service_mode: bool) -> Result<()> {
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
    let config = store
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
            emit_event(&serve_event, output, &log, &mut control);
            if pairing_waiting && let Some(pending) = pending_remote_pairing.take() {
                emit_event(
                    &ServeEvent::RemotePairingReady {
                        qr_payload: pending.qr_payload,
                        join_code: pending.join_code,
                        expires_at: pending.expires_at,
                    },
                    output,
                    &log,
                    &mut control,
                );
            } else if pairing_failed && pending_remote_pairing.take().is_some() {
                emit_command_error(
                    &anyhow::anyhow!("remote pairing channel failed to join the relay"),
                    output,
                    &log,
                    &mut control,
                );
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
                Ok(line) => incoming_commands.push(line),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    commands_disconnected = true;
                    break;
                }
            }
        }
        for line in incoming_commands {
            let mut words = line.split_whitespace();
            match words.next() {
                Some("approve") => {
                    if let Err(error) =
                        approve_pairing(&service, words.next(), output, &log, &mut control)
                    {
                        emit_command_error(&error, output, &log, &mut control);
                    } else if let Some(manager) = &relay {
                        sync_relay_devices(manager, store);
                    }
                }
                Some("reject") => {
                    if let Err(error) = reject_pairing(&service, words.next()) {
                        emit_command_error(&error, output, &log, &mut control);
                    }
                }
                Some("revoke") => {
                    if let Err(error) =
                        revoke_device(&service, words.next(), output, &log, &mut control)
                    {
                        emit_command_error(&error, output, &log, &mut control);
                    } else if let Some(manager) = &relay {
                        sync_relay_devices(manager, store);
                    }
                }
                Some("devices") => {
                    emit_devices(&service, output, &log, &mut control);
                }
                Some("sessions") => {
                    emit_sessions(&service, output, &log, &mut control);
                }
                Some("refresh") => {
                    if let Err(error) = service.refresh_config() {
                        emit_command_error(&anyhow::Error::new(error), output, &log, &mut control);
                    } else {
                        emit_devices(&service, output, &log, &mut control);
                        emit_sessions(&service, output, &log, &mut control);
                    }
                }
                Some("release") => {
                    if let Err(error) =
                        release_session(&service, words.next(), output, &log, &mut control)
                    {
                        emit_command_error(&error, output, &log, &mut control);
                    }
                }
                Some("pair-remote") => {
                    match prepare_remote_pairing(
                        relay.as_ref(),
                        relay_url.as_deref(),
                        &host_fingerprint,
                    ) {
                        Ok(pending) => pending_remote_pairing = Some(pending),
                        Err(error) => emit_command_error(&error, output, &log, &mut control),
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
                Some("quit" | "exit") => {
                    should_stop = true;
                    break;
                }
                _ => {}
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

fn load_host_identity(
    store: &ConfigStore,
    host_id: uuid::Uuid,
) -> Result<pix_core::host_identity::HostIdentityKey> {
    let identity_path = store
        .path()
        .parent()
        .context("locating host identity directory")?
        .join("host-identity.key");
    let identity_store = HostIdentityStore::new(identity_path);
    #[cfg(target_os = "macos")]
    let identity_store = if std::env::var("PIX_DISABLE_KEYCHAIN").is_ok_and(|value| value == "1") {
        identity_store
    } else {
        identity_store.with_keychain_host_id(host_id.to_string())
    };
    #[cfg(target_os = "linux")]
    let identity_store = identity_store.with_secret_service_host_id(host_id.to_string());
    identity_store.load_or_create().map_err(Into::into)
}

/// Materializes the small Pix context projection extension next to the host
/// config for the lifetime of `pix serve`. `pi -e` accepts a file path, while
/// embedding the source here keeps `cargo install` and app-bundled binaries
/// self-contained without requiring a second installed extension package.
fn create_pi_context_guard(directory: &std::path::Path) -> Result<NamedTempFile> {
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
fn sync_relay_devices(manager: &pix_core::RelayManager, store: &ConfigStore) {
    let Ok(config) = store.load() else { return };
    let _ = manager.sync_devices(
        config
            .devices
            .iter()
            .map(|device| (device.id.as_str(), device.relay_channel.as_str())),
    );
}

const REMOTE_PAIRING_TTL: Duration = Duration::from_secs(120);

struct PendingRemotePairing {
    qr_payload: String,
    join_code: String,
    expires_at: u64,
}

fn prepare_remote_pairing(
    relay: Option<&pix_core::RelayManager>,
    relay_url: Option<&str>,
    host_fingerprint: &str,
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
    })
}

/// RFC 3986 percent-encoding for QR query values; unreserved bytes pass.
fn percent_encode(value: &str) -> String {
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
enum ServeEvent {
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
    },
    SessionList {
        sessions: Vec<SessionEvent>,
    },
    SessionReleased {
        session_id: String,
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
    },
    CommandError {
        message: String,
    },
}

#[derive(Debug, Serialize)]
struct DeviceEvent {
    id: String,
    name: String,
    paired_at: String,
}

#[derive(Debug, Serialize)]
struct SessionEvent {
    id: String,
    workspace: String,
    clients: usize,
    state: &'static str,
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

/// Emits one service event to the log file (always) and stdout (best
/// effort).
///
/// A broken or stalled stdout pipe — the native app paused in a debugger,
/// or gone entirely — must never terminate or panic the host service. The
/// service exits through stdin EOF or `quit`, and the log file keeps the
/// record either way.
fn emit_event(
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
fn human_event(event: &ServeEvent) -> String {
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
        ServeEvent::SessionList { sessions } => {
            format!(
                "✓ {} active session{}\n",
                sessions.len(),
                plural(sessions.len())
            )
        }
        ServeEvent::SessionReleased { .. } => "✓ Session released\n".to_owned(),
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
        } => render_remote_pairing(qr_payload, join_code, *expires_at),
        ServeEvent::CommandError { message } => format!("✕ {message}\n"),
    }
}

fn format_confirmation_code(code: &str) -> String {
    if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

fn pairing_instructions(remote: bool) -> &'static str {
    if remote {
        "Open Pix on your iPhone and scan this QR code."
    } else {
        "Open Pix on your iPhone and choose this Mac from nearby hosts."
    }
}

fn terminal_label(value: &str) -> String {
    let mut label = value
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect::<String>();
    if label.is_empty() {
        label.push_str("device");
    }
    label
}

fn render_remote_pairing(qr_payload: &str, join_code: &str, expires_at: u64) -> String {
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

fn render_remote_pairing_for_ui(
    ui: SetupUi,
    qr_payload: &str,
    join_code: &str,
    expires_at: u64,
) -> String {
    let rendered = render_remote_pairing(qr_payload, join_code, expires_at);
    rendered.replace(join_code, &ui.cyan(join_code, true))
}

fn loggable_event(event: &ServeEvent) -> serde_json::Value {
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
        ServeEvent::SessionList { sessions } => serde_json::json!({
            "type": "session_list",
            "count": sessions.len(),
        }),
        ServeEvent::SessionReleased { .. } => serde_json::json!({"type": "session_released"}),
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

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
struct SetupOptions {
    relay: Option<String>,
    workspace: Option<PathBuf>,
    workspace_name: Option<String>,
    no_pair: bool,
    no_service: bool,
    yes: bool,
    non_interactive: bool,
    verbose: bool,
}

#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
struct SetupPairingOptions {
    remote: bool,
    yes: bool,
    interactive: bool,
    ui: SetupUi,
    keep_service: bool,
}

/// Runs the product-facing first-use flow while keeping the existing
/// subsystem commands available for diagnostics and automation.
fn setup(store: &ConfigStore, options: &SetupOptions) -> Result<()> {
    let interactive = !options.non_interactive
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal();
    if !interactive && !options.non_interactive {
        eprintln!(
            "Pix setup needs a terminal for prompts; pass --non-interactive with \
             --workspace and --no-pair, or run it from a terminal."
        );
    }
    if !interactive && !options.no_pair && !options.yes {
        bail!("non-interactive setup cannot approve a phone; pass --no-pair or --yes");
    }

    let ui = SetupUi::new(interactive, options.verbose);
    let config_was_present = store.path().is_file();
    let config = store
        .load_or_create(default_host_name())
        .context("loading Pix configuration")?;

    if config_was_present
        && setup_is_already_configured(store, &config)
        && !options.non_interactive
        && options.relay.is_none()
        && options.workspace.is_none()
        && !options.no_pair
        && !options.no_service
    {
        return setup_existing(store, config, options, ui);
    }

    let started_at = std::time::Instant::now();
    let mode =
        if interactive && options.relay.is_none() && options.workspace.is_none() && !options.yes {
            setup_welcome(ui)?
        } else {
            SetupMode::Quick
        };

    match mode {
        SetupMode::Quick => setup_quick(store, config, options, ui, started_at),
        SetupMode::Advanced => setup_advanced(store, config, options, ui, started_at),
        SetupMode::Exit => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupMode {
    Quick,
    Advanced,
    Exit,
}

fn setup_welcome(ui: SetupUi) -> Result<SetupMode> {
    ui.brand_header(None);
    ui.hint("Remote access for Pi");
    ui.body("Set up this computer so you can use Pi from your phone.");
    let options = vec![
        "Quick setup".to_owned(),
        "Advanced setup".to_owned(),
        "Exit".to_owned(),
    ];
    let selected = ui.select("How would you like to set up Pix?", &options, 0)?;
    Ok(match selected {
        1 => SetupMode::Advanced,
        2 => SetupMode::Exit,
        _ => SetupMode::Quick,
    })
}

fn setup_is_already_configured(store: &ConfigStore, config: &pix_core::HostConfig) -> bool {
    !config.workspaces.is_empty()
        || !config.devices.is_empty()
        || config.preferences.active_relay_url().is_some()
        || HostServiceStatus::current(store.path()).is_some()
}

#[allow(clippy::too_many_lines)]
fn setup_existing(
    store: &ConfigStore,
    config: pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
) -> Result<()> {
    ui.brand_header(None);
    ui.section("Pix is already set up on this computer");
    ui.success(
        &format!("Pi {}", configured_pi_version(store, &config)),
        None,
    );
    ui.success(
        &format!(
            "{} paired device{}",
            config.devices.len(),
            plural(config.devices.len())
        ),
        None,
    );
    ui.success(
        &format!(
            "{} workspace{}",
            config.workspaces.len(),
            plural(config.workspaces.len())
        ),
        None,
    );
    if let Some(relay) = config.preferences.active_relay_url() {
        ui.success("Relay configured", Some(&display_relay_url(relay)));
    } else {
        ui.muted("○ Local network only");
    }
    if HostServiceStatus::current(store.path()).is_some() {
        ui.success("Background service running", None);
    }

    if !ui.interactive() {
        return verify_setup(
            store,
            &config,
            ui,
            configured_pi_version(store, &config),
            config.preferences.active_relay_url().map(str::to_owned),
            false,
            std::time::Duration::ZERO,
        );
    }

    let choices = vec![
        "Check setup".to_owned(),
        "Pair another device".to_owned(),
        "Add workspace".to_owned(),
        "Reconfigure Pix".to_owned(),
        "Exit".to_owned(),
    ];
    let selected = ui.select("What would you like to do?", &choices, 0)?;
    match selected {
        1 => {
            let mut config = config;
            let relay = config.preferences.active_relay_url().map(str::to_owned);
            let _relay = run_setup_pairing_with_recovery(
                store,
                &mut config,
                relay,
                options.yes,
                true,
                ui,
                !options.no_service,
            )?;
            let service = install_setup_service(store, options.no_service, ui)?;
            let final_config = store.load().context("reloading setup configuration")?;
            verify_setup(
                store,
                &final_config,
                ui,
                configured_pi_version(store, &final_config),
                final_config
                    .preferences
                    .active_relay_url()
                    .map(str::to_owned),
                service,
                std::time::Duration::ZERO,
            )
        }
        2 => {
            let mut config = config;
            let mut existing_options = SetupOptions {
                relay: None,
                workspace: None,
                workspace_name: None,
                no_pair: true,
                no_service: true,
                yes: false,
                non_interactive: false,
                verbose: options.verbose,
            };
            configure_setup_workspace(&mut config, &existing_options, ui, true, true)?;
            store.save(&config).context("saving setup configuration")?;
            existing_options.no_pair = false;
            existing_options.no_service = false;
            verify_setup(
                store,
                &config,
                ui,
                configured_pi_version(store, &config),
                config.preferences.active_relay_url().map(str::to_owned),
                false,
                std::time::Duration::ZERO,
            )
        }
        3 => {
            let mut config = config;
            let relay = configure_setup_relay(&mut config, options, ui, true)?;
            configure_setup_workspace(&mut config, options, ui, true, true)?;
            store.save(&config).context("saving setup configuration")?;
            verify_setup(
                store,
                &config,
                ui,
                configured_pi_version(store, &config),
                relay,
                false,
                std::time::Duration::ZERO,
            )
        }
        0 => verify_setup(
            store,
            &config,
            ui,
            configured_pi_version(store, &config),
            config.preferences.active_relay_url().map(str::to_owned),
            false,
            std::time::Duration::ZERO,
        ),
        _ => Ok(()),
    }
}

fn setup_quick(
    store: &ConfigStore,
    mut config: pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
    started_at: std::time::Instant,
) -> Result<()> {
    if ui.interactive() {
        ui.crumb_header("Setup");
        ui.section("Checking this computer");
    }
    let pi_version = prepare_setup_environment(store, &mut config, options, ui)?;
    let relay = configure_setup_relay(&mut config, options, ui, false)?;
    configure_setup_workspace(&mut config, options, ui, ui.interactive(), false)?;
    store.save(&config).context("saving setup configuration")?;

    let relay = if config.devices.is_empty() && !options.no_pair {
        run_setup_pairing_with_recovery(
            store,
            &mut config,
            relay,
            options.yes,
            ui.interactive(),
            ui,
            !options.no_service,
        )?
    } else if config.devices.is_empty() {
        if ui.interactive() {
            ui.muted("○ Device pairing skipped");
        } else {
            println!("Pairing... skipped");
        }
        relay
    } else if ui.interactive() {
        ui.success(
            &format!(
                "{} paired device{} already configured",
                config.devices.len(),
                plural(config.devices.len())
            ),
            None,
        );
        relay
    } else {
        println!(
            "Pairing... {} device{} already configured",
            config.devices.len(),
            plural(config.devices.len())
        );
        relay
    };

    let service = install_setup_service(store, options.no_service, ui)?;
    let final_config = store.load().context("reloading setup configuration")?;
    verify_setup(
        store,
        &final_config,
        ui,
        pi_version,
        relay,
        service,
        started_at.elapsed(),
    )
}

fn prepare_setup_environment(
    store: &ConfigStore,
    config: &mut pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
) -> Result<String> {
    let environment = HostEnvironment::resolve_for("pi");
    loop {
        if ui.interactive() {
            ui.task("Looking for Pi...");
        } else {
            ui.task("Checking Pi");
        }
        let result = PiProbe::new(config.preferences.pi_executable.clone())
            .with_environment(environment.clone())
            .inspect();
        match result {
            Ok(installation) if installation.supported => {
                ui.task_done(&format!("Pi {}", installation.version));
                if options.verbose {
                    ui.hint(&format!(
                        "Executable: {}",
                        installation.executable.display()
                    ));
                }
                if ui.interactive() {
                    ui.task("Preparing host identity...");
                } else {
                    ui.task("Host identity");
                }
                let _identity =
                    load_host_identity(store, config.host.id).context("preparing host identity")?;
                ui.task_done("Host identity ready");
                if options.verbose {
                    ui.hint(&format!(
                        "Identity store: {}",
                        host_identity_path(store).display()
                    ));
                }
                return Ok(installation.version.to_string());
            }
            Ok(installation) => {
                ui.task_failed(&format!("Pi {} is not supported", installation.version));
                if !ui.interactive() {
                    bail!(
                        "Pi {} is outside the currently verified range {}",
                        installation.version,
                        pix_core::pi::SUPPORTED_PI_VERSION
                    );
                }
                ui.error(
                    "This Pix build supports a different Pi version",
                    Some(pix_core::pi::SUPPORTED_PI_VERSION),
                );
            }
            Err(error) => {
                ui.task_failed("Pi was not found");
                if !ui.interactive() {
                    return Err(anyhow::Error::new(error).context("checking Pi"));
                }
                let not_found = matches!(&error, pix_core::pi::PiError::NotFound);
                if not_found {
                    ui.error(
                        "Pi was not found",
                        Some(
                            "Make sure `pi` is available in your PATH, or select an executable manually.",
                        ),
                    );
                } else {
                    ui.error(
                        "Pix couldn't verify Pi",
                        Some("Run `pix doctor --verbose` for more details."),
                    );
                }
            }
        }

        let choices = vec![
            "Try again".to_owned(),
            "Choose Pi executable".to_owned(),
            "Exit".to_owned(),
        ];
        match ui.select("Pi setup", &choices, 0)? {
            1 => {
                let current = config
                    .preferences
                    .pi_executable
                    .as_deref()
                    .map(|path| path.to_string_lossy().into_owned());
                let path = ui.input("Pi executable", current.as_deref())?;
                if path.trim().is_empty() {
                    ui.warning("Pi executable is required", None);
                } else {
                    config.preferences.pi_executable = Some(PathBuf::from(path));
                }
            }
            2 => bail!("setup cancelled"),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_lines)]
fn setup_advanced(
    store: &ConfigStore,
    mut config: pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
    started_at: std::time::Instant,
) -> Result<()> {
    ui.crumb_header("Advanced setup");

    let host_name = ui.input("Host name", Some(&config.host.display_name))?;
    if !host_name.trim().is_empty() {
        config.host.display_name = host_name;
    }

    ui.section("Pi");
    let mut pi_choices = vec!["Auto-detect from PATH".to_owned()];
    if let Some(path) = &config.preferences.pi_executable {
        pi_choices.push(path.display().to_string());
    }
    pi_choices.push("Choose another executable...".to_owned());
    let pi_choice = ui.select("Pi executable", &pi_choices, 0)?;
    if pi_choice == pi_choices.len() - 1 {
        let current_pi = config
            .preferences
            .pi_executable
            .as_deref()
            .map(|value| value.to_string_lossy().into_owned());
        let path = ui.input("Pi executable", current_pi.as_deref())?;
        if !path.trim().is_empty() {
            config.preferences.pi_executable = Some(PathBuf::from(path));
        }
    } else if pi_choice == 0 {
        config.preferences.pi_executable = None;
    }

    ui.section("Connectivity");
    let connectivity = ui.multiselect(
        "Choose connection methods:",
        &[
            "Local network".to_owned(),
            "Relay".to_owned(),
            "Custom direct endpoint".to_owned(),
        ],
        &[true, true, false],
    )?;
    if connectivity.get(2).copied().unwrap_or(false) {
        ui.warning(
            "Custom direct endpoints are not available yet",
            Some("Pix will continue with the selected local and relay methods."),
        );
    }
    let relay = if connectivity.get(1).copied().unwrap_or(false) {
        let default = config
            .preferences
            .active_relay_url()
            .unwrap_or(DEFAULT_RELAY_URL);
        let value = ui.input("Relay URL", Some(default))?;
        let relay = validate_relay_url(&value)?;
        config.preferences.relay_url = Some(relay.clone());
        config.preferences.relay_enabled = true;
        Some(relay)
    } else {
        config.preferences.relay_enabled = false;
        None
    };

    ui.section("Workspace access");
    let candidates = workspace_candidates();
    let mut workspace_options = candidates
        .iter()
        .map(|(path, label)| format!("{}  {}", display_workspace_path(path), label))
        .collect::<Vec<_>>();
    workspace_options.push("Add another path...".to_owned());
    let mut defaults = vec![false; workspace_options.len()];
    if !candidates.is_empty() {
        defaults[0] = true;
    }
    let selected = ui.multiselect(
        "Select folders Pix can access:",
        &workspace_options,
        &defaults,
    )?;
    for (index, checked) in selected.iter().enumerate() {
        if !checked {
            continue;
        }
        if index == candidates.len() {
            let path = select_workspace_path(ui)?;
            add_workspace(&mut config, path, None, ui)?;
        } else if let Some((path, _)) = candidates.get(index) {
            add_workspace(&mut config, path.clone(), None, ui)?;
        }
    }
    if config.workspaces.is_empty() {
        let path = select_workspace_path(ui)?;
        add_workspace(&mut config, path, None, ui)?;
    }

    ui.section("Background service");
    let service_choices = vec![
        "Yes, recommended".to_owned(),
        "No, I'll run Pix manually".to_owned(),
    ];
    let install_service = !options.no_service
        && ui.select(
            "Start Pix automatically when this computer boots?",
            &service_choices,
            0,
        )? == 0;

    ui.section("Review");
    ui.hint(&format!("Host\n  {}", config.host.display_name));
    ui.hint(&format!(
        "Pi\n  {}",
        config
            .preferences
            .pi_executable
            .as_deref()
            .map_or("Auto-detect from PATH".to_owned(), |path| path
                .display()
                .to_string())
    ));
    ui.hint(&format!(
        "Connectivity\n  {}",
        if relay.is_some() {
            "Local network\n  Pix Relay"
        } else {
            "Local network"
        }
    ));
    ui.hint(&format!(
        "Workspaces\n  {}",
        config
            .workspaces
            .iter()
            .map(|item| display_workspace_path(&item.path))
            .collect::<Vec<_>>()
            .join("\n  ")
    ));
    ui.hint(&format!(
        "Background service\n  {}",
        if install_service {
            "Enabled"
        } else {
            "Disabled"
        }
    ));
    let review_choices = vec![
        "Continue".to_owned(),
        "Go back".to_owned(),
        "Cancel".to_owned(),
    ];
    match ui.select("", &review_choices, 0)? {
        1 => {
            let mut quick_options = options.clone();
            quick_options.no_service = !install_service;
            return setup_quick(store, config, &quick_options, ui, started_at);
        }
        2 => bail!("setup cancelled"),
        _ => {}
    }

    if ui.interactive() {
        ui.section("Checking this computer");
    }
    let pi_version = prepare_setup_environment(store, &mut config, options, ui)?;
    store.save(&config).context("saving setup configuration")?;
    let relay = if config.devices.is_empty() && !options.no_pair {
        run_setup_pairing_with_recovery(
            store,
            &mut config,
            relay.clone(),
            options.yes,
            true,
            ui,
            install_service,
        )?
    } else {
        relay
    };
    let service = if install_service {
        install_setup_service(store, false, ui)?
    } else {
        install_setup_service(store, true, ui)?
    };
    let final_config = store.load().context("reloading setup configuration")?;
    verify_setup(
        store,
        &final_config,
        ui,
        pi_version,
        relay,
        service,
        started_at.elapsed(),
    )
}

#[allow(clippy::needless_pass_by_value)]
fn add_workspace(
    config: &mut pix_core::HostConfig,
    path: PathBuf,
    name: Option<String>,
    ui: SetupUi,
) -> Result<Option<String>> {
    let canonical = std::fs::canonicalize(&path)
        .with_context(|| format!("resolving workspace {}", path.display()))?;
    if let Some(existing) = config
        .workspaces
        .iter()
        .find(|workspace| workspace.path == canonical)
    {
        return Ok(Some(display_workspace_path(&existing.path)));
    }
    let mut registry = WorkspaceRegistry::new(config);
    let added = registry
        .add(&canonical, name)
        .with_context(|| format!("authorizing workspace {}", path.display()))?
        .clone();
    let displayed = display_workspace_path(&added.path);
    if ui.interactive() {
        ui.success("Added workspace", Some(&displayed));
    }
    Ok(Some(displayed))
}

fn configure_setup_relay(
    config: &mut pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
    reconfigure: bool,
) -> Result<Option<String>> {
    if let Some(url) = options.relay.as_deref() {
        let relay = validate_relay_url(url)?;
        config.preferences.relay_url = Some(relay.clone());
        config.preferences.relay_enabled = true;
        if ui.interactive() {
            ui.success("Pix Relay", Some(&display_relay_url(&relay)));
        } else {
            println!("Relay... ok");
        }
        return Ok(Some(relay));
    }

    if !reconfigure && let Some(url) = config.preferences.active_relay_url() {
        let relay = url.to_owned();
        if ui.interactive() {
            ui.section("Remote access");
            ui.success("Pix Relay", Some(&display_relay_url(&relay)));
        } else {
            println!("Relay... ok");
        }
        return Ok(Some(relay));
    }

    if !ui.interactive() {
        config.preferences.relay_enabled = false;
        println!("Relay... skipped");
        return Ok(None);
    }

    ui.section("Remote access");
    ui.body("How should Pix reach this computer when you're away?");
    let choices = vec![
        "Pix Relay                     Recommended".to_owned(),
        "Local network only".to_owned(),
        "Custom relay".to_owned(),
    ];
    let selected = ui.select("", &choices, 0)?;
    let relay = match selected {
        1 => {
            config.preferences.relay_enabled = false;
            None
        }
        2 => loop {
            let value = ui.input("Relay URL", None)?;
            match validate_relay_url(&value) {
                Ok(url) => break Some(url),
                Err(_) => ui.error(
                    "Pix relay URL is invalid",
                    Some("Use wss:// for a relay, or ws:// for a local endpoint."),
                ),
            }
        },
        _ => Some(DEFAULT_RELAY_URL.to_owned()),
    };
    if let Some(url) = &relay {
        config.preferences.relay_url = Some(url.clone());
        config.preferences.relay_enabled = true;
        ui.success("Pix Relay selected", Some(&display_relay_url(url)));
    } else {
        ui.success("Local network only", None);
    }
    Ok(relay)
}

fn validate_relay_url(url: &str) -> Result<String> {
    let value = url.trim();
    if value.is_empty() || !(value.starts_with("ws://") || value.starts_with("wss://")) {
        bail!("relay URL must start with ws:// or wss://");
    }
    if value.chars().any(char::is_whitespace) {
        bail!("relay URL cannot contain whitespace");
    }
    Ok(value.to_owned())
}

fn configure_setup_workspace(
    config: &mut pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
    interactive: bool,
    reconfigure: bool,
) -> Result<Option<String>> {
    let needs_workspace =
        options.workspace.is_some() || config.workspaces.is_empty() || reconfigure;
    if !needs_workspace {
        let workspace = config
            .workspaces
            .first()
            .map(|item| display_workspace_path(&item.path));
        if interactive {
            ui.section("Workspace access");
            ui.success(
                &format!(
                    "{} authorized workspace{}",
                    config.workspaces.len(),
                    plural(config.workspaces.len())
                ),
                workspace.as_deref(),
            );
        } else {
            println!("Workspace... ok");
        }
        return Ok(workspace);
    }

    let path = match options.workspace.clone() {
        Some(path) => expand_home(path),
        None if interactive => select_workspace_path(ui)?,
        None => bail!("setup needs --workspace when no authorized workspace exists"),
    };
    let canonical = std::fs::canonicalize(&path)
        .with_context(|| format!("resolving workspace {}", path.display()))?;
    if let Some(existing) = config
        .workspaces
        .iter()
        .find(|workspace| workspace.path == canonical)
    {
        if interactive {
            ui.success(
                "Workspace already authorized",
                Some(&display_workspace_path(&existing.path)),
            );
        } else {
            println!("Workspace... ok");
        }
        return Ok(Some(display_workspace_path(&existing.path)));
    }
    let name = options.workspace_name.clone();
    let mut registry = WorkspaceRegistry::new(config);
    let added = registry
        .add(&canonical, name)
        .with_context(|| format!("authorizing workspace {}", path.display()))?
        .clone();
    let displayed = display_workspace_path(&added.path);
    if interactive {
        ui.success("Added workspace", Some(&displayed));
    } else {
        println!("Workspace... ok");
    }
    Ok(Some(displayed))
}

fn select_workspace_path(ui: SetupUi) -> Result<PathBuf> {
    let candidates = workspace_candidates();
    let mut options = candidates
        .iter()
        .map(|(path, label)| format!("{:<36} {}", display_workspace_path(path), label))
        .collect::<Vec<_>>();
    options.push("Enter another path...".to_owned());
    let selected = ui.select("Choose your first workspace:", &options, 0)?;
    if selected < candidates.len() {
        return Ok(candidates[selected].0.clone());
    }
    loop {
        let value = ui.input("Workspace path", None)?;
        let path = expand_home(PathBuf::from(value));
        match std::fs::canonicalize(&path) {
            Ok(path) if path.is_dir() => return Ok(path),
            Ok(_) => ui.error("Workspace must be a folder", None),
            Err(_) => ui.error(
                "Workspace path was not found",
                Some("Choose an existing folder and try again."),
            ),
        }
    }
}

fn workspace_candidates() -> Vec<(PathBuf, &'static str)> {
    let mut candidates = Vec::new();
    let mut add = |path: PathBuf, label: &'static str| {
        let Ok(canonical) = std::fs::canonicalize(path) else {
            return;
        };
        if !canonical.is_dir() || candidates.iter().any(|(item, _)| item == &canonical) {
            return;
        }
        candidates.push((canonical, label));
    };
    if let Ok(current) = std::env::current_dir() {
        let git_root = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&current)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| PathBuf::from(value.trim()));
        if let Some(root) = git_root {
            add(root, "git repository");
        }
        add(current.clone(), "current directory");
        if let Some(parent) = current.parent() {
            add(parent.to_path_buf(), "parent directory");
        }
    }
    if let Some(home) = home_directory() {
        add(home, "home directory");
    }
    candidates
}

fn expand_home(path: PathBuf) -> PathBuf {
    if path == std::path::Path::new("~") {
        return home_directory().unwrap_or(path);
    }
    if let Some(rest) = path.to_str().and_then(|value| value.strip_prefix("~/")) {
        return home_directory().map(|home| home.join(rest)).unwrap_or(path);
    }
    path
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn display_workspace_path(path: &std::path::Path) -> String {
    if let Some(home) = home_directory()
        && let Ok(relative) = path.strip_prefix(&home)
    {
        if relative.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~/{}", relative.display());
    }
    path.display().to_string()
}

fn display_relay_url(url: &str) -> String {
    url.strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))
        .unwrap_or(url)
        .to_owned()
}

fn host_identity_path(store: &ConfigStore) -> PathBuf {
    store.path().parent().map_or_else(
        || PathBuf::from("host-identity.key"),
        |dir| dir.join("host-identity.key"),
    )
}

fn configured_pi_version(_store: &ConfigStore, config: &pix_core::HostConfig) -> String {
    PiProbe::new(config.preferences.pi_executable.clone())
        .with_environment(HostEnvironment::resolve_for("pi"))
        .inspect()
        .map_or_else(
            |_| "unavailable".to_owned(),
            |installation| installation.version.to_string(),
        )
}

#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn verify_setup(
    store: &ConfigStore,
    config: &pix_core::HostConfig,
    ui: SetupUi,
    pi_version: String,
    relay: Option<String>,
    service_installed: bool,
    elapsed: std::time::Duration,
) -> Result<()> {
    if !ui.interactive() {
        println!("Verifying setup... ok");
        println!("Setup complete.");
        return Ok(());
    }

    ui.crumb_header("Setup");
    ui.section("Verifying setup");
    ui.task("Checking host service...");
    let running = service_installed || HostServiceStatus::current(store.path()).is_some();
    if running {
        ui.task_done("Host service running");
    } else {
        ui.task_failed("Host service not running");
        ui.warning(
            "You can still run Pix manually",
            Some("Start it with `pix serve`."),
        );
    }
    ui.task("Checking connection...");
    if relay.is_some() {
        ui.task_done("Relay configured");
    } else {
        ui.task_done("Local network ready");
    }
    if config.devices.is_empty() {
        ui.muted("○ iPhone offline");
    } else {
        ui.success(
            &format!(
                "{} paired device{}",
                config.devices.len(),
                plural(config.devices.len())
            ),
            None,
        );
    }

    ui.brand_header(None);
    ui.success("Setup complete", None);
    ui.body("This computer is ready for remote Pi access.");
    println!();
    ui.hint("Host");
    println!(
        "  {}",
        if running {
            ui.green("● Online", false)
        } else {
            ui.paint("○ Offline", "\x1b[2m", false)
        }
    );
    ui.hint("Device");
    let device = config.devices.first().map_or_else(
        || "○ Offline".to_owned(),
        |item| format!("✓ {}", terminal_label(&item.name)),
    );
    println!("  {}", ui.paint(&device, "\x1b[97m", false));
    ui.hint("Workspace");
    let workspace = config.workspaces.first().map_or_else(
        || "None".to_owned(),
        |item| display_workspace_path(&item.path),
    );
    println!("  {}", ui.paint(&workspace, "\x1b[97m", false));
    ui.hint("Remote access");
    println!(
        "  {}",
        ui.paint(
            &relay
                .as_deref()
                .map_or("Local network only".to_owned(), |url| {
                    format!("✓ Pix Relay ({})", display_relay_url(url))
                }),
            "\x1b[97m",
            false
        )
    );
    println!();
    ui.body("Open Pix on your iPhone to start.");
    println!();
    ui.hint("Useful commands");
    println!("    pix status       Check this host");
    println!("    pix pair         Pair another device");
    println!("    pix workspace    Manage workspaces");
    println!();
    let seconds = elapsed.as_secs();
    ui.hint(&format!("Pi {pi_version}  •  Done in {seconds}s"));
    Ok(())
}

fn prompt_line(label: &str, default: &str) -> Result<String> {
    print!("› {label}: ");
    std::io::stdout().flush().context("flushing setup prompt")?;
    let mut line = String::new();
    let read = std::io::stdin()
        .read_line(&mut line)
        .context("reading setup input")?;
    if read == 0 {
        if default.is_empty() {
            bail!("setup input ended before completing the prompt");
        }
        return Ok(default.to_owned());
    }
    let value = line.trim();
    if value.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(value.to_owned())
    }
}

/// Attaches the interactive pairing flow to the already-running persistent
/// host service. The service remains available to the macOS menu app and other
/// clients throughout pairing; approving a request only updates durable host
/// state and does not restart Bonjour or the encrypted transport.
#[allow(clippy::too_many_lines)]
fn run_setup_pairing(store: &ConfigStore, pairing: SetupPairingOptions) -> Result<()> {
    let SetupPairingOptions {
        remote,
        yes,
        interactive,
        ui,
        keep_service,
    } = pairing;
    #[cfg(not(unix))]
    {
        let _ = (store, remote, yes, interactive, ui, keep_service);
        bail!("interactive pairing is currently supported on Unix hosts");
    }
    #[cfg(unix)]
    let service_was_running = HostServiceStatus::current(store.path()).is_some();
    #[cfg(unix)]
    let event_stream = {
        service::ensure_running(store)?;
        let stream = service::connect_events(store)
            .context("connecting to the running Pix host event stream")?;
        // Ask the persistent service to replay current state after this
        // subscriber is attached. This also covers a request created just
        // before the CLI connected.
        service::send_command(store, "pending")?;
        if remote {
            service::send_command(store, "pair-remote")?;
        }
        stream
    };
    #[cfg(unix)]
    let mut events = std::io::BufReader::new(event_stream);
    #[cfg(unix)]
    let mut line = String::new();
    #[cfg(unix)]
    let mut approved_device: Option<String> = None;
    #[cfg(unix)]
    let mut approved_connection: Option<String> = None;

    if interactive {
        ui.crumb_header("Setup");
        ui.section("Pair your phone");
        ui.body(pairing_instructions(remote));
        if !remote {
            ui.hint("Keep your iPhone and Mac on the same network and allow local discovery.");
            ui.hint("To pair by QR code instead, configure Pix Relay first.");
        }
    }
    let mut waiting_task = true;
    if remote {
        ui.task("Preparing secure pairing...");
    } else {
        ui.task("Waiting for your phone...");
    }

    #[cfg(unix)]
    loop {
        line.clear();
        let bytes = events
            .read_line(&mut line)
            .context("reading setup host events")?;
        if bytes == 0 {
            break;
        }
        let event: serde_json::Value =
            serde_json::from_str(line.trim()).context("decoding setup host event")?;
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("remote_pairing_ready") => {
                let payload = event
                    .get("qr_payload")
                    .and_then(serde_json::Value::as_str)
                    .context("setup host omitted QR payload")?;
                let join_code = event
                    .get("join_code")
                    .and_then(serde_json::Value::as_str)
                    .context("setup host omitted pairing code")?;
                let expires_at = event
                    .get("expires_at")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default();
                ui.task_done("Secure pairing ready");
                print!(
                    "{}",
                    render_remote_pairing_for_ui(ui, payload, join_code, expires_at)
                );
                std::io::stdout().flush().ok();
            }
            Some("pairing_requested") => {
                let id = event
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .context("setup host omitted pairing request ID")?;
                let device_name = event
                    .get("device_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Your phone")
                    .to_owned();
                let name = terminal_label(&device_name);
                let code = event
                    .get("confirmation_code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if std::mem::replace(&mut waiting_task, false) {
                    ui.task_done(&format!("{name} found"));
                }
                ui.section(&format!("{name} wants to pair"));
                ui.body("Confirm that this code matches the one on your phone:");
                println!(
                    "                 {}",
                    ui.cyan(&format_confirmation_code(code), true)
                );
                let approve = if yes {
                    true
                } else if interactive {
                    let choices = vec![format!("Pair {name}"), "Reject".to_owned()];
                    ui.select("", &choices, 0)? == 0
                } else {
                    bail!("pairing requires --yes when setup is non-interactive")
                };
                service::send_command(
                    store,
                    &format!("{} {id}", if approve { "approve" } else { "reject" }),
                )?;
                if approve {
                    // The service stays alive after approval. The phone can
                    // finish its authenticated snapshot/IK exchange without
                    // racing a parent process shutdown.
                    if keep_service {
                        ui.success("iPhone pairing approved", None);
                        return Ok(());
                    }
                    approved_device = Some(device_name);
                    ui.task("Finishing secure pairing...");
                } else {
                    ui.warning("Pairing rejected", Some("Waiting for another device."));
                    waiting_task = true;
                    ui.task("Waiting for your phone...");
                }
            }
            Some("connection_established") if !keep_service => {
                if approved_device.as_deref()
                    == event.get("device_name").and_then(serde_json::Value::as_str)
                {
                    approved_connection = event
                        .get("connection_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                }
            }
            Some("connection_closed") if !keep_service => {
                if approved_connection.as_deref()
                    == event
                        .get("connection_id")
                        .and_then(serde_json::Value::as_str)
                {
                    if !service_was_running {
                        service::stop(store).context("stopping the temporary setup service")?;
                    }
                    ui.success("iPhone paired", None);
                    return Ok(());
                }
            }
            Some("command_error") => {
                let message = event
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("setup host command failed");
                bail!("{message}");
            }
            Some("connection_failed") => {
                ui.warning(
                    "Device connection failed",
                    Some("Waiting for another attempt."),
                );
            }
            _ => {}
        }
    }

    #[cfg(unix)]
    bail!("Pix host event stream closed before pairing a device")
}

/// Relay setup is recoverable. A failed remote channel can be retried, moved
/// to another endpoint, or downgraded to LAN pairing without discarding the
/// workspace and host identity work that already completed.
#[allow(clippy::too_many_lines)]
fn run_setup_pairing_with_recovery(
    store: &ConfigStore,
    config: &mut pix_core::HostConfig,
    mut relay: Option<String>,
    yes: bool,
    interactive: bool,
    ui: SetupUi,
    keep_service: bool,
) -> Result<Option<String>> {
    loop {
        match run_setup_pairing(
            store,
            SetupPairingOptions {
                remote: relay.is_some(),
                yes,
                interactive,
                ui,
                keep_service,
            },
        ) {
            Ok(()) => return Ok(relay),
            Err(_error) if relay.is_some() && interactive => {
                let relay_label = relay
                    .as_deref()
                    .map_or_else(|| "configured relay".to_owned(), display_relay_url);
                ui.error("Couldn't connect to the relay", Some(&relay_label));
                let choices = vec![
                    "Try again".to_owned(),
                    "Change relay".to_owned(),
                    "Continue with local network only".to_owned(),
                    "Exit".to_owned(),
                ];
                match ui.select("Remote pairing", &choices, 0)? {
                    1 => loop {
                        let value = ui.input("Relay URL", relay.as_deref())?;
                        match validate_relay_url(&value) {
                            Ok(url) => {
                                config.preferences.relay_url = Some(url.clone());
                                config.preferences.relay_enabled = true;
                                store.save(config).context("saving relay configuration")?;
                                relay = Some(url);
                                break;
                            }
                            Err(_) => ui.error(
                                "Pix relay URL is invalid",
                                Some("Use wss:// for a relay, or ws:// for a local endpoint."),
                            ),
                        }
                    },
                    2 => {
                        config.preferences.relay_enabled = false;
                        store
                            .save(config)
                            .context("saving local network preference")?;
                        relay = None;
                    }
                    3 => bail!("setup cancelled"),
                    _ => {}
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[allow(clippy::unnecessary_wraps)]
fn install_setup_service(store: &ConfigStore, no_service: bool, ui: SetupUi) -> Result<bool> {
    let _ = ui.verbose();
    if no_service {
        if ui.interactive() {
            ui.muted("○ Background service skipped (--no-service)");
        } else {
            println!("Background service... skipped");
        }
        return Ok(false);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        loop {
            ui.task("Installing background service...");
            match service::install_for_setup(store) {
                Ok(unit_path) => {
                    ui.task_done("Background service installed");
                    if ui.verbose() {
                        ui.hint(&format!("Service: {}", unit_path.display()));
                    }
                    ui.task("Starting Pix...");
                    ui.task_done("Pix is running");
                    return Ok(true);
                }
                Err(error) if ui.interactive() => {
                    ui.task_failed("Background service installation failed");
                    ui.warning(
                        "Pix couldn't install the background service",
                        Some("Pairing and workspace setup are complete. You can still run Pix manually."),
                    );
                    let choices = vec![
                        "Continue without background service".to_owned(),
                        "Try again".to_owned(),
                        "Show details".to_owned(),
                    ];
                    match ui.select("", &choices, 0)? {
                        1 => {}
                        2 => {
                            ui.hint(&format!("{error:#}"));
                            return Ok(false);
                        }
                        _ => return Ok(false),
                    }
                }
                Err(error) => {
                    ui.task_failed("Background service installation failed");
                    if ui.verbose() {
                        ui.hint(&format!("{error:#}"));
                    }
                    return Ok(false);
                }
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = store;
        if ui.interactive() {
            ui.muted("○ Background service installation is available on Linux and macOS");
        } else {
            println!("Background service... unavailable on this platform");
        }
        Ok(false)
    }
}

fn doctor(store: &ConfigStore, pi: Option<PathBuf>, verbose: bool) -> Result<()> {
    println!("Pix doctor");
    println!("  config: {}", store.path().display());
    match store.load() {
        Ok(config) => println!(
            "  host config: ok ({} workspace{}, {} paired device{})",
            config.workspaces.len(),
            plural(config.workspaces.len()),
            config.devices.len(),
            plural(config.devices.len())
        ),
        Err(pix_core::config::ConfigError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            println!("  host config: not created yet");
        }
        Err(error) => bail!("host config: {error}"),
    }

    let environment = HostEnvironment::resolve_for("pi");
    println!("  environment: {}", environment.describe());
    println!("  PATH entries: {}", environment.path_entry_count());
    let installation = PiProbe::new(pi)
        .with_environment(environment)
        .inspect()
        .context("probing Pi (run `pix pi set <path>` to pin a specific executable)")?;
    println!("  pi executable: {}", installation.executable.display());
    println!("  pi version: {}", installation.version);
    if verbose {
        println!(
            "  host identity store: {}",
            host_identity_path(store).display()
        );
    }
    if installation.supported {
        println!("  pi RPC compatibility: verified");
        Ok(())
    } else {
        bail!(
            "Pi {} is outside the currently verified range {}",
            installation.version,
            pix_core::pi::SUPPORTED_PI_VERSION
        )
    }
}

fn approve_pairing(
    service: &pix_core::HostServiceHandle,
    request: Option<&str>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) -> Result<()> {
    let request = request.context("approve requires request ID")?;
    service
        .approve(uuid::Uuid::parse_str(request).context("invalid request ID")?)
        .context("approving pairing request")?;
    emit_devices(service, output, log, control);
    Ok(())
}

fn reject_pairing(service: &pix_core::HostServiceHandle, request: Option<&str>) -> Result<()> {
    let request = request.context("reject requires request ID")?;
    service
        .reject(uuid::Uuid::parse_str(request).context("invalid request ID")?)
        .context("rejecting pairing request")?;
    Ok(())
}

fn revoke_device(
    service: &pix_core::HostServiceHandle,
    device_id: Option<&str>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) -> Result<()> {
    let device_id = device_id.context("revoke requires device ID")?;
    let revoked = service
        .revoke_device(device_id)
        .context("revoking paired device")?;
    emit_event(
        &ServeEvent::DeviceRevoked {
            device_id: revoked.id,
            device_name: revoked.name,
        },
        output,
        log,
        control,
    );
    emit_devices(service, output, log, control);
    Ok(())
}

fn release_session(
    service: &pix_core::HostServiceHandle,
    session_id: Option<&str>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) -> Result<()> {
    let session_id = session_id.context("release requires session ID")?;
    service
        .release_session(session_id)
        .context("releasing session")?;
    emit_event(
        &ServeEvent::SessionReleased {
            session_id: session_id.to_owned(),
        },
        output,
        log,
        control,
    );
    emit_sessions(service, output, log, control);
    Ok(())
}

fn emit_command_error(
    error: &anyhow::Error,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) {
    emit_event(
        &ServeEvent::CommandError {
            message: format!("{error:#}"),
        },
        output,
        log,
        control,
    );
}

fn emit_devices(
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

fn emit_sessions(
    service: &pix_core::HostServiceHandle,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) {
    let sessions = service
        .active_sessions()
        .into_iter()
        .map(|session| SessionEvent {
            id: session.session_id.to_string(),
            workspace: session.workspace.display().to_string(),
            clients: session.client_count,
            state: if session.completed { "idle" } else { "running" },
        })
        .collect();
    emit_event(&ServeEvent::SessionList { sessions }, output, log, control);
}

fn configured_pi_executable(
    config: &pix_core::HostConfig,
    environment: &HostEnvironment,
) -> PathBuf {
    config.preferences.pi_executable.clone().unwrap_or_else(|| {
        PiProbe::new(None)
            .with_environment(environment.clone())
            .inspect()
            .map_or_else(
                |_| PathBuf::from("pi"),
                |installation| installation.executable,
            )
    })
}

fn device(store: &ConfigStore, command: DeviceCommand) -> Result<()> {
    match command {
        DeviceCommand::Pair => {
            let config = store
                .load_or_create(default_host_name())
                .context("loading Pix configuration")?;
            let remote = config.preferences.active_relay_url().is_some();
            let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
            if !interactive {
                bail!("`pix device pair` requires an interactive terminal");
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
            if config.devices.is_empty() {
                println!("No paired devices.");
                return Ok(());
            }
            for device in config.devices {
                println!("{}  {}", device.id, device.name);
                println!("  paired {}", device.paired_at.to_rfc3339());
            }
            Ok(())
        }
        DeviceCommand::Revoke { id } => {
            let mut config = store.load().context("loading Pix configuration")?;
            let id = select_device_id(&config, id)?;
            let index = config
                .devices
                .iter()
                .position(|device| device.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown device: {id}"))?;
            let removed = config.devices.remove(index);
            store.save(&config).context("saving Pix configuration")?;
            println!("Revoked {} ({})", removed.name, removed.id);
            Ok(())
        }
    }
}

fn pi_command(store: &ConfigStore, command: PiCommand) -> Result<()> {
    match command {
        PiCommand::Show => {
            let config = store.load_or_create(default_host_name())?;
            let environment = HostEnvironment::resolve_for("pi");
            let executable = configured_pi_executable(&config, &environment);
            let source = if config.preferences.pi_executable.is_some() {
                "configured"
            } else {
                "detected"
            };
            println!("{source}: {}", executable.display());
            Ok(())
        }
        PiCommand::Set { path } => {
            let environment = HostEnvironment::resolve_for("pi");
            let installation = PiProbe::new(Some(path.clone()))
                .with_environment(environment)
                .inspect()
                .with_context(|| format!("probing Pi at {}", path.display()))?;
            if !installation.supported {
                bail!(
                    "Pi {} is outside the currently verified range {}",
                    installation.version,
                    pix_core::pi::SUPPORTED_PI_VERSION
                );
            }
            let mut config = store.load_or_create(default_host_name())?;
            config.preferences.pi_executable = Some(installation.executable.clone());
            store.save(&config).context("saving Pix configuration")?;
            println!("Using {}", installation.executable.display());
            Ok(())
        }
        PiCommand::Clear => {
            let mut config = store.load().context("loading Pix configuration")?;
            config.preferences.pi_executable = None;
            store.save(&config).context("saving Pix configuration")?;
            println!("Cleared the saved Pi executable.");
            Ok(())
        }
    }
}

fn workspace(store: &ConfigStore, command: WorkspaceCommand) -> Result<()> {
    match command {
        WorkspaceCommand::Add { path, name } => {
            let mut config = store
                .load_or_create(default_host_name())
                .context("loading Pix configuration")?;
            let mut registry = WorkspaceRegistry::new(&mut config);
            let added = registry
                .add(&path, name)
                .with_context(|| format!("authorizing workspace {}", path.display()))?
                .clone();
            store.save(&config).context("saving Pix configuration")?;
            println!("Authorized {} ({})", added.name, added.id);
            println!("  {}", added.path.display());
            Ok(())
        }
        WorkspaceCommand::List => {
            let config = store.load().context("loading Pix configuration")?;
            if config.workspaces.is_empty() {
                println!("No authorized workspaces.");
                return Ok(());
            }
            for workspace in config.workspaces {
                println!("{}  {}", workspace.id, workspace.name);
                println!("  {}", workspace.path.display());
            }
            Ok(())
        }
        WorkspaceCommand::Remove { id } => {
            let mut config = store.load().context("loading Pix configuration")?;
            let id = select_workspace_id(&config, id)?;
            let index = config
                .workspaces
                .iter()
                .position(|workspace| workspace.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown workspace: {id}"))?;
            let removed = config.workspaces.remove(index);
            store.save(&config).context("saving Pix configuration")?;
            println!("Removed {} ({})", removed.name, removed.id);
            Ok(())
        }
    }
}

fn select_device_id(config: &pix_core::HostConfig, id: Option<String>) -> Result<String> {
    if let Some(id) = id {
        return Ok(id);
    }
    if config.devices.is_empty() {
        bail!("no paired devices");
    }
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!("device ID is required outside an interactive terminal");
    }
    println!("Select a device:");
    for (index, device) in config.devices.iter().enumerate() {
        println!("  {}. {}", index + 1, device.name);
    }
    let answer = prompt_line("Device number", "1")?;
    if let Ok(index) = answer.parse::<usize>()
        && let Some(device) = config.devices.get(index.saturating_sub(1))
    {
        return Ok(device.id.clone());
    }
    if let Some(device) = config
        .devices
        .iter()
        .find(|device| device.name.eq_ignore_ascii_case(&answer))
    {
        return Ok(device.id.clone());
    }
    bail!("unknown device selection: {answer}")
}

fn select_workspace_id(
    config: &pix_core::HostConfig,
    id: Option<uuid::Uuid>,
) -> Result<uuid::Uuid> {
    if let Some(id) = id {
        return Ok(id);
    }
    if config.workspaces.is_empty() {
        bail!("no authorized workspaces");
    }
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!("workspace ID is required outside an interactive terminal");
    }
    println!("Select a workspace:");
    for (index, workspace) in config.workspaces.iter().enumerate() {
        println!(
            "  {}. {}  {}",
            index + 1,
            workspace.name,
            workspace.path.display()
        );
    }
    let answer = prompt_line("Workspace number", "1")?;
    if let Ok(index) = answer.parse::<usize>()
        && let Some(workspace) = config.workspaces.get(index.saturating_sub(1))
    {
        return Ok(workspace.id);
    }
    if let Some(workspace) = config
        .workspaces
        .iter()
        .find(|workspace| workspace.name.eq_ignore_ascii_case(&answer))
    {
        return Ok(workspace.id);
    }
    bail!("unknown workspace selection: {answer}")
}

fn default_host_name() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Pix Host".to_owned())
}

const fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        ServeEvent, format_confirmation_code, human_event, loggable_event, pairing_instructions,
        render_remote_pairing, validate_relay_url,
    };

    #[test]
    fn loggable_events_omit_paths_names_and_secrets() {
        let event = ServeEvent::Environment {
            source: "login shell (/Users/dev/.local/bin/zsh)".to_owned(),
            path_entries: 3,
            pi_executable: "/Users/dev/.local/bin/pi".to_owned(),
        };
        let rendered = loggable_event(&event).to_string();
        assert!(rendered.contains("path_entries"));
        assert!(!rendered.contains("/Users/dev"));
        assert!(!rendered.contains("pi_executable"));
    }

    #[test]
    fn loggable_events_redact_remote_pairing_material() {
        let event = ServeEvent::RemotePairingReady {
            qr_payload: "pix://pair?secret=top-secret".to_owned(),
            join_code: "ABCD-EFGH".to_owned(),
            expires_at: 123,
        };
        let rendered = loggable_event(&event).to_string();
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("top-secret"));
        assert!(!rendered.contains("ABCD-EFGH"));
    }

    #[test]
    fn human_events_do_not_fall_back_to_debug_structs() {
        let rendered = human_event(&ServeEvent::Ready {
            port: 1234,
            fingerprint: "fingerprint".to_owned(),
        });
        assert_eq!(rendered, "✓ Pix host is ready\n");
        assert!(!rendered.contains("Ready {"));
    }

    #[test]
    fn confirmation_codes_are_grouped_for_humans() {
        assert_eq!(format_confirmation_code("877437"), "877 437");
        assert_eq!(format_confirmation_code("12345"), "12345");
    }

    #[test]
    fn pairing_instructions_match_the_selected_transport() {
        assert!(pairing_instructions(true).contains("scan this QR code"));
        assert!(!pairing_instructions(false).contains("QR"));
        assert!(pairing_instructions(false).contains("nearby hosts"));
    }

    #[test]
    fn terminal_qr_renderer_keeps_raw_payload_out_of_human_text() {
        let rendered = render_remote_pairing(
            "pix://pair?v=1&relay=wss%3A%2F%2Fexample.test&secret=top-secret",
            "KR9M-PBYA",
            123,
        );
        assert!(rendered.contains("Scan this QR code with Pix"));
        assert!(rendered.contains("Pairing code"));
        assert!(rendered.contains("KR9M-PBYA"));
        assert!(!rendered.contains("top-secret"));
    }

    #[test]
    fn setup_relay_validation_accepts_only_websocket_endpoints() {
        assert_eq!(
            validate_relay_url(" wss://relay.example.com ").expect("valid relay"),
            "wss://relay.example.com"
        );
        assert!(validate_relay_url("https://relay.example.com").is_err());
        assert!(validate_relay_url("wss://relay.example.com/with space").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn host_log_does_not_follow_symlink_targets() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("log directory");
        let config_path = directory.path().join("config.json");
        let log_path = super::HostLog::path_for(&config_path);
        fs::create_dir_all(log_path.parent().expect("log parent")).expect("log parent");
        let protected = directory.path().join("protected.txt");
        fs::write(&protected, b"sentinel").expect("protected file");
        symlink(&protected, &log_path).expect("log symlink");

        let log = super::HostLog::open(&config_path);
        log.append_text("lifecycle", "should not reach the target");

        assert_eq!(
            fs::read(&protected).expect("protected file remains"),
            b"sentinel"
        );
    }
}
