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

pub fn run(store: &ConfigStore, command: &ServiceCommand) -> Result<()> {
    match command {
        ServiceCommand::Install { no_start } => install(store, *no_start),
        ServiceCommand::Start => start(store),
        ServiceCommand::Stop => stop(store),
        ServiceCommand::Restart => restart(store),
        ServiceCommand::Uninstall => uninstall(store),
        ServiceCommand::Status => status(store),
        ServiceCommand::Logs { tail } => show_logs(store, *tail),
    }
}

fn install(store: &ConfigStore, no_start: bool) -> Result<()> {
    platform_install(store, no_start, true).map(|_| ())
}

/// Installs and starts the service for setup. The returned path is only used
/// for setup UI; the actual daemon is managed by the platform service manager.
pub fn install_for_setup(store: &ConfigStore) -> Result<PathBuf> {
    platform_install(store, false, false)
}

pub fn start(store: &ConfigStore) -> Result<()> {
    if crate::status::HostServiceStatus::current(store.path()).is_some() {
        println!("Pix host service is already running.");
        return Ok(());
    }
    platform_start(store, true)
}

pub fn stop(store: &ConfigStore) -> Result<()> {
    platform_stop(store)
}

pub fn restart(store: &ConfigStore) -> Result<()> {
    platform_restart(store)
}

pub fn uninstall(store: &ConfigStore) -> Result<()> {
    platform_uninstall(store)
}

/// Returns whether the platform manager has an installed service.
pub fn managed_service_installed(store: &ConfigStore) -> Result<bool> {
    platform_installed(store)
}

/// Returns whether the platform manager currently reports the service active.
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

fn show_logs(store: &ConfigStore, tail: usize) -> Result<()> {
    let path = log_path(store.path());
    println!("log file: {}", path.display());
    match fs::read_to_string(&path) {
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
fn platform_stop(store: &ConfigStore) -> Result<()> {
    linux::stop(store)
}
#[cfg(target_os = "macos")]
fn platform_stop(store: &ConfigStore) -> Result<()> {
    macos::stop(store)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_stop(_store: &ConfigStore) -> Result<()> {
    bail!("`pix service` is supported on Linux and macOS")
}

#[cfg(target_os = "linux")]
fn platform_restart(store: &ConfigStore) -> Result<()> {
    linux::restart(store)
}
#[cfg(target_os = "macos")]
fn platform_restart(store: &ConfigStore) -> Result<()> {
    macos::restart(store)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_restart(_store: &ConfigStore) -> Result<()> {
    bail!("`pix service` is supported on Linux and macOS")
}

#[cfg(target_os = "linux")]
fn platform_uninstall(store: &ConfigStore) -> Result<()> {
    linux::uninstall(store)
}
#[cfg(target_os = "macos")]
fn platform_uninstall(store: &ConfigStore) -> Result<()> {
    macos::uninstall(store)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_uninstall(_store: &ConfigStore) -> Result<()> {
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
