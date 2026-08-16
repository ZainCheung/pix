//! User-level systemd service installation for Linux.
//!
//! `pix service install` writes a user unit under
//! `$XDG_CONFIG_HOME/systemd/user/pix.service` (or `~/.config/...`) and
//! delegates activation to `systemctl --user`. Root privileges are never
//! required and are never requested.

use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use tempfile::Builder;

const UNIT_NAME: &str = "pix.service";

pub fn run(store: &pix_core::ConfigStore, command: &ServiceCommand) -> Result<()> {
    match command {
        ServiceCommand::Install { no_start } => install(store, *no_start),
        ServiceCommand::Uninstall => uninstall(store),
        ServiceCommand::Stop => stop(store),
    }
}

fn install(store: &pix_core::ConfigStore, no_start: bool) -> Result<()> {
    require_linux()?;
    require_systemctl()?;

    let executable = std::env::current_exe().context("locating current pix executable")?;
    let config_path = absolute_config_path(store.path())?;
    let unit_path = unit_file_path()?;
    write_unit_file(&unit_path, &executable, &config_path)?;
    run_systemctl(&["daemon-reload"])?;
    if no_start {
        run_systemctl(&["enable", UNIT_NAME])?;
    } else {
        run_systemctl(&["enable", "--now", UNIT_NAME])?;
    }
    println!("Installed Pix user service ({}).", unit_path.display());
    if no_start {
        println!("Run `systemctl --user start pix.service` to start it.");
    } else {
        println!("Pix service is enabled and running.");
    }
    Ok(())
}

fn uninstall(store: &pix_core::ConfigStore) -> Result<()> {
    require_linux()?;
    let unit_path = unit_file_path()?;
    if !unit_path.exists() {
        println!(
            "No Pix user service unit was installed at {}.",
            unit_path.display()
        );
        return Ok(());
    }
    require_systemctl()?;

    // Do not remove the unit unless systemd has acknowledged both the stop
    // and the reload. Keeping the unit on failure makes recovery possible and
    // avoids silently leaving a running daemon behind.
    run_systemctl(&["disable", "--now", UNIT_NAME])
        .context("stopping and disabling Pix user service")?;
    run_systemctl(&["daemon-reload"]).context("reloading the systemd user manager")?;
    if service_is_active()? {
        bail!(
            "Pix user service is still active; keeping {}",
            unit_path.display()
        );
    }

    match fs::remove_file(&unit_path) {
        Ok(()) => println!("Removed {}.", unit_path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "No Pix user service unit was installed at {}.",
                unit_path.display()
            );
        }
        Err(error) => {
            return Err(error).with_context(|| format!("removing {}", unit_path.display()));
        }
    }
    let _ = store;
    Ok(())
}

/// Stops a running daemon through its private local control channel. This is
/// used by systemd's `ExecStop`, so it does not depend on stdin or a second
/// systemd transaction.
fn stop(store: &pix_core::ConfigStore) -> Result<()> {
    require_linux()?;
    if crate::status::request_control_command(store.path(), "quit")? {
        println!("Requested Pix host service shutdown.");
        return Ok(());
    }
    if crate::status::HostServiceStatus::current(store.path()).is_none() {
        println!("Pix host service is not running.");
        return Ok(());
    }
    bail!("Pix host service is running but its control socket is unavailable")
}

// On Linux this function reduces to the success branch, but the Result
// return keeps the same call site and error contract on other platforms.
#[allow(clippy::unnecessary_wraps)]
fn require_linux() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        bail!("`pix service` is available on Linux with systemd user services")
    }
}

fn require_systemctl() -> Result<()> {
    if systemctl_exists() {
        Ok(())
    } else {
        bail!("systemctl is not available; cannot manage a user service")
    }
}

