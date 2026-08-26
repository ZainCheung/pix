//! Cross-platform management of the persistent Pix host service.
//!
//! The CLI and the macOS menu app attach to the same long-lived `pix serve
//! --service` process through the private sockets in the Pix run directory.
//! Platform modules only own service-manager details: systemd user units on
//! Linux and `LaunchAgents` on macOS.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;
use serde::{Deserialize, Serialize};
use tempfile::Builder;

use crate::commands::shared::{default_host_name, load_host_identity};
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
        /// Explicitly make the current CLI the service owner when another
        /// CLI is already registered for this configuration.
        #[arg(long)]
        adopt: bool,
    },
    /// Remove the user service and disable it.
    Uninstall,
    /// Start the installed user service.
    Start,
    /// Stop the user service without removing its installation.
    Stop,
    /// Restart the installed user service.
    Restart,
    /// Authorize the host identity interactively and refresh its protected
    /// local recovery copy for background services.
    RepairIdentity,
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
        ServiceCommand::Install { no_start, adopt } => {
            let path = install_service(store, *no_start, !output.is_json(), *adopt)?;
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
        ServiceCommand::RepairIdentity => repair_identity(store, output),
        ServiceCommand::Uninstall => {
            platform_uninstall(store, !output.is_json())?;
            remove_service_owner(store)?;
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

fn repair_identity(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    if output.is_json() {
        bail!(
            "`pix service repair-identity` requires human output so Keychain authorization can be shown"
        )
    }
    let transaction = store.transaction()?;
    let config = transaction
        .load_or_create(default_host_name())
        .context("loading Pix configuration")?;
    load_host_identity(store, config.host.id).context("authorizing host identity")?;
    let recovery_path = store
        .path()
        .parent()
        .context("locating host identity directory")?
        .join("host-identity.key");
    println!(
        "Host identity authorized; protected recovery copy is ready at {}.",
        recovery_path.display()
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ServiceOwner {
    executable: PathBuf,
    version: String,
}

const SERVICE_OWNER_FILE: &str = "service-owner.json";

fn install_service(
    store: &ConfigStore,
    no_start: bool,
    announce: bool,
    adopt: bool,
) -> Result<PathBuf> {
    let owner = current_service_owner()?;
    ensure_service_owner(store, &owner, adopt)?;
    let path = platform_install(store, no_start, announce)?;
    write_service_owner(store, &owner)?;
    if announce && adopt {
        println!("Service owner adopted by {}.", owner.executable.display());
    }
    Ok(path)
}

fn current_service_owner() -> Result<ServiceOwner> {
    let executable = std::env::current_exe().context("locating current pix executable")?;
    Ok(ServiceOwner {
        executable: normalize_executable(&executable),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

fn normalize_executable(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn service_owner_path(store: &ConfigStore) -> Result<PathBuf> {
    Ok(store
        .path()
        .parent()
        .context("locating Pix configuration directory")?
        .join(SERVICE_OWNER_FILE))
}

fn read_service_owner(store: &ConfigStore) -> Result<Option<ServiceOwner>> {
    let path = service_owner_path(store)?;
    let manifest = match fs::read(&path) {
        Ok(contents) => Some(
            serde_json::from_slice::<ServiceOwner>(&contents)
                .with_context(|| format!("decoding service owner {}", path.display()))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let installed_executable = platform_installed_executable(store)?;
    if let Some(executable) = installed_executable {
        let executable = normalize_executable(&executable);
        if let Some(owner) = manifest
            .as_ref()
            .filter(|owner| normalize_executable(&owner.executable) == executable)
        {
            return Ok(Some(ServiceOwner {
                executable,
                version: owner.version.clone(),
            }));
        }
        // Older Pix installations predate the ownership manifest, and a
        // --no-start adoption may leave the old process loaded temporarily.
        // The platform definition is the live source of truth for the path.
        return Ok(Some(ServiceOwner {
            executable,
            version: "legacy/unknown".to_owned(),
        }));
    }
    Ok(manifest)
}

fn write_service_owner(store: &ConfigStore, owner: &ServiceOwner) -> Result<()> {
    let path = service_owner_path(store)?;
    let parent = path.parent().context("locating service owner directory")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut temporary = Builder::new()
        .prefix(".pix-service-owner-")
        .tempfile_in(parent)
        .with_context(|| format!("creating temporary service owner in {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .context("securing service owner")?;
    }
    let encoded = serde_json::to_vec_pretty(owner).context("encoding service owner")?;
    temporary
        .write_all(&encoded)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .with_context(|| {
            format!(
                "writing temporary service owner {}",
                temporary.path().display()
            )
        })?;
    temporary.persist(&path).map_err(|error| {
        anyhow::anyhow!(
            "persisting service owner to {}: {}",
            path.display(),
            error.error
        )
    })?;
    Ok(())
}

fn remove_service_owner(store: &ConfigStore) -> Result<()> {
    let path = service_owner_path(store)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

fn ensure_service_owner(store: &ConfigStore, current: &ServiceOwner, adopt: bool) -> Result<()> {
    if !platform_installed(store)? {
        return Ok(());
    }
    check_service_owner(read_service_owner(store)?.as_ref(), current, adopt)
}

fn check_service_owner(
    existing: Option<&ServiceOwner>,
    current: &ServiceOwner,
    adopt: bool,
) -> Result<()> {
    let Some(existing) = existing else {
        if adopt {
            return Ok(());
        }
        bail!(
            "Pix service owner is unknown for this existing installation; run `pix service install --adopt` once to register the current CLI ({})",
            current.executable.display()
        );
    };
    if normalize_executable(&existing.executable) == current.executable || adopt {
        return Ok(());
    }
    bail!(
        "Pix service is owned by {} (Pix {}), but this CLI is {}; run `pix service install --adopt` only when intentionally switching the service owner",
        existing.executable.display(),
        existing.version,
        current.executable.display()
    );
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
    let unit_path = install_service(store, false, false, false)?;
    wait_until_ready(store)?;
    Ok(unit_path)
}

/// Removes the service unit. Setup uses this when the user asked for no
/// background service but pairing needed a temporary host.
pub fn uninstall_for_setup(store: &ConfigStore) -> Result<()> {
    platform_uninstall(store, false)?;
    remove_service_owner(store)
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
        install_service(store, false, false, false)?;
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
    let owner = read_service_owner(store)?;
    let current_cli = current_service_owner()?.executable;
    println!("Pix service status");
    println!("  manager: {}", platform_name());
    println!("  installed: {}", if installed { "yes" } else { "no" });
    println!("  manager active: {}", if active { "yes" } else { "no" });
    match owner {
        Some(owner) => {
            println!(
                "  owner: {} (Pix {})",
                owner.executable.display(),
                owner.version
            );
            println!(
                "  owner matches current CLI: {}",
                if normalize_executable(&owner.executable) == current_cli {
                    "yes"
                } else {
                    "no"
                }
            );
        }
        None if installed => {
            println!("  owner: unknown (run `pix service install --adopt` once to register it)");
        }
        None => println!("  owner: not registered"),
    }
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
    let owner = read_service_owner(store)?;
    let current_cli = current_service_owner()?;
    let owner_snapshot = owner.map(|owner| {
        let matches_current_cli = normalize_executable(&owner.executable) == current_cli.executable;
        serde_json::json!({
            "executable": owner.executable,
            "version": owner.version,
            "matches_current_cli": matches_current_cli,
        })
    });
    Ok(serde_json::json!({
        "manager": platform_name(),
        "installed": installed,
        "manager_active": manager_active,
        "owner": owner_snapshot,
        "current_cli": {
            "executable": current_cli.executable,
            "version": current_cli.version,
        },
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
fn platform_installed_executable(store: &ConfigStore) -> Result<Option<PathBuf>> {
    linux::installed_executable(store)
}
#[cfg(target_os = "macos")]
fn platform_installed_executable(store: &ConfigStore) -> Result<Option<PathBuf>> {
    macos::installed_executable(store)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_installed_executable(_store: &ConfigStore) -> Result<Option<PathBuf>> {
    Ok(None)
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
            0 => run(
                store,
                &ServiceCommand::Install {
                    no_start: false,
                    adopt: false,
                },
                output,
            ),
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        ConfigStore, ServiceOwner, check_service_owner, normalize_executable, read_service_owner,
        write_service_owner,
    };

    #[test]
    fn service_owner_manifest_round_trips_the_canonical_cli_path() {
        let directory = tempdir().expect("service owner directory");
        let store = ConfigStore::new(directory.path().join("config.json"));
        let owner = ServiceOwner {
            executable: normalize_executable(std::path::Path::new("/bin/sh")),
            version: "0.1.0".to_owned(),
        };

        write_service_owner(&store, &owner).expect("write owner manifest");
        assert_eq!(
            read_service_owner(&store).expect("read owner manifest"),
            Some(owner)
        );
    }

    #[test]
    fn service_owner_manifest_uses_a_stable_relative_filename() {
        let directory = tempdir().expect("service owner directory");
        let store = ConfigStore::new(directory.path().join("config.json"));
        let owner = ServiceOwner {
            executable: "/tmp/pix".into(),
            version: "0.1.0".to_owned(),
        };

        write_service_owner(&store, &owner).expect("write owner manifest");
        assert!(directory.path().join("service-owner.json").is_file());
    }

    #[test]
    fn service_owner_mismatch_requires_explicit_adoption() {
        let existing = ServiceOwner {
            executable: "/Applications/Pix.app/Contents/Resources/pix".into(),
            version: "0.1.0".to_owned(),
        };
        let current = ServiceOwner {
            executable: "/Users/example/.local/bin/pix".into(),
            version: "0.1.0".to_owned(),
        };

        let error = check_service_owner(Some(&existing), &current, false)
            .expect_err("a different CLI must not silently take ownership");
        assert!(error.to_string().contains("--adopt"));
        check_service_owner(Some(&existing), &current, true).expect("explicit adoption");
    }
}
