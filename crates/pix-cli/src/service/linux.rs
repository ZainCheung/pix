//! Linux systemd --user integration for the persistent Pix host service.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;
use tempfile::Builder;

const UNIT_NAME: &str = "pix.service";

pub(crate) fn install(store: &ConfigStore, no_start: bool, announce: bool) -> Result<PathBuf> {
    require_systemctl()?;
    let executable = std::env::current_exe().context("locating current pix executable")?;
    let config_path = absolute_config_path(store.path())?;
    let path = unit_file_path()?;
    write_unit_file(&path, &executable, &config_path)?;
    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", UNIT_NAME])?;
    if !no_start && crate::status::HostServiceStatus::current(store.path()).is_none() {
        run_systemctl(&["start", UNIT_NAME])?;
    }
    if announce {
        println!("Installed Pix systemd user service ({}).", path.display());
        if no_start {
            println!("Run `pix service start` to start it.");
        } else if crate::status::HostServiceStatus::current(store.path()).is_some() {
            println!("Pix host service is running.");
        } else {
            println!("Pix service is enabled; waiting for it to become ready.");
        }
    }
    Ok(path)
}

pub(crate) fn start(store: &ConfigStore, announce: bool) -> Result<()> {
    require_systemctl()?;
    if !installed(store)? {
        bail!("Pix systemd user service is not installed; run `pix service install` first");
    }
    run_systemctl(&["start", UNIT_NAME])?;
    if announce {
        println!("Pix service start requested.");
    }
    Ok(())
}

pub(crate) fn stop(store: &ConfigStore) -> Result<()> {
    require_systemctl()?;
    // systemd invokes this same subcommand from ExecStop. In that context,
    // request a clean host shutdown instead of starting a nested systemctl
    // transaction (which would recurse indefinitely).
    if std::env::var_os("PIX_SERVICE_STOP_HOOK").is_some() {
        let _ = crate::status::request_control_command(store.path(), "quit")?;
        return Ok(());
    }
    if installed(store)? {
        if active(store)? {
            run_systemctl(&["stop", UNIT_NAME])?;
            println!("Pix service stopped.");
        } else if crate::status::request_control_command(store.path(), "quit")? {
            // A user-level unit can be installed but inactive while a
            // manually launched service instance is still serving this
            // config. Prefer the instance's private control socket before
            // reporting that the service is stopped.
            println!("Requested Pix host service shutdown.");
        } else {
            println!("Pix service is not running.");
        }
        return Ok(());
    }
    if crate::status::request_control_command(store.path(), "quit")? {
        println!("Requested Pix host service shutdown.");
    } else {
        println!("Pix service is not installed or running.");
    }
    Ok(())
}

pub(crate) fn restart(store: &ConfigStore) -> Result<()> {
    require_systemctl()?;
    if !installed(store)? {
        bail!("Pix systemd user service is not installed; run `pix service install` first");
    }
    run_systemctl(&["restart", UNIT_NAME])?;
    println!("Pix service restart requested.");
    Ok(())
}

pub(crate) fn uninstall(store: &ConfigStore) -> Result<()> {
    require_systemctl()?;
    let path = unit_file_path()?;
    if !path.exists() {
        println!(
            "No Pix systemd user service is installed at {}.",
            path.display()
        );
        return Ok(());
    }
    run_systemctl(&["disable", "--now", UNIT_NAME])?;
    run_systemctl(&["daemon-reload"])?;
    if active(store)? {
        bail!(
            "Pix systemd user service is still active; keeping {}",
            path.display()
        );
    }
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    println!("Removed {}.", path.display());
    Ok(())
}

pub(crate) fn installed(_store: &ConfigStore) -> Result<bool> {
    Ok(unit_file_path()?.is_file())
}

pub(crate) fn active(store: &ConfigStore) -> Result<bool> {
    // systemd --user is global to the login session and cannot distinguish a
    // different Pix config. The config-scoped status record is the source of
    // truth used by `pix status` and native clients.
    Ok(crate::status::HostServiceStatus::current(store.path()).is_some())
}

pub(crate) fn unit_file_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set; cannot locate the systemd user unit")?
            .join(".config"),
    };
    Ok(base.join("systemd").join("user").join(UNIT_NAME))
}

pub(crate) fn render_unit(executable: &Path, config_path: &Path) -> String {
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
         Environment=PIX_SERVICE_STOP_HOOK=1\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         StandardInput=null\n\
         StandardOutput=null\n\
         StandardError=journal\n\
         TimeoutStopSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
    )
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
    std::io::Write::write_all(
        &mut temporary,
        render_unit(executable, config_path).as_bytes(),
    )
    .and_then(|()| temporary.as_file_mut().sync_all())
    .with_context(|| format!("writing temporary unit {}", temporary.path().display()))?;
    temporary.persist(path).map_err(|error| {
        anyhow::anyhow!("persisting unit to {}: {}", path.display(), error.error)
    })?;
    Ok(())
}

fn require_systemctl() -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("checking for systemctl")?;
    if status.success() {
        Ok(())
    } else {
        bail!("systemctl is not available; cannot manage a user service")
    }
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

fn absolute_config_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("locating the current directory for the Pix config")?
            .join(path))
    }
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
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("StandardInput=null"));
        assert!(unit.contains("StandardOutput=null"));
        assert!(unit.contains(
            "ExecStop=\"/usr/bin/Pix Host/pix%%bin\" --config \"/tmp/custom config/100%%/config.json\" service stop"
        ));
        assert!(unit.contains("Environment=PIX_SERVICE_STOP_HOOK=1"));
        assert!(!unit.contains('\r'));
    }
}