fn systemctl_exists() -> bool {
    Command::new("systemctl")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_systemctl(arguments: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .status()
        .with_context(|| format!("running systemctl --user {}", arguments.join(" ")))?;
    if status.success() {
        Ok(())
    } else {
        bail!(
            "systemctl --user {} exited with {status}",
            arguments.join(" ")
        )
    }
}

fn service_is_active() -> Result<bool> {
    let output = Command::new("systemctl")
        .args([
            "--user",
            "show",
            UNIT_NAME,
            "--property=ActiveState",
            "--value",
        ])
        .output()
        .context("inspecting Pix user service state")?;
    if !output.status.success() {
        bail!(
            "systemctl --user show {} exited with {}",
            UNIT_NAME,
            output.status
        );
    }
    let state = String::from_utf8_lossy(&output.stdout);
    Ok(matches!(
        state.trim(),
        "active" | "activating" | "deactivating"
    ))
}

/// Renders the user unit. Kept as a pure function for tests.
pub fn render_unit(executable: &Path, config_path: &Path) -> String {
    let executable = quote_systemd_exec_arg(executable);
    let config_path = quote_systemd_exec_arg(config_path);
    format!(
        "[Unit]\n\
         Description=Pix host service (secure Pi remote interface)\n\
         Wants=network-online.target\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={executable} --config {config_path} serve --service\n\
         ExecStop={executable} --config {config_path} service stop\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         StandardInput=null\n\
         # The daemon writes payload-free lifecycle records to its private\n\
         # log. Do not stream raw event JSON into the journal.\n\
         StandardOutput=null\n\
         StandardError=journal\n\
         TimeoutStopSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
    )
}

/// Quotes one systemd `Exec*` argument without invoking a shell. `%` is
/// doubled because systemd expands specifiers even inside double quotes.
fn quote_systemd_exec_arg(path: &Path) -> String {
    let value = path.to_string_lossy();
    let mut quoted = String::from("\"");
    for byte in value.bytes() {
        match byte {
            b'\\' => quoted.push_str("\\\\"),
            b'"' => quoted.push_str("\\\""),
            b'%' => quoted.push_str("%%"),
            b'\n' => quoted.push_str("\\x0a"),
            b'\r' => quoted.push_str("\\x0d"),
            b'\t' => quoted.push_str("\\x09"),
            0..=31 | 127 => {
                let _ = write!(quoted, "\\x{byte:02x}");
            }
            _ => quoted.push(char::from(byte)),
        }
    }
    quoted.push('"');
    quoted
}

/// Returns `$XDG_CONFIG_HOME/systemd/user/pix.service` or the standard
/// `~/.config` fallback. Never creates directories.
pub fn unit_file_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is not set; cannot locate the systemd user unit")?;
            home.join(".config")
        }
    };
    Ok(base.join("systemd").join("user").join(UNIT_NAME))
}

fn write_unit_file(path: &Path, executable: &Path, config_path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("locating the systemd user unit directory")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut temporary = Builder::new()
        .prefix(".pix-service-")
        .tempfile_in(parent)
        .with_context(|| format!("creating temporary unit in {}", parent.display()))?;
    temporary
        .write_all(render_unit(executable, config_path).as_bytes())
        .and_then(|()| temporary.as_file_mut().sync_all())
        .with_context(|| format!("writing temporary unit {}", temporary.path().display()))?;
    temporary.persist(path).map_err(|error| {
        anyhow::anyhow!("persisting unit to {}: {}", path.display(), error.error)
    })?;
    Ok(())
}

fn absolute_config_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("locating the current directory for the Pix config")?
            .join(path))
    }
}

#[derive(Debug, clap::Subcommand)]
pub enum ServiceCommand {
    /// Create and enable the Pix user service.
    Install {
        /// Enable the unit but do not start it now.
        #[arg(long)]
        no_start: bool,
    },
    /// Stop, disable, and remove the Pix user service.
    Uninstall,
    /// Ask a running Pix host service to exit through its private control socket.
    #[command(hide = true)]
    Stop,
}

#[cfg(test)]
mod tests {
    use super::render_unit;

    #[test]
    fn unit_contains_the_systemd_user_service_contract() {
        let unit = render_unit(
            std::path::Path::new("/usr/bin/Pix Host/pix%bin"),
            std::path::Path::new("/tmp/custom config/100%/config.json"),
        );
        assert!(unit.contains(
            "ExecStart=\"/usr/bin/Pix Host/pix%%bin\" --config \"/tmp/custom config/100%%/config.json\" serve --service"
        ));
        assert!(unit.contains(
            "ExecStop=\"/usr/bin/Pix Host/pix%%bin\" --config \"/tmp/custom config/100%%/config.json\" service stop"
        ));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("StandardInput=null"));
        assert!(unit.contains("StandardOutput=null"));
        assert!(!unit.contains('\r'));
    }
}
