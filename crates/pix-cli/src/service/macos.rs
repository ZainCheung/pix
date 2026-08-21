//! macOS per-user `LaunchAgent` integration for the persistent Pix host service.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;
use tempfile::Builder;

const LAUNCH_AGENT_LABEL: &str = "com.deepoke.pix.host";

pub(crate) fn install(store: &ConfigStore, no_start: bool, announce: bool) -> Result<PathBuf> {
    require_launchctl()?;
    let executable = std::env::current_exe().context("locating current pix executable")?;
    let config_path = absolute_config_path(store.path())?;
    let plist_path = launch_agent_path()?;
    let log_directory = config_path
        .parent()
        .context("locating the Pix configuration directory")?
        .join("logs");
    fs::create_dir_all(&log_directory)
        .with_context(|| format!("creating {}", log_directory.display()))?;
    write_launch_agent(&plist_path, &executable, &config_path, &log_directory)?;

    let host_running = crate::status::HostServiceStatus::current(store.path()).is_some();
    if launchctl_is_loaded()? {
        if !no_start && !host_running {
            run_launchctl(&["kickstart", "-k", &launchctl_target()?])?;
        }
    } else if !no_start && !host_running {
        bootstrap_launch_agent(&plist_path)?;
    }
    if announce {
        println!("Installed Pix LaunchAgent ({}).", plist_path.display());
        if no_start {
            println!("Run `pix service start` to start it.");
        } else {
            println!("Pix service is enabled and running.");
        }
    }
    Ok(plist_path)
}

pub(crate) fn start(store: &ConfigStore, announce: bool) -> Result<()> {
    require_launchctl()?;
    if !installed(store)? {
        bail!("Pix LaunchAgent is not installed; run `pix service install` first");
    }
    if launchctl_is_loaded()? {
        run_launchctl(&["kickstart", "-k", &launchctl_target()?])?;
    } else {
        bootstrap_launch_agent(&launch_agent_path()?)?;
    }
    if announce {
        println!("Pix service start requested.");
    }
    Ok(())
}

pub(crate) fn stop(store: &ConfigStore) -> Result<()> {
    require_launchctl()?;
    let requested = request_graceful_shutdown(store)?;
    if launchctl_is_loaded()? {
        run_launchctl(&["bootout", &launchctl_target()?])?;
        println!("Pix service stopped.");
    } else if requested {
        println!("Requested Pix host service shutdown.");
    } else {
        println!("Pix service is not running.");
    }
    Ok(())
}

pub(crate) fn restart(store: &ConfigStore) -> Result<()> {
    require_launchctl()?;
    if !installed(store)? {
        bail!("Pix LaunchAgent is not installed; run `pix service install` first");
    }
    let _ = request_graceful_shutdown(store)?;
    if launchctl_is_loaded()? {
        run_launchctl(&["bootout", &launchctl_target()?])?;
    }
    if installed(store)? {
        bootstrap_launch_agent(&launch_agent_path()?)?;
    }
    println!("Pix service restart requested.");
    Ok(())
}

fn request_graceful_shutdown(store: &ConfigStore) -> Result<bool> {
    if !crate::status::request_control_command(store.path(), "quit")? {
        return Ok(false);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while crate::status::HostServiceStatus::current(store.path()).is_some()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(true)
}

pub(crate) fn uninstall(store: &ConfigStore) -> Result<()> {
    require_launchctl()?;
    let path = launch_agent_path()?;
    let _ = if path.exists() {
        request_graceful_shutdown(store)?
    } else {
        false
    };
    if launchctl_is_loaded()? {
        run_launchctl(&["bootout", &launchctl_target()?])?;
    }
    if crate::status::HostServiceStatus::current(store.path()).is_some() {
        bail!(
            "Pix host service is still running; keeping {}",
            path.display()
        );
    }
    match fs::remove_file(&path) {
        Ok(()) => println!("Removed {}.", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("No Pix LaunchAgent is installed at {}.", path.display());
        }
        Err(error) => return Err(error).with_context(|| format!("removing {}", path.display())),
    }
    Ok(())
}

pub(crate) fn installed(_store: &ConfigStore) -> Result<bool> {
    Ok(launch_agent_path()?.is_file())
}

pub(crate) fn active(store: &ConfigStore) -> Result<bool> {
    // launchctl is global to the user session and cannot distinguish a
    // different Pix config. The config-scoped status record is the source of
    // truth used by the native macOS client and `pix status`.
    Ok(crate::status::HostServiceStatus::current(store.path()).is_some())
}

fn require_launchctl() -> Result<()> {
    let status = Command::new("launchctl")
        .arg("help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("checking for launchctl")?;
    if status.success() {
        Ok(())
    } else {
        bail!("launchctl is not available; cannot manage a macOS LaunchAgent")
    }
}

fn launch_agent_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; cannot locate the macOS LaunchAgent")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist")))
}

