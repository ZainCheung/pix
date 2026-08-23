//! Cross-platform management of the persistent Pix host service.
//!
//! The CLI and the macOS menu app attach to the same long-lived `pix serve
//! --service` process through the private sockets in the Pix run directory.
//! Platform modules only own service-manager details: systemd user units on
//! Linux and `LaunchAgents` on macOS.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;

use crate::output::CommandOutput;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug, clap::Subcommand)]
pub enum ServiceCommand {
    /// Install and enable the user service, starting it unless --no-start is set.
    Install {
        /// Install and enable the service without starting it now.
        #[arg(long)]
        no_start: bool,
    },
    /// Remove the user service and disable it.
    Uninstall,
    /// Start the installed user service.
    Start,
    /// Stop the user service without removing its installation.
    Stop,
    /// Restart the installed user service.
    Restart,
    /// Show service-manager and host-process status.
    Status,
    /// Print the most recent payload-free host log entries.
    Logs {
        /// Number of trailing log lines to print.
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
}

pub fn run(store: &ConfigStore, command: &ServiceCommand, output: CommandOutput) -> Result<()> {
    match command {
        ServiceCommand::Install { no_start } => {
            let path = platform_install(store, *no_start, !output.is_json())?;
            if !no_start {
                wait_until_ready(store)?;
            }
            if output.is_json() {
                output.success(
                    "service.install",
                    &serde_json::json!({
                        "service": snapshot(store)?,
                        "definition": path,
                    }),
                )?;
            }
            Ok(())
        }
        ServiceCommand::Start => {
            start_with_announce(store, !output.is_json())?;
            emit_mutation(output, "service.start", store)
        }
        ServiceCommand::Stop => {
            platform_stop(store, !output.is_json())?;
            emit_mutation(output, "service.stop", store)
        }
        ServiceCommand::Restart => {
            platform_restart(store, !output.is_json())?;
            wait_until_ready(store)?;
            emit_mutation(output, "service.restart", store)
        }
        ServiceCommand::Uninstall => {
            platform_uninstall(store, !output.is_json())?;
            emit_mutation(output, "service.uninstall", store)
        }
        ServiceCommand::Status => {
            if output.is_json() {
                output.success("service.status", &snapshot(store)?)
            } else {
                status(store)
            }
        }
        ServiceCommand::Logs { tail } => show_logs(store, *tail, output),
    }
}

fn emit_mutation(output: CommandOutput, command: &str, store: &ConfigStore) -> Result<()> {
    if output.is_json() {
        output.success(command, &snapshot(store)?)?;
    }
    Ok(())
}

/// Installs and starts the service for setup. The returned path is only used
/// for setup UI; the actual daemon is managed by the platform service manager.
pub fn install_for_setup(store: &ConfigStore) -> Result<PathBuf> {
    let unit_path = platform_install(store, false, false)?;
    wait_until_ready(store)?;
    Ok(unit_path)
}

/// Removes the service unit. Setup uses this when the user asked for no
/// background service but pairing needed a temporary host.
pub fn uninstall_for_setup(store: &ConfigStore) -> Result<()> {
    platform_uninstall(store, false)
}

fn start_with_announce(store: &ConfigStore, announce: bool) -> Result<()> {
    if crate::status::HostServiceStatus::current(store.path()).is_some() {
        if announce {
            println!("Pix host service is already running.");
        }
        return Ok(());
    }
    platform_start(store, announce)?;
    wait_until_ready(store)
}

/// Stops the selected host without writing human text into a structured caller.
pub fn stop_quiet(store: &ConfigStore) -> Result<()> {
    platform_stop(store, false)
}

/// Restarts a matching managed service and waits for its new control surface.
pub fn restart_for_config(store: &ConfigStore) -> Result<()> {
    platform_restart(store, false)?;
    wait_until_ready(store)
}

/// Returns whether the platform manager has an installed service.
pub fn managed_service_installed(store: &ConfigStore) -> Result<bool> {
    platform_installed(store)
}

/// Returns whether the selected configuration's host service is active.
pub fn managed_service_active(store: &ConfigStore) -> Result<bool> {
    platform_active(store)
}

/// Starts (or installs and starts) the persistent service and waits for its
/// private control/event sockets. This is the entry point used by
/// `pix device pair`, so pairing never tears down an existing host.
pub fn ensure_running(store: &ConfigStore) -> Result<()> {
    if service_ready(store) {
        return Ok(());
    }
    if platform_installed(store)? {
        platform_start(store, false)?;
    } else {
        platform_install(store, false, false)?;
    }
    wait_until_ready(store)
}

fn wait_until_ready(store: &ConfigStore) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        if service_ready(store) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!(
        "Pix service manager accepted the start request but the host did not become ready; run `pix service status` and `pix service logs`"
    )
}

