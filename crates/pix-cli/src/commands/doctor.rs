//! Local configuration and Pi RPC diagnostics.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pix_core::{ConfigStore, HostEnvironment, PiProbe};

use crate::output::CommandOutput;

pub(crate) fn doctor(
    store: &ConfigStore,
    pi: Option<PathBuf>,
    verbose: bool,
    output: CommandOutput,
) -> Result<()> {
    let (config_state, workspaces, devices) = match store.load() {
        Ok(config) => ("ready", config.workspaces.len(), config.devices.len()),
        Err(pix_core::config::ConfigError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            ("missing", 0, 0)
        }
        Err(error) => bail!("host config: {error}"),
    };

    let environment = HostEnvironment::resolve_for("pi");
    let environment_source = environment.describe();
    let path_entries = environment.path_entry_count();
    if !output.is_json() {
        println!("Pix doctor");
        println!("  config: {}", store.path().display());
        if config_state == "ready" {
            println!(
                "  host config: ok ({} workspace{}, {} paired device{})",
                workspaces,
                plural(workspaces),
                devices,
                plural(devices)
            );
        } else {
            println!("  host config: not created yet");
        }
        println!("  environment: {environment_source}");
        println!("  PATH entries: {path_entries}");
    }
    let installation = PiProbe::new(pi)
        .with_environment(environment)
        .inspect()
        .context("probing Pi (run `pix pi set <path>` to pin a specific executable)")?;
    if !installation.supported {
        bail!(
            "Pi {} is outside the currently verified range {}",
            installation.version,
            pix_core::pi::SUPPORTED_PI_VERSION
        );
    }
    if output.is_json() {
        return output.success(
            "doctor",
            &serde_json::json!({
                "config": {
                    "path": store.path(),
                    "state": config_state,
                    "workspaces": workspaces,
                    "devices": devices,
                },
                "environment": {
                    "source": environment_source,
                    "path_entries": path_entries,
                },
                "pi": {
                    "executable": installation.executable,
                    "version": installation.version,
                    "supported": installation.supported,
                    "supported_range": pix_core::pi::SUPPORTED_PI_VERSION,
                },
                "host_identity_store": verbose.then(|| host_identity_path(store)),
            }),
        );
    }

    println!("  pi executable: {}", installation.executable.display());
    println!("  pi version: {}", installation.version);
    if verbose {
        println!(
            "  host identity store: {}",
            host_identity_path(store).display()
        );
    }
    println!("  pi RPC compatibility: verified");
    Ok(())
}

use crate::commands::shared::{host_identity_path, plural};
