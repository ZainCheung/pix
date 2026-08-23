use anyhow::Result;
use pix_core::ConfigStore;
use serde::Serialize;

use crate::commands::shared::terminal_label;
use crate::service;
use crate::setup_ui::{DIM, MenuItem, MenuResult, SetupUi, UiTone};
use crate::status::HostServiceStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeAction {
    Setup,
    Devices,
    Workspaces,
    Status,
    Update,
    Settings,
    Commands,
    Quit,
}

const LOGO: &str = r"  _____ _
 |  __ (_)
 | |__) |__  __
 |  ___/ \ \/ /
 | |   | |>  <
 |_|   |_/_/\_";

#[derive(Debug, Serialize)]
pub(crate) struct HostOverview {
    pub(crate) config_path: String,
    pub(crate) config_state: ConfigState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<String>,
    pub(crate) pi: PiOverview,
    pub(crate) service: ServiceOverview,
    pub(crate) access: AccessOverview,
    pub(crate) devices: usize,
    pub(crate) workspaces: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct PiOverview {
    pub(crate) source: PiSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) executable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) supported: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ServiceOverview {
    pub(crate) state: ServiceState,
    pub(crate) installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AccessOverview {
    pub(crate) mode: AccessMode,
    pub(crate) relay_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) relay_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConfigState {
    Ready,
    Missing,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PiSource {
    Configured,
    Path,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ServiceState {
    Running,
    Stopped,
    NotInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccessMode {
    Relay,
    RelayDisabled,
    Local,
    Unknown,
}

impl HostOverview {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn collect(store: &ConfigStore) -> Self {
        let current_service = HostServiceStatus::current(store.path());
        let service_installed =
            current_service.is_some() || service::managed_service_installed(store).unwrap_or(false);
        let service = current_service.map_or_else(
            || ServiceOverview {
                state: if service_installed {
                    ServiceState::Stopped
                } else {
                    ServiceState::NotInstalled
                },
                installed: service_installed,
                pid: None,
                port: None,
                started_at: None,
            },
            |status| ServiceOverview {
                state: ServiceState::Running,
                installed: service_installed,
                pid: Some(status.pid),
                port: Some(status.port),
                started_at: Some(status.started_at),
            },
        );

        match store.load() {
            Ok(config) => {
                let mut pi = config.preferences.pi_executable.as_ref().map_or_else(
                    || PiOverview {
                        source: PiSource::Path,
                        executable: None,
                        version: None,
                        supported: None,
                    },
                    |path| PiOverview {
                        source: PiSource::Configured,
                        executable: Some(path.display().to_string()),
                        version: None,
                        supported: None,
                    },
                );
                // Status absorbs the doctor probe: the overview reports the
                // Pi pix actually runs, not just what the config points at.
                let environment = pix_core::HostEnvironment::resolve_for("pi");
                if let Ok(installation) =
                    pix_core::PiProbe::new(config.preferences.pi_executable.clone())
                        .with_environment(environment)
                        .inspect()
                {
                    pi.executable = Some(installation.executable.display().to_string());
                    pi.version = Some(installation.version.to_string());
                    pi.supported = Some(installation.supported);
                }
                let access = match &config.preferences.relay_url {
                    Some(url) if config.preferences.relay_enabled => AccessOverview {
                        mode: AccessMode::Relay,
                        relay_enabled: true,
                        relay_url: Some(url.clone()),
                    },
                    Some(url) => AccessOverview {
                        mode: AccessMode::RelayDisabled,
                        relay_enabled: false,
                        relay_url: Some(url.clone()),
                    },
                    None => AccessOverview {
                        mode: AccessMode::Local,
                        relay_enabled: false,
                        relay_url: None,
                    },
                };
                Self {
                    config_path: store.path().display().to_string(),
                    config_state: ConfigState::Ready,
                    config_error: None,
                    host: Some(config.host.display_name),
                    pi,
                    service,
                    access,
                    devices: config.devices.len(),
                    workspaces: config.workspaces.len(),
                }
            }
            Err(pix_core::config::ConfigError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Self {
                    config_path: store.path().display().to_string(),
                    config_state: ConfigState::Missing,
                    config_error: None,
                    host: None,
                    pi: PiOverview {
                        source: PiSource::Path,
                        executable: None,
                        version: None,
                        supported: None,
                    },
                    service,
                    access: AccessOverview {
                        mode: AccessMode::Local,
                        relay_enabled: false,
                        relay_url: None,
                    },
                    devices: 0,
                    workspaces: 0,
                }
            }
            Err(error) => Self {
                config_path: store.path().display().to_string(),
                config_state: ConfigState::Invalid,
                config_error: Some(error.to_string()),
                host: None,
                pi: PiOverview {
                    source: PiSource::Unknown,
                    executable: None,
                    version: None,
                    supported: None,
                },
                service,
                access: AccessOverview {
                    mode: AccessMode::Unknown,
                    relay_enabled: false,
                    relay_url: None,
                },
                devices: 0,
                workspaces: 0,
            },
        }
    }
}

pub(crate) fn run(overview: &HostOverview, ui: SetupUi) -> Result<HomeAction> {
    render_overview(overview, ui, false);
    let (actions, default) = match overview.config_state {
        ConfigState::Ready => (
            vec![
                (
                    HomeAction::Devices,
                    MenuItem::new("Devices", "Pair, approve, or revoke a phone"),
                ),
                (
                    HomeAction::Workspaces,
                    MenuItem::new("Workspaces", "Authorize or remove host folders"),
                ),
                (
                    HomeAction::Status,
                    MenuItem::new("Status", "Show detailed host state"),
                ),
                (
                    HomeAction::Update,
                    MenuItem::new("Update", "Upgrade pix from the latest release"),
                ),
                (
                    HomeAction::Settings,
                    MenuItem::new("Settings", "Configure remote access"),
                ),
                (
                    HomeAction::Quit,
                    MenuItem::new("Quit", "Return to the shell"),
                ),
            ],
            0,
        ),
        ConfigState::Missing => (
            vec![
                (
                    HomeAction::Setup,
                    MenuItem::new("Setup", "Prepare this computer for remote Pi access"),
                ),
                (
                    HomeAction::Devices,
                    MenuItem::new("Devices", "Pair, approve, or revoke a phone"),
                ),
                (
                    HomeAction::Workspaces,
                    MenuItem::new("Workspaces", "Authorize or remove host folders"),
                ),
                (
                    HomeAction::Status,
                    MenuItem::new("Status", "Show detailed host state"),
                ),
                (
                    HomeAction::Update,
                    MenuItem::new("Update", "Upgrade pix from the latest release"),
                ),
                (
                    HomeAction::Settings,
                    MenuItem::new("Settings", "Configure remote access"),
                ),
                (
                    HomeAction::Quit,
                    MenuItem::new("Quit", "Return to the shell"),
                ),
            ],
            0,
        ),
        ConfigState::Invalid => (
            vec![
                (
                    HomeAction::Status,
                    MenuItem::new("Status", "Inspect the invalid host configuration"),
                ),
                (
                    HomeAction::Setup,
                    MenuItem::new("Repair setup", "Review setup after diagnosing the error"),
                ),
                (
                    HomeAction::Update,
                    MenuItem::new("Update", "Upgrade pix from the latest release"),
                ),
                (
                    HomeAction::Commands,
                    MenuItem::new("Show commands", "Open the complete CLI reference"),
                ),
                (
                    HomeAction::Quit,
                    MenuItem::new("Quit", "Return to the shell"),
                ),
            ],
            0,
        ),
    };
    let items = actions.iter().map(|(_, item)| *item).collect::<Vec<_>>();
    match ui.menu("Actions", &items, default)? {
        MenuResult::Selected(index) => Ok(actions[index].0),
        MenuResult::Help => Ok(HomeAction::Commands),
        MenuResult::Quit => Ok(HomeAction::Quit),
    }
}

pub(crate) fn render_overview(overview: &HostOverview, ui: SetupUi, detailed: bool) {
    if detailed {
        ui.crumb_header("Status");
    } else {
        println!();
        for line in LOGO.lines() {
            println!("  {}", ui.cyan(line, true));
        }
        println!(
            "  {}",
            ui.paint(concat!("pix ", env!("CARGO_PKG_VERSION")), DIM, false)
        );
        println!();
    }

    match overview.config_state {
        ConfigState::Ready => ui.status_row(
            "host",
            overview.host.as_deref().unwrap_or("Pix Host"),
            UiTone::Default,
        ),
        ConfigState::Missing => ui.status_row("host", "not configured", UiTone::Warning),
        ConfigState::Invalid => {
            ui.status_row("host", "configuration needs attention", UiTone::Danger);
        }
    }
    let pi = match (overview.pi.source, overview.pi.executable.as_deref()) {
        (PiSource::Configured, Some(path)) if detailed => {
            format!("configured ({})", terminal_label(path))
        }
        (PiSource::Configured, _) => "configured executable".to_owned(),
        (PiSource::Path, _) => "PATH discovery".to_owned(),
        _ => "unknown".to_owned(),
    };
    let pi = match &overview.pi.version {
        Some(version) if overview.pi.supported.unwrap_or(true) => format!("{pi} · {version}"),
        Some(version) => format!("{pi} · {version} (unsupported)"),
        None => pi,
    };
    ui.status_row("pi", &pi, UiTone::Muted);
    match overview.service.state {
        ServiceState::Running => ui.status_row("service", "● running", UiTone::Success),
        ServiceState::Stopped => ui.status_row("service", "○ installed, stopped", UiTone::Warning),
        ServiceState::NotInstalled => ui.status_row("service", "○ not installed", UiTone::Muted),
    }
    match overview.access.mode {
        AccessMode::Relay => ui.status_row("access", "Pix Relay enabled", UiTone::Accent),
        AccessMode::RelayDisabled => ui.status_row("access", "Pix Relay disabled", UiTone::Warning),
        AccessMode::Local => ui.status_row("access", "local network only", UiTone::Muted),
        AccessMode::Unknown => ui.status_row("access", "unknown", UiTone::Danger),
    }
    ui.status_row(
        "devices",
        &format!("{} paired", overview.devices),
        if overview.devices == 0 {
            UiTone::Warning
        } else {
            UiTone::Default
        },
    );
    ui.status_row(
        "workspaces",
        &format!("{} authorized", overview.workspaces),
        if overview.workspaces == 0 {
            UiTone::Warning
        } else {
            UiTone::Default
        },
    );
    if detailed {
        println!();
        ui.status_row(
            "config",
            &terminal_label(&overview.config_path),
            UiTone::Muted,
        );
        if let Some(error) = &overview.config_error {
            ui.status_row("error", &terminal_label(error), UiTone::Danger);
        }
        if let (Some(pid), Some(port)) = (overview.service.pid, overview.service.port) {
            ui.status_row(
                "listener",
                &format!("pid {pid}, port {port}"),
                UiTone::Muted,
            );
        }
        if let Some(url) = &overview.access.relay_url {
            ui.status_row("relay", &terminal_label(url), UiTone::Muted);
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use pix_core::{ConfigStore, HostConfig};
    use tempfile::tempdir;

    use super::{ConfigState, HostOverview};

    #[test]
    fn opening_home_does_not_create_configuration() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.json");
        let store = ConfigStore::new(&path);

        let overview = HostOverview::collect(&store);

        assert_eq!(overview.config_state, ConfigState::Missing);
        assert!(!path.exists());
    }

    #[test]
    fn overview_exposes_counts_without_device_secrets() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.json");
        let store = ConfigStore::new(&path);
        let config = HostConfig::new("Studio Mac");
        store.save(&config).expect("save config");

        let overview = HostOverview::collect(&store);
        let json = serde_json::to_value(&overview).expect("serialize overview");

        assert_eq!(json["host"], "Studio Mac");
        assert_eq!(json["devices"], 0);
        assert!(json.get("public_key").is_none());
        assert!(json.get("relay_channel").is_none());
    }
}