fn service_ready(store: &ConfigStore) -> bool {
    crate::status::HostServiceStatus::current(store.path()).is_some()
        && crate::status::HostServiceStatus::event_socket_path_for(store.path()).exists()
}

/// Sends one command to the running host. The service acknowledges receipt;
/// command-specific errors are emitted on the event socket.
pub fn send_command(store: &ConfigStore, command: &str) -> Result<()> {
    if !crate::status::request_control_command(store.path(), command)? {
        bail!("Pix host service is not running; run `pix service start`");
    }
    Ok(())
}

/// Connects to the non-persistent JSONL event stream exposed by the host.
#[cfg(unix)]
pub fn connect_events(store: &ConfigStore) -> Result<std::os::unix::net::UnixStream> {
    crate::status::connect_event_stream(store.path())
}

fn status(store: &ConfigStore) -> Result<()> {
    let installed = platform_installed(store)?;
    let active = platform_active(store)?;
    println!("Pix service status");
    println!("  manager: {}", platform_name());
    println!("  installed: {}", if installed { "yes" } else { "no" });
    println!("  manager active: {}", if active { "yes" } else { "no" });
    if let Some(current) = crate::status::HostServiceStatus::current(store.path()) {
        println!(
            "  host: running (pid {}, port {}, started_at {})",
            current.pid, current.port, current.started_at
        );
    } else {
        println!("  host: not running");
    }
    println!(
        "  control socket: {}",
        crate::status::HostServiceStatus::control_socket_path_for(store.path()).display()
    );
    println!(
        "  event socket: {}",
        crate::status::HostServiceStatus::event_socket_path_for(store.path()).display()
    );
    Ok(())
}

fn snapshot(store: &ConfigStore) -> Result<serde_json::Value> {
    let installed = platform_installed(store)?;
    let manager_active = platform_active(store)?;
    let current = crate::status::HostServiceStatus::current(store.path());
    Ok(serde_json::json!({
        "manager": platform_name(),
        "installed": installed,
        "manager_active": manager_active,
        "host": match &current {
            Some(status) => serde_json::json!({
                "state": "running",
                "pid": status.pid,
                "port": status.port,
                "started_at": status.started_at,
            }),
            None => serde_json::json!({"state": "stopped"}),
        },
        "control_socket": crate::status::HostServiceStatus::control_socket_path_for(store.path()),
        "event_socket": crate::status::HostServiceStatus::event_socket_path_for(store.path()),
    }))
}

fn show_logs(store: &ConfigStore, tail: usize, output: CommandOutput) -> Result<()> {
    let path = log_path(store.path());
    match fs::read_to_string(&path) {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(tail);
            if output.is_json() {
                let entries = lines[start..]
                    .iter()
                    .map(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .unwrap_or_else(|_| serde_json::Value::String((*line).to_owned()))
                    })
                    .collect::<Vec<_>>();
                return output.success(
                    "service.logs",
                    &serde_json::json!({"path": path, "entries": entries}),
                );
            }
            println!("log file: {}", path.display());
            for line in &lines[start..] {
                println!("{line}");
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if output.is_json() {
                return output.success(
                    "service.logs",
                    &serde_json::json!({"path": path, "entries": []}),
                );
            }
            println!("log file: {}", path.display());
            println!("(no log entries yet)");
            Ok(())
        }
        Err(error) => Err(error).context("reading host log"),
    }
}

pub fn log_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map_or_else(|| PathBuf::from("logs"), |dir| dir.join("logs"))
        .join("host.jsonl")
}

fn platform_name() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "systemd --user"
    }
    #[cfg(target_os = "macos")]
    {
        "launchd LaunchAgent"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        "unsupported"
    }
}