fn launchctl_domain() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("determining the current macOS user ID")?;
    if !output.status.success() {
        bail!("id -u exited with {}", output.status);
    }
    let uid = String::from_utf8(output.stdout)
        .context("decoding the current macOS user ID")?
        .trim()
        .to_owned();
    if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("macOS user ID is invalid");
    }
    Ok(format!("gui/{uid}"))
}

fn launchctl_target() -> Result<String> {
    Ok(format!("{}/{LAUNCH_AGENT_LABEL}", launchctl_domain()?))
}

fn launchctl_is_loaded() -> Result<bool> {
    Ok(Command::new("launchctl")
        .args(["print", &launchctl_target()?])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("inspecting the Pix LaunchAgent")?
        .success())
}

fn bootstrap_launch_agent(path: &Path) -> Result<()> {
    let domain = launchctl_domain()?;
    let path = path.to_string_lossy().into_owned();
    run_launchctl(&["bootstrap", &domain, &path])
}

fn run_launchctl(arguments: &[&str]) -> Result<()> {
    let status = Command::new("launchctl")
        .args(arguments)
        .status()
        .with_context(|| format!("running launchctl {}", arguments.join(" ")))?;
    if status.success() {
        Ok(())
    } else {
        bail!("launchctl {} exited with {status}", arguments.join(" "))
    }
}

fn render_launch_agent(executable: &Path, config_path: &Path, log_directory: &Path) -> String {
    let executable = xml_escape(&executable.to_string_lossy());
    let config_path = xml_escape(&config_path.to_string_lossy());
    let log_directory = xml_escape(&log_directory.to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
           <key>Label</key><string>{LAUNCH_AGENT_LABEL}</string>\n\
           <key>ProgramArguments</key>\n\
           <array>\n\
             <string>{executable}</string>\n\
             <string>--config</string>\n\
             <string>{config_path}</string>\n\
             <string>serve</string>\n\
             <string>--service</string>\n\
           </array>\n\
           <key>RunAtLoad</key><true/>\n\
           <key>KeepAlive</key><true/>\n\
           <key>ThrottleInterval</key><integer>3</integer>\n\
           <key>ProcessType</key><string>Interactive</string>\n\
           <key>StandardOutPath</key><string>{log_directory}/launchagent.stdout.log</string>\n\
           <key>StandardErrorPath</key><string>{log_directory}/launchagent.stderr.log</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn write_launch_agent(
    path: &Path,
    executable: &Path,
    config_path: &Path,
    log_directory: &Path,
) -> Result<()> {
    let parent = path
        .parent()
        .context("locating the macOS LaunchAgent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut temporary = Builder::new()
        .prefix(".pix-launch-agent-")
        .tempfile_in(parent)
        .with_context(|| format!("creating temporary LaunchAgent in {}", parent.display()))?;
    temporary
        .write_all(render_launch_agent(executable, config_path, log_directory).as_bytes())
        .and_then(|()| temporary.as_file_mut().sync_all())
        .with_context(|| {
            format!(
                "writing temporary LaunchAgent {}",
                temporary.path().display()
            )
        })?;
    temporary.persist(path).map_err(|error| {
        anyhow::anyhow!(
            "persisting LaunchAgent to {}: {}",
            path.display(),
            error.error
        )
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
