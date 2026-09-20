//! Terminal-free host-management operations shared by the explicit CLI and
//! the persistent TUI.
//!
//! These functions deliberately return data instead of rendering text.  The
//! CLI keeps ownership of its established text/JSON envelopes while the TUI
//! can run the same mutations in a background worker.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pix_core::{ConfigStore, HostConfig, HostEnvironment, PiProbe};

use crate::commands::shared::default_host_name;
use crate::service;
use crate::status::HostServiceStatus;

/// The result of a relay configuration mutation.
#[derive(Debug, Clone)]
pub(crate) struct RelayMutation {
    pub(crate) config: HostConfig,
    pub(crate) restart_required: bool,
}

/// The result of selecting a Pi executable.
#[derive(Debug, Clone)]
pub(crate) struct PiMutation {
    pub(crate) executable: PathBuf,
    pub(crate) version: String,
    pub(crate) supported: bool,
    pub(crate) restart_required: bool,
}

/// The result of clearing a saved Pi executable.
#[derive(Debug, Clone)]
pub(crate) struct PiClearMutation {
    pub(crate) restart_required: bool,
}

/// Actions exposed by the persistent Settings screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceAction {
    Install,
    Start,
    Stop,
    Restart,
    Uninstall,
}

impl ServiceAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Install => "Install service",
            Self::Start => "Start service",
            Self::Stop => "Stop service",
            Self::Restart => "Restart service",
            Self::Uninstall => "Uninstall service",
        }
    }

    pub(crate) const fn busy_label(self) -> &'static str {
        match self {
            Self::Install => "Installing service…",
            Self::Start => "Starting service…",
            Self::Stop => "Stopping service…",
            Self::Restart => "Restarting service…",
            Self::Uninstall => "Uninstalling service…",
        }
    }
}

/// Validates and normalizes a relay endpoint using the same contract as the
/// explicit `pix relay set` command.
pub(crate) fn validate_relay_url(url: &str) -> Result<String> {
    let value = url.trim();
    pix_core::validate_relay_url(value).context(
        "relay URL must be a valid ws:// or wss:// endpoint without credentials or a fragment",
    )?;
    Ok(value.to_owned())
}

/// Saves a relay endpoint and enables relay transport.
pub(crate) fn set_relay(store: &ConfigStore, url: &str) -> Result<RelayMutation> {
    let url = validate_relay_url(url)?;
    let transaction = store.transaction()?;
    let mut config = transaction.load_or_create(default_host_name())?;
    config.preferences.relay_url = Some(url);
    config.preferences.relay_enabled = true;
    transaction
        .save(&config)
        .context("saving Pix configuration")?;
    drop(transaction);
    Ok(RelayMutation {
        config,
        restart_required: HostServiceStatus::current(store.path()).is_some(),
    })
}

/// Removes the saved relay endpoint.
pub(crate) fn clear_relay(store: &ConfigStore) -> Result<RelayMutation> {
    let transaction = store.transaction()?;
    let mut config = transaction.load().context("loading Pix configuration")?;
    config.preferences.relay_url = None;
    transaction
        .save(&config)
        .context("saving Pix configuration")?;
    drop(transaction);
    Ok(RelayMutation {
        config,
        restart_required: HostServiceStatus::current(store.path()).is_some(),
    })
}

/// Enables or disables relay transport while retaining its endpoint.
pub(crate) fn set_relay_enabled(store: &ConfigStore, enabled: bool) -> Result<RelayMutation> {
    let transaction = store.transaction()?;
    let mut config = transaction.load().context("loading Pix configuration")?;
    if enabled && config.preferences.relay_url.is_none() {
        bail!("relay is not configured; run `pix relay set <url>` first");
    }
    config.preferences.relay_enabled = enabled;
    transaction
        .save(&config)
        .context("saving Pix configuration")?;
    drop(transaction);
    Ok(RelayMutation {
        config,
        restart_required: HostServiceStatus::current(store.path()).is_some(),
    })
}

/// Validates, probes, and saves an explicit Pi executable.  The config write
/// happens only after the compatibility probe succeeds.
pub(crate) fn set_pi(store: &ConfigStore, path: &std::path::Path) -> Result<PiMutation> {
    let installation = PiProbe::new(Some(path.to_path_buf()))
        .with_environment(HostEnvironment::resolve_for("pi"))
        .inspect()
        .with_context(|| format!("probing Pi at {}", path.display()))?;
    if !installation.is_compatible() {
        bail!(
            "Pi {} is too old. Pix requires Pi {} or newer.",
            installation.version,
            pix_core::pi::MINIMUM_PI_VERSION
        );
    }
    let transaction = store.transaction()?;
    let mut config = transaction.load_or_create(default_host_name())?;
    config.preferences.pi_executable = Some(installation.executable.clone());
    transaction
        .save(&config)
        .context("saving Pix configuration")?;
    drop(transaction);
    Ok(PiMutation {
        executable: installation.executable,
        version: installation.version.to_string(),
        supported: installation.supported,
        restart_required: HostServiceStatus::current(store.path()).is_some(),
    })
}

/// Clears a saved Pi executable and returns to PATH discovery.
pub(crate) fn clear_pi(store: &ConfigStore) -> Result<PiClearMutation> {
    let transaction = store.transaction()?;
    let mut config = transaction.load().context("loading Pix configuration")?;
    config.preferences.pi_executable = None;
    transaction
        .save(&config)
        .context("saving Pix configuration")?;
    drop(transaction);
    Ok(PiClearMutation {
        restart_required: HostServiceStatus::current(store.path()).is_some(),
    })
}

/// Runs one service-manager action without writing human output.  The
/// platform-specific service module remains the sole owner of manager
/// semantics and failure messages.
pub(crate) fn service_action(store: &ConfigStore, action: ServiceAction) -> Result<()> {
    match action {
        ServiceAction::Install => service::install_quiet(store).map(|_| ()),
        ServiceAction::Start => service::start_quiet(store),
        ServiceAction::Stop => service::stop_quiet(store),
        ServiceAction::Restart => service::restart_for_config(store),
        ServiceAction::Uninstall => service::uninstall_quiet(store),
    }
}