#[cfg(target_os = "linux")]
fn platform_install(store: &ConfigStore, no_start: bool, announce: bool) -> Result<PathBuf> {
    linux::install(store, no_start, announce)
}
#[cfg(target_os = "macos")]
fn platform_install(store: &ConfigStore, no_start: bool, announce: bool) -> Result<PathBuf> {
    macos::install(store, no_start, announce)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_install(_store: &ConfigStore, _no_start: bool, _announce: bool) -> Result<PathBuf> {
    bail!("`pix service` is supported on Linux and macOS")
}

#[cfg(target_os = "linux")]
fn platform_start(store: &ConfigStore, announce: bool) -> Result<()> {
    linux::start(store, announce)
}
#[cfg(target_os = "macos")]
fn platform_start(store: &ConfigStore, announce: bool) -> Result<()> {
    macos::start(store, announce)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_start(_store: &ConfigStore, _announce: bool) -> Result<()> {
    bail!("`pix service` is supported on Linux and macOS")
}

#[cfg(target_os = "linux")]
fn platform_stop(store: &ConfigStore, announce: bool) -> Result<()> {
    linux::stop(store, announce)
}
#[cfg(target_os = "macos")]
fn platform_stop(store: &ConfigStore, announce: bool) -> Result<()> {
    macos::stop(store, announce)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_stop(_store: &ConfigStore, _announce: bool) -> Result<()> {
    bail!("`pix service` is supported on Linux and macOS")
}

#[cfg(target_os = "linux")]
fn platform_restart(store: &ConfigStore, announce: bool) -> Result<()> {
    linux::restart(store, announce)
}
#[cfg(target_os = "macos")]
fn platform_restart(store: &ConfigStore, announce: bool) -> Result<()> {
    macos::restart(store, announce)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_restart(_store: &ConfigStore, _announce: bool) -> Result<()> {
    bail!("`pix service` is supported on Linux and macOS")
}

#[cfg(target_os = "linux")]
fn platform_uninstall(store: &ConfigStore, announce: bool) -> Result<()> {
    linux::uninstall(store, announce)
}
#[cfg(target_os = "macos")]
fn platform_uninstall(store: &ConfigStore, announce: bool) -> Result<()> {
    macos::uninstall(store, announce)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_uninstall(_store: &ConfigStore, _announce: bool) -> Result<()> {
    bail!("`pix service` is supported on Linux and macOS")
}

#[cfg(target_os = "linux")]
fn platform_installed(store: &ConfigStore) -> Result<bool> {
    linux::installed(store)
}
#[cfg(target_os = "macos")]
fn platform_installed(store: &ConfigStore) -> Result<bool> {
    macos::installed(store)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_installed(_store: &ConfigStore) -> Result<bool> {
    Ok(false)
}

#[cfg(target_os = "linux")]
fn platform_active(store: &ConfigStore) -> Result<bool> {
    linux::active(store)
}
#[cfg(target_os = "macos")]
fn platform_active(store: &ConfigStore) -> Result<bool> {
    macos::active(store)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_active(_store: &ConfigStore) -> Result<bool> {
    Ok(false)
}

pub(crate) fn service_command(
    store: &ConfigStore,
    command: Option<ServiceCommand>,
    output: CommandOutput,
    interactive: bool,
) -> Result<()> {
    if let Some(command) = command {
        return run(store, &command, output);
    }
    if !interactive {
        return Err(usage_error(
            "a service command is required outside an interactive terminal",
        ));
    }
    let running = HostServiceStatus::current(store.path()).is_some();
    let installed = managed_service_installed(store).unwrap_or(false);
    let ui = SetupUi::new(true, false);
    ui.crumb_header("Service");
    ui.status_row(
        "host",
        if running {
            "● running"
        } else if installed {
            "○ installed, stopped"
        } else {
            "○ not installed"
        },
        if running {
            UiTone::Success
        } else if installed {
            UiTone::Warning
        } else {
            UiTone::Muted
        },
    );
    println!();
    let mut actions = Vec::new();
    if !installed {
        actions.push((
            0_u8,
            MenuItem::new("Install service", "Start Pix automatically"),
        ));
    } else if running {
        actions.push((
            1,
            MenuItem::new("Restart service", "Reload host configuration"),
        ));
        actions.push((2, MenuItem::new("Stop service", "Stop remote host access")));
    } else {
        actions.push((
            3,
            MenuItem::new("Start service", "Resume remote host access"),
        ));
    }
    actions.push((
        4,
        MenuItem::new("Service status", "Show manager and socket details"),
    ));
    actions.push((
        5,
        MenuItem::new("View logs", "Show recent payload-free events"),
    ));
    if installed {
        actions.push((
            6,
            MenuItem::new("Uninstall service", "Remove automatic startup"),
        ));
    }
    actions.push((7, MenuItem::new("Back", "Return to the shell")));
    let items = actions.iter().map(|(_, item)| *item).collect::<Vec<_>>();
    match ui.menu("Actions", &items, 0)? {
        MenuResult::Selected(index) => match actions[index].0 {
            0 => run(store, &ServiceCommand::Install { no_start: false }, output),
            1 => run(store, &ServiceCommand::Restart, output),
            2 => run(store, &ServiceCommand::Stop, output),
            3 => run(store, &ServiceCommand::Start, output),
            4 => run(store, &ServiceCommand::Status, output),
            5 => run(store, &ServiceCommand::Logs { tail: 50 }, output),
            6 => {
                let choices = vec!["Uninstall service".to_owned(), "Cancel".to_owned()];
                if ui.select("Remove the Pix background service?", &choices, 1)? == 0 {
                    run(store, &ServiceCommand::Uninstall, output)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        },
        MenuResult::Help => print_cli_help(),
        MenuResult::Quit => Ok(()),
    }
}

use crate::setup_ui::{MenuItem, MenuResult, SetupUi, UiTone};
use crate::status::HostServiceStatus;
use crate::{print_cli_help, usage_error};
