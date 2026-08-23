//! Relay endpoint configuration.

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;

use crate::output::CommandOutput;
use crate::setup_ui::{MenuItem, MenuResult, SetupUi, UiTone};
use crate::status::HostServiceStatus;

#[allow(clippy::too_many_lines)]
pub(crate) fn relay_command(
    store: &ConfigStore,
    command: Option<RelayCommand>,
    output: CommandOutput,
    interactive: bool,
) -> Result<()> {
    let Some(command) = command else {
        if !interactive {
            return Err(usage_error(
                "a relay command is required outside an interactive terminal",
            ));
        }
        return relay_menu(store, output);
    };
    match command {
        RelayCommand::Show => {
            let config = match store.load() {
                Ok(config) => Some(config),
                Err(pix_core::config::ConfigError::Read { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(error) => return Err(error.into()),
            };
            if output.is_json() {
                let data = config.as_ref().map_or_else(
                    || {
                        serde_json::json!({
                            "url": null,
                            "enabled": false,
                            "configured": false,
                            "config_state": "missing",
                            "service_restart_required": false,
                        })
                    },
                    |config| {
                        let mut data = relay_json(config, false);
                        data["config_state"] = serde_json::json!("ready");
                        data
                    },
                );
                return output.success("relay.show", &data);
            }
            let Some(config) = config else {
                println!("relay: not configured");
                return Ok(());
            };
            match &config.preferences.relay_url {
                Some(url) if config.preferences.relay_enabled => {
                    println!("relay: {url} (enabled)");
                }
                Some(url) => println!("relay: {url} (disabled)"),
                None => println!("relay: not configured"),
            }
            Ok(())
        }
        RelayCommand::Set { url } => {
            let url = validate_relay_url(&url)?;
            let transaction = store.transaction()?;
            let mut config = transaction.load_or_create(default_host_name())?;
            config.preferences.relay_url = Some(url.clone());
            config.preferences.relay_enabled = true;
            transaction
                .save(&config)
                .context("saving Pix configuration")?;
            drop(transaction);
            let restart_required = HostServiceStatus::current(store.path()).is_some();
            if output.is_json() {
                return output.success("relay.set", &relay_json(&config, restart_required));
            }
            println!("relay: {url} (enabled)");
            if restart_required {
                println!("  Restart the host with `pix service restart` to apply this change.");
            }
            Ok(())
        }
        RelayCommand::Clear => {
            let transaction = store.transaction()?;
            let mut config = transaction.load().context("loading Pix configuration")?;
            config.preferences.relay_url = None;
            transaction
                .save(&config)
                .context("saving Pix configuration")?;
            drop(transaction);
            let restart_required = HostServiceStatus::current(store.path()).is_some();
            if output.is_json() {
                return output.success("relay.clear", &relay_json(&config, restart_required));
            }
            println!("relay: not configured");
            if restart_required {
                println!("  Restart the host with `pix service restart` to apply this change.");
            }
            Ok(())
        }
        RelayCommand::Enable | RelayCommand::Disable => {
            let enable = matches!(command, RelayCommand::Enable);
            let transaction = store.transaction()?;
            let mut config = transaction.load().context("loading Pix configuration")?;
            if enable && config.preferences.relay_url.is_none() {
                bail!("relay is not configured; run `pix relay set <url>` first");
            }
            config.preferences.relay_enabled = enable;
            transaction
                .save(&config)
                .context("saving Pix configuration")?;
            drop(transaction);
            let restart_required = HostServiceStatus::current(store.path()).is_some();
            if output.is_json() {
                return output.success(
                    if enable {
                        "relay.enable"
                    } else {
                        "relay.disable"
                    },
                    &relay_json(&config, restart_required),
                );
            }
            match (&config.preferences.relay_url, enable) {
                (Some(url), true) => println!("relay: {url} (enabled)"),
                (Some(url), false) => println!("relay: {url} (disabled)"),
                (None, _) => println!("relay: not configured"),
            }
            if restart_required {
                println!("  Restart the host with `pix service restart` to apply this change.");
            }
            Ok(())
        }
    }
}

pub(crate) fn relay_json(
    config: &pix_core::HostConfig,
    restart_required: bool,
) -> serde_json::Value {
    serde_json::json!({
        "url": config.preferences.relay_url,
        "enabled": config.preferences.relay_url.is_some() && config.preferences.relay_enabled,
        "configured": config.preferences.relay_url.is_some(),
        "service_restart_required": restart_required,
    })
}

pub(crate) fn relay_menu(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let config = load_or_ephemeral_config(store)?;
    let ui = SetupUi::new(true, false);
    ui.crumb_header("Relay");
    match (
        &config.preferences.relay_url,
        config.preferences.relay_enabled,
    ) {
        (Some(url), true) => ui.status_row("relay", url, UiTone::Success),
        (Some(url), false) => ui.status_row("relay", url, UiTone::Warning),
        (None, _) => ui.status_row("relay", "not configured", UiTone::Muted),
    }
    println!();
    let mut actions = vec![(
        0_u8,
        MenuItem::new("Configure relay", "Set a ws:// or wss:// endpoint"),
    )];
    if config.preferences.relay_url.is_some() {
        if config.preferences.relay_enabled {
            actions.push((
                1,
                MenuItem::new("Disable relay", "Keep the endpoint but use LAN only"),
            ));
        } else {
            actions.push((2, MenuItem::new("Enable relay", "Resume remote access")));
        }
        actions.push((3, MenuItem::new("Clear relay", "Remove the saved endpoint")));
    }
    actions.push((4, MenuItem::new("Back", "Return to the shell")));
    let items = actions.iter().map(|(_, item)| *item).collect::<Vec<_>>();
    match ui.menu("Actions", &items, 0)? {
        MenuResult::Selected(index) => match actions[index].0 {
            0 => {
                let url = ui.input(
                    "Relay WebSocket URL",
                    config.preferences.relay_url.as_deref(),
                )?;
                relay_command(store, Some(RelayCommand::Set { url }), output, true)
            }
            1 => relay_command(store, Some(RelayCommand::Disable), output, true),
            2 => relay_command(store, Some(RelayCommand::Enable), output, true),
            3 => {
                let choices = vec!["Clear relay".to_owned(), "Cancel".to_owned()];
                if ui.select("Remove the saved relay endpoint?", &choices, 1)? == 0 {
                    relay_command(store, Some(RelayCommand::Clear), output, true)
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

pub(crate) fn validate_relay_url(url: &str) -> Result<String> {
    let value = url.trim();
    pix_core::validate_relay_url(value).context(
        "relay URL must be a valid ws:// or wss:// endpoint without credentials or a fragment",
    )?;
    Ok(value.to_owned())
}

pub(crate) fn display_relay_url(url: &str) -> String {
    url.strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))
        .unwrap_or(url)
        .to_owned()
}

use crate::RelayCommand;
use crate::commands::shared::{default_host_name, load_or_ephemeral_config};
use crate::{print_cli_help, usage_error};
