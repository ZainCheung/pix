//! Pi executable discovery and pinning.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pix_core::{ConfigStore, HostEnvironment, PiProbe};

use crate::commands::pi_bridge;
use crate::output::CommandOutput;
use crate::setup_ui::{MenuItem, MenuResult, SetupUi, UiTone};
use crate::status::HostServiceStatus;

pub(crate) fn configured_pi_version(_store: &ConfigStore, config: &pix_core::HostConfig) -> String {
    PiProbe::new(config.preferences.pi_executable.clone())
        .with_environment(HostEnvironment::resolve_for("pi"))
        .inspect()
        .map_or_else(
            |_| "unavailable".to_owned(),
            |installation| installation.version.to_string(),
        )
}

pub(crate) fn configured_pi_executable(
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

#[allow(clippy::too_many_lines)]
pub(crate) fn pi_command(
    store: &ConfigStore,
    command: Option<PiCommand>,
    output: CommandOutput,
    interactive: bool,
) -> Result<()> {
    let Some(command) = command else {
        if !interactive {
            return Err(usage_error(
                "a Pi command is required outside an interactive terminal",
            ));
        }
        return pi_menu(store, output);
    };
    match command {
        PiCommand::Show => {
            let config = match store.load() {
                Ok(config) => Some(config),
                Err(pix_core::config::ConfigError::Read { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(error) => return Err(error.into()),
            };
            let environment = HostEnvironment::resolve_for("pi");
            let executable = config.as_ref().map_or_else(
                || {
                    PiProbe::new(None)
                        .with_environment(environment.clone())
                        .inspect()
                        .map_or_else(
                            |_| PathBuf::from("pi"),
                            |installation| installation.executable,
                        )
                },
                |config| configured_pi_executable(config, &environment),
            );
            let source = if config
                .as_ref()
                .is_some_and(|config| config.preferences.pi_executable.is_some())
            {
                "configured"
            } else {
                "path"
            };
            if output.is_json() {
                return output.success(
                    "pi.show",
                    &serde_json::json!({
                        "source": source,
                        "executable": executable,
                        "config_state": if config.is_some() { "ready" } else { "missing" },
                    }),
                );
            }
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
            let transaction = store.transaction()?;
            let mut config = transaction.load_or_create(default_host_name())?;
            config.preferences.pi_executable = Some(installation.executable.clone());
            transaction
                .save(&config)
                .context("saving Pix configuration")?;
            drop(transaction);
            let restart_required = HostServiceStatus::current(store.path()).is_some();
            if output.is_json() {
                return output.success(
                    "pi.set",
                    &serde_json::json!({
                        "source": "configured",
                        "executable": installation.executable,
                        "version": installation.version,
                        "supported": installation.supported,
                        "service_restart_required": restart_required,
                    }),
                );
            }
            println!("Using {}", installation.executable.display());
            if restart_required {
                println!("Restart the host with `pix service restart` to use this executable.");
            }
            Ok(())
        }
        PiCommand::Clear => {
            let transaction = store.transaction()?;
            let mut config = transaction.load().context("loading Pix configuration")?;
            config.preferences.pi_executable = None;
            transaction
                .save(&config)
                .context("saving Pix configuration")?;
            drop(transaction);
            let restart_required = HostServiceStatus::current(store.path()).is_some();
            if output.is_json() {
                return output.success(
                    "pi.clear",
                    &serde_json::json!({
                        "source": "path",
                        "executable": null,
                        "service_restart_required": restart_required,
                    }),
                );
            }
            println!("Cleared the saved Pi executable.");
            if restart_required {
                println!("Restart the host with `pix service restart` to apply this change.");
            }
            Ok(())
        }
        PiCommand::Bridge { command } => pi_bridge::bridge_command(store, &command, output),
    }
}

pub(crate) fn pi_menu(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let config = load_or_ephemeral_config(store)?;
    let ui = SetupUi::new(true, false);
    ui.crumb_header("Pi");
    ui.status_row(
        "executable",
        config
            .preferences
            .pi_executable
            .as_deref()
            .map_or("PATH discovery".to_owned(), |path| {
                path.display().to_string()
            })
            .as_str(),
        if config.preferences.pi_executable.is_some() {
            UiTone::Default
        } else {
            UiTone::Muted
        },
    );
    println!();
    let mut actions = vec![
        (
            0_u8,
            MenuItem::new("Show Pi", "Resolve the executable Pix will use"),
        ),
        (
            1,
            MenuItem::new("Choose Pi", "Validate and save an executable path"),
        ),
    ];
    if config.preferences.pi_executable.is_some() {
        actions.push((
            2,
            MenuItem::new("Use PATH discovery", "Clear the saved executable"),
        ));
    }
    actions.push((3, MenuItem::new("Back", "Return to the shell")));
    let items = actions.iter().map(|(_, item)| *item).collect::<Vec<_>>();
    match ui.menu("Actions", &items, 0)? {
        MenuResult::Selected(index) => match actions[index].0 {
            0 => pi_command(store, Some(PiCommand::Show), output, true),
            1 => {
                let path = PathBuf::from(ui.input("Pi executable path", None)?);
                pi_command(store, Some(PiCommand::Set { path }), output, true)
            }
            2 => pi_command(store, Some(PiCommand::Clear), output, true),
            _ => Ok(()),
        },
        MenuResult::Help => print_cli_help(),
        MenuResult::Quit => Ok(()),
    }
}

use crate::PiCommand;
use crate::commands::shared::{default_host_name, load_or_ephemeral_config};
use crate::{print_cli_help, usage_error};
