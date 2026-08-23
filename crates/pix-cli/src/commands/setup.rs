//! The first-use setup wizard.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pix_core::{ConfigStore, HostEnvironment, PiProbe, WorkspaceRegistry};

use crate::setup_ui::SetupUi;
use crate::status::HostServiceStatus;

pub(crate) fn default_setup_options() -> SetupOptions {
    SetupOptions {
        relay: None,
        workspace: None,
        workspace_name: None,
        no_pair: false,
        no_service: false,
        yes: false,
        non_interactive: false,
        verbose: false,
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct SetupOptions {
    pub(crate) relay: Option<String>,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) workspace_name: Option<String>,
    pub(crate) no_pair: bool,
    pub(crate) no_service: bool,
    pub(crate) yes: bool,
    pub(crate) non_interactive: bool,
    pub(crate) verbose: bool,
}

#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct SetupPairingOptions {
    pub(crate) remote: bool,
    pub(crate) yes: bool,
    pub(crate) interactive: bool,
    pub(crate) ui: SetupUi,
    pub(crate) keep_service: bool,
}

/// Runs the product-facing first-use flow while keeping the existing
/// subsystem commands available for diagnostics and automation.
pub(crate) fn setup(store: &ConfigStore, options: &SetupOptions) -> Result<()> {
    let interactive = !options.non_interactive
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal();
    if !interactive && !options.non_interactive {
        eprintln!(
            "Pix setup needs a terminal for prompts; pass --non-interactive with \
             --workspace and --no-pair, or run it from a terminal."
        );
    }
    if !interactive && !options.no_pair && !options.yes {
        bail!("non-interactive setup cannot approve a phone; pass --no-pair or --yes");
    }

    let ui = SetupUi::new(interactive, options.verbose);
    let config_was_present = store.path().is_file();
    let config = store
        .load_or_create(default_host_name())
        .context("loading Pix configuration")?;

    if config_was_present
        && setup_is_already_configured(store, &config)
        && !options.non_interactive
        && options.relay.is_none()
        && options.workspace.is_none()
        && !options.no_pair
        && !options.no_service
    {
        return setup_existing(store, config, options, ui);
    }

    let started_at = std::time::Instant::now();
    let mode =
        if interactive && options.relay.is_none() && options.workspace.is_none() && !options.yes {
            setup_welcome(ui)?
        } else {
            SetupMode::Quick
        };

    match mode {
        SetupMode::Quick => setup_quick(store, config, options, ui, started_at),
        SetupMode::Advanced => setup_advanced(store, config, options, ui, started_at),
        SetupMode::Exit => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupMode {
    Quick,
    Advanced,
    Exit,
}

pub(crate) fn setup_welcome(ui: SetupUi) -> Result<SetupMode> {
    ui.brand_header(None);
    ui.hint("Remote access for Pi");
    ui.body("Set up this computer so you can use Pi from your phone.");
    let options = vec![
        "Quick setup".to_owned(),
        "Advanced setup".to_owned(),
        "Exit".to_owned(),
    ];
    let selected = ui.select("How would you like to set up Pix?", &options, 0)?;
    Ok(match selected {
        1 => SetupMode::Advanced,
        2 => SetupMode::Exit,
        _ => SetupMode::Quick,
    })
}

pub(crate) fn setup_is_already_configured(
    store: &ConfigStore,
    config: &pix_core::HostConfig,
) -> bool {
    !config.workspaces.is_empty()
        || !config.devices.is_empty()
        || config.preferences.active_relay_url().is_some()
        || HostServiceStatus::current(store.path()).is_some()
}

#[allow(clippy::too_many_lines)]
pub(crate) fn setup_existing(
    store: &ConfigStore,
    config: pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
) -> Result<()> {
    ui.brand_header(None);
    ui.section("Pix is already set up on this computer");
    ui.success(
        &format!("Pi {}", configured_pi_version(store, &config)),
        None,
    );
    ui.success(
        &format!(
            "{} paired device{}",
            config.devices.len(),
            plural(config.devices.len())
        ),
        None,
    );
    ui.success(
        &format!(
            "{} workspace{}",
            config.workspaces.len(),
            plural(config.workspaces.len())
        ),
        None,
    );
    if let Some(relay) = config.preferences.active_relay_url() {
        ui.success("Relay configured", Some(&display_relay_url(relay)));
    } else {
        ui.muted("○ Local network only");
    }
    if HostServiceStatus::current(store.path()).is_some() {
        ui.success("Background service running", None);
    }

    if !ui.interactive() {
        return verify_setup(
            store,
            &config,
            ui,
            configured_pi_version(store, &config),
            config.preferences.active_relay_url().map(str::to_owned),
            false,
            std::time::Duration::ZERO,
        );
    }

    let choices = vec![
        "Check setup".to_owned(),
        "Pair another device".to_owned(),
        "Add workspace".to_owned(),
        "Reconfigure Pix".to_owned(),
        "Exit".to_owned(),
    ];
    let selected = ui.select("What would you like to do?", &choices, 0)?;
    match selected {
        1 => {
            let mut config = config;
            let relay = config.preferences.active_relay_url().map(str::to_owned);
            let _relay = run_setup_pairing_with_recovery(
                store,
                &mut config,
                relay,
                options.yes,
                true,
                ui,
                !options.no_service,
            )?;
            let service = install_setup_service(store, options.no_service, ui)?;
            let final_config = store.load().context("reloading setup configuration")?;
            verify_setup(
                store,
                &final_config,
                ui,
                configured_pi_version(store, &final_config),
                final_config
                    .preferences
                    .active_relay_url()
                    .map(str::to_owned),
                service,
                std::time::Duration::ZERO,
            )
        }
        2 => {
            let mut config = config;
            let baseline = config.clone();
            let mut existing_options = SetupOptions {
                relay: None,
                workspace: None,
                workspace_name: None,
                no_pair: true,
                no_service: true,
                yes: false,
                non_interactive: false,
                verbose: options.verbose,
            };
            configure_setup_workspace(&mut config, &existing_options, ui, true, true)?;
            commit_setup_draft(store, &baseline, &mut config)?;
            existing_options.no_pair = false;
            existing_options.no_service = false;
            verify_setup(
                store,
                &config,
                ui,
                configured_pi_version(store, &config),
                config.preferences.active_relay_url().map(str::to_owned),
                false,
                std::time::Duration::ZERO,
            )
        }
        3 => {
            let mut config = config;
            let baseline = config.clone();
            let relay = configure_setup_relay(&mut config, options, ui, true)?;
            configure_setup_workspace(&mut config, options, ui, true, true)?;
            commit_setup_draft(store, &baseline, &mut config)?;
            verify_setup(
                store,
                &config,
                ui,
                configured_pi_version(store, &config),
                relay,
                false,
                std::time::Duration::ZERO,
            )
        }
        0 => verify_setup(
            store,
            &config,
            ui,
            configured_pi_version(store, &config),
            config.preferences.active_relay_url().map(str::to_owned),
            false,
            std::time::Duration::ZERO,
        ),
        _ => Ok(()),
    }
}

/// Commits setup's explicit host/preference choices and newly added
/// workspaces onto the latest durable snapshot. Device trust and concurrent
/// workspace removals always come from that latest snapshot, so a long-lived
/// wizard cannot resurrect a revoked device or authorization.
pub(crate) fn commit_setup_draft(
    store: &ConfigStore,
    baseline: &pix_core::HostConfig,
    draft: &mut pix_core::HostConfig,
) -> Result<()> {
    let service_was_live = host_service_control_live(store)?;
    prepare_running_service_mutation(store)?;
    let restart_required = draft.host.display_name != baseline.host.display_name
        || draft.preferences != baseline.preferences;
    let additions = draft
        .workspaces
        .iter()
        .filter(|candidate| {
            !baseline
                .workspaces
                .iter()
                .any(|workspace| workspace.id == candidate.id || workspace.path == candidate.path)
        })
        .cloned()
        .collect::<Vec<_>>();
    let transaction = store.transaction()?;
    let mut current = transaction.load_or_create(draft.host.display_name.clone())?;
    if draft.host.display_name != baseline.host.display_name {
        current
            .host
            .display_name
            .clone_from(&draft.host.display_name);
    }
    if draft.preferences.relay_enabled != baseline.preferences.relay_enabled {
        current.preferences.relay_enabled = draft.preferences.relay_enabled;
    }
    if draft.preferences.relay_url != baseline.preferences.relay_url {
        current
            .preferences
            .relay_url
            .clone_from(&draft.preferences.relay_url);
    }
    if draft.preferences.pi_executable != baseline.preferences.pi_executable {
        current
            .preferences
            .pi_executable
            .clone_from(&draft.preferences.pi_executable);
    }
    if draft.preferences.idle_timeout_seconds != baseline.preferences.idle_timeout_seconds {
        current.preferences.idle_timeout_seconds = draft.preferences.idle_timeout_seconds;
    }
    if draft.preferences.max_active_sessions != baseline.preferences.max_active_sessions {
        current.preferences.max_active_sessions = draft.preferences.max_active_sessions;
    }
    if draft.preferences.max_concurrent_turns != baseline.preferences.max_concurrent_turns {
        current.preferences.max_concurrent_turns = draft.preferences.max_concurrent_turns;
    }
    for workspace in additions {
        if !current
            .workspaces
            .iter()
            .any(|existing| existing.id == workspace.id || existing.path == workspace.path)
        {
            current.workspaces.push(workspace);
        }
    }
    transaction
        .save(&current)
        .context("saving setup configuration")?;
    drop(transaction);
    if service_was_live && restart_required {
        restart_or_stop_for_configuration_change(store)?;
    } else {
        refresh_running_service(store)?;
    }
    *draft = current;
    Ok(())
}

pub(crate) fn setup_quick(
    store: &ConfigStore,
    mut config: pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
    started_at: std::time::Instant,
) -> Result<()> {
    let baseline = config.clone();
    if ui.interactive() {
        ui.crumb_header("Setup");
        ui.section("Checking this computer");
    }
    let pi_version = prepare_setup_environment(store, &mut config, options, ui)?;
    let relay = configure_setup_relay(&mut config, options, ui, false)?;
    configure_setup_workspace(&mut config, options, ui, ui.interactive(), false)?;
    commit_setup_draft(store, &baseline, &mut config)?;

    let relay = if config.devices.is_empty() && !options.no_pair {
        run_setup_pairing_with_recovery(
            store,
            &mut config,
            relay,
            options.yes,
            ui.interactive(),
            ui,
            !options.no_service,
        )?
    } else if config.devices.is_empty() {
        if ui.interactive() {
            ui.muted("○ Device pairing skipped");
        } else {
            println!("Pairing... skipped");
        }
        relay
    } else if ui.interactive() {
        ui.success(
            &format!(
                "{} paired device{} already configured",
                config.devices.len(),
                plural(config.devices.len())
            ),
            None,
        );
        relay
    } else {
        println!(
            "Pairing... {} device{} already configured",
            config.devices.len(),
            plural(config.devices.len())
        );
        relay
    };

    let service = install_setup_service(store, options.no_service, ui)?;
    let final_config = store.load().context("reloading setup configuration")?;
    verify_setup(
        store,
        &final_config,
        ui,
        pi_version,
        relay,
        service,
        started_at.elapsed(),
    )
}

pub(crate) fn prepare_setup_environment(
    store: &ConfigStore,
    config: &mut pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
) -> Result<String> {
    let environment = HostEnvironment::resolve_for("pi");
    loop {
        if ui.interactive() {
            ui.task("Looking for Pi...");
        } else {
            ui.task("Checking Pi");
        }
        let result = PiProbe::new(config.preferences.pi_executable.clone())
            .with_environment(environment.clone())
            .inspect();
        match result {
            Ok(installation) if installation.supported => {
                ui.task_done(&format!("Pi {}", installation.version));
                if options.verbose {
                    ui.hint(&format!(
                        "Executable: {}",
                        installation.executable.display()
                    ));
                }
                if ui.interactive() {
                    ui.task("Preparing host identity...");
                } else {
                    ui.task("Host identity");
                }
                let _identity =
                    load_host_identity(store, config.host.id).context("preparing host identity")?;
                ui.task_done("Host identity ready");
                if options.verbose {
                    ui.hint(&format!(
                        "Identity store: {}",
                        host_identity_path(store).display()
                    ));
                }
                return Ok(installation.version.to_string());
            }
            Ok(installation) => {
                ui.task_failed(&format!("Pi {} is not supported", installation.version));
                if !ui.interactive() {
                    bail!(
                        "Pi {} is outside the currently verified range {}",
                        installation.version,
                        pix_core::pi::SUPPORTED_PI_VERSION
                    );
                }
                ui.error(
                    "This Pix build supports a different Pi version",
                    Some(pix_core::pi::SUPPORTED_PI_VERSION),
                );
            }
            Err(error) => {
                ui.task_failed("Pi was not found");
                if !ui.interactive() {
                    return Err(anyhow::Error::new(error).context("checking Pi"));
                }
                let not_found = matches!(&error, pix_core::pi::PiError::NotFound);
                if not_found {
                    ui.error(
                        "Pi was not found",
                        Some(
                            "Make sure `pi` is available in your PATH, or select an executable manually.",
                        ),
                    );
                } else {
                    ui.error(
                        "Pix couldn't verify Pi",
                        Some("Run `pix doctor --verbose` for more details."),
                    );
                }
            }
        }

        let choices = vec![
            "Try again".to_owned(),
            "Choose Pi executable".to_owned(),
            "Exit".to_owned(),
        ];
        match ui.select("Pi setup", &choices, 0)? {
            1 => {
                let current = config
                    .preferences
                    .pi_executable
                    .as_deref()
                    .map(|path| path.to_string_lossy().into_owned());
                let path = ui.input("Pi executable", current.as_deref())?;
                if path.trim().is_empty() {
                    ui.warning("Pi executable is required", None);
                } else {
                    config.preferences.pi_executable = Some(PathBuf::from(path));
                }
            }
            2 => bail!("setup cancelled"),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn setup_advanced(
    store: &ConfigStore,
    mut config: pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
    started_at: std::time::Instant,
) -> Result<()> {
    let baseline = config.clone();
    ui.crumb_header("Advanced setup");

    let host_name = ui.input("Host name", Some(&config.host.display_name))?;
    if !host_name.trim().is_empty() {
        config.host.display_name = host_name;
    }

    ui.section("Pi");
    let mut pi_choices = vec!["Auto-detect from PATH".to_owned()];
    if let Some(path) = &config.preferences.pi_executable {
        pi_choices.push(path.display().to_string());
    }
    pi_choices.push("Choose another executable...".to_owned());
    let pi_choice = ui.select("Pi executable", &pi_choices, 0)?;
    if pi_choice == pi_choices.len() - 1 {
        let current_pi = config
            .preferences
            .pi_executable
            .as_deref()
            .map(|value| value.to_string_lossy().into_owned());
        let path = ui.input("Pi executable", current_pi.as_deref())?;
        if !path.trim().is_empty() {
            config.preferences.pi_executable = Some(PathBuf::from(path));
        }
    } else if pi_choice == 0 {
        config.preferences.pi_executable = None;
    }

    ui.section("Connectivity");
    let connectivity = ui.multiselect(
        "Choose connection methods:",
        &[
            "Local network".to_owned(),
            "Relay".to_owned(),
            "Custom direct endpoint".to_owned(),
        ],
        &[true, true, false],
    )?;
    if connectivity.get(2).copied().unwrap_or(false) {
        ui.warning(
            "Custom direct endpoints are not available yet",
            Some("Pix will continue with the selected local and relay methods."),
        );
    }
    let relay = if connectivity.get(1).copied().unwrap_or(false) {
        let default = config
            .preferences
            .active_relay_url()
            .unwrap_or(DEFAULT_RELAY_URL);
        let value = ui.input("Relay URL", Some(default))?;
        let relay = validate_relay_url(&value)?;
        config.preferences.relay_url = Some(relay.clone());
        config.preferences.relay_enabled = true;
        Some(relay)
    } else {
        config.preferences.relay_enabled = false;
        None
    };

    ui.section("Workspace access");
    let candidates = workspace_candidates();
    let mut workspace_options = candidates
        .iter()
        .map(|(path, label)| format!("{}  {}", display_workspace_path(path), label))
        .collect::<Vec<_>>();
    workspace_options.push("Add another path...".to_owned());
    let mut defaults = vec![false; workspace_options.len()];
    if !candidates.is_empty() {
        defaults[0] = true;
    }
    let selected = ui.multiselect(
        "Select folders Pix can access:",
        &workspace_options,
        &defaults,
    )?;
    for (index, checked) in selected.iter().enumerate() {
        if !checked {
            continue;
        }
        if index == candidates.len() {
            let path = select_workspace_path(ui)?;
            add_workspace(&mut config, path, None, ui)?;
        } else if let Some((path, _)) = candidates.get(index) {
            add_workspace(&mut config, path.clone(), None, ui)?;
        }
    }
    if config.workspaces.is_empty() {
        let path = select_workspace_path(ui)?;
        add_workspace(&mut config, path, None, ui)?;
    }

    ui.section("Background service");
    let service_choices = vec![
        "Yes, recommended".to_owned(),
        "No, I'll run Pix manually".to_owned(),
    ];
    let install_service = !options.no_service
        && ui.select(
            "Start Pix automatically when this computer boots?",
            &service_choices,
            0,
        )? == 0;

    ui.section("Review");
    ui.hint(&format!(
        "Host\n  {}",
        terminal_label(&config.host.display_name)
    ));
    ui.hint(&format!(
        "Pi\n  {}",
        config
            .preferences
            .pi_executable
            .as_deref()
            .map_or("Auto-detect from PATH".to_owned(), |path| path
                .display()
                .to_string())
    ));
    ui.hint(&format!(
        "Connectivity\n  {}",
        if relay.is_some() {
            "Local network\n  Pix Relay"
        } else {
            "Local network"
        }
    ));
    ui.hint(&format!(
        "Workspaces\n  {}",
        config
            .workspaces
            .iter()
            .map(|item| display_workspace_path(&item.path))
            .collect::<Vec<_>>()
            .join("\n  ")
    ));
    ui.hint(&format!(
        "Background service\n  {}",
        if install_service {
            "Enabled"
        } else {
            "Disabled"
        }
    ));
    let review_choices = vec![
        "Continue".to_owned(),
        "Go back".to_owned(),
        "Cancel".to_owned(),
    ];
    match ui.select("", &review_choices, 0)? {
        1 => {
            let mut quick_options = options.clone();
            quick_options.no_service = !install_service;
            return setup_quick(store, config, &quick_options, ui, started_at);
        }
        2 => bail!("setup cancelled"),
        _ => {}
    }

    if ui.interactive() {
        ui.section("Checking this computer");
    }
    let pi_version = prepare_setup_environment(store, &mut config, options, ui)?;
    commit_setup_draft(store, &baseline, &mut config)?;
    let relay = if config.devices.is_empty() && !options.no_pair {
        run_setup_pairing_with_recovery(
            store,
            &mut config,
            relay.clone(),
            options.yes,
            true,
            ui,
            install_service,
        )?
    } else {
        relay
    };
    let service = if install_service {
        install_setup_service(store, false, ui)?
    } else {
        install_setup_service(store, true, ui)?
    };
    let final_config = store.load().context("reloading setup configuration")?;
    verify_setup(
        store,
        &final_config,
        ui,
        pi_version,
        relay,
        service,
        started_at.elapsed(),
    )
}

pub(crate) fn configure_setup_relay(
    config: &mut pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
    reconfigure: bool,
) -> Result<Option<String>> {
    if let Some(url) = options.relay.as_deref() {
        let relay = validate_relay_url(url)?;
        config.preferences.relay_url = Some(relay.clone());
        config.preferences.relay_enabled = true;
        if ui.interactive() {
            ui.success("Pix Relay", Some(&display_relay_url(&relay)));
        } else {
            println!("Relay... ok");
        }
        return Ok(Some(relay));
    }

    if !reconfigure && let Some(url) = config.preferences.active_relay_url() {
        let relay = url.to_owned();
        if ui.interactive() {
            ui.section("Remote access");
            ui.success("Pix Relay", Some(&display_relay_url(&relay)));
        } else {
            println!("Relay... ok");
        }
        return Ok(Some(relay));
    }

    if !ui.interactive() {
        config.preferences.relay_enabled = false;
        println!("Relay... skipped");
        return Ok(None);
    }

    ui.section("Remote access");
    ui.body("How should Pix reach this computer when you're away?");
    let choices = vec![
        "Pix Relay                     Recommended".to_owned(),
        "Local network only".to_owned(),
        "Custom relay".to_owned(),
    ];
    let selected = ui.select("", &choices, 0)?;
    let relay = match selected {
        1 => {
            config.preferences.relay_enabled = false;
            None
        }
        2 => loop {
            let value = ui.input("Relay URL", None)?;
            match validate_relay_url(&value) {
                Ok(url) => break Some(url),
                Err(_) => ui.error(
                    "Pix relay URL is invalid",
                    Some("Use wss:// for a relay, or ws:// for a local endpoint."),
                ),
            }
        },
        _ => Some(DEFAULT_RELAY_URL.to_owned()),
    };
    if let Some(url) = &relay {
        config.preferences.relay_url = Some(url.clone());
        config.preferences.relay_enabled = true;
        ui.success("Pix Relay selected", Some(&display_relay_url(url)));
    } else {
        ui.success("Local network only", None);
    }
    Ok(relay)
}

pub(crate) fn configure_setup_workspace(
    config: &mut pix_core::HostConfig,
    options: &SetupOptions,
    ui: SetupUi,
    interactive: bool,
    reconfigure: bool,
) -> Result<Option<String>> {
    let needs_workspace =
        options.workspace.is_some() || config.workspaces.is_empty() || reconfigure;
    if !needs_workspace {
        let workspace = config
            .workspaces
            .first()
            .map(|item| display_workspace_path(&item.path));
        if interactive {
            ui.section("Workspace access");
            ui.success(
                &format!(
                    "{} authorized workspace{}",
                    config.workspaces.len(),
                    plural(config.workspaces.len())
                ),
                workspace.as_deref(),
            );
        } else {
            println!("Workspace... ok");
        }
        return Ok(workspace);
    }

    let path = match options.workspace.clone() {
        Some(path) => expand_home(path),
        None if interactive => select_workspace_path(ui)?,
        None => bail!("setup needs --workspace when no authorized workspace exists"),
    };
    let canonical = std::fs::canonicalize(&path)
        .with_context(|| format!("resolving workspace {}", path.display()))?;
    if let Some(existing) = config
        .workspaces
        .iter()
        .find(|workspace| workspace.path == canonical)
    {
        if interactive {
            ui.success(
                "Workspace already authorized",
                Some(&display_workspace_path(&existing.path)),
            );
        } else {
            println!("Workspace... ok");
        }
        return Ok(Some(display_workspace_path(&existing.path)));
    }
    let name = options.workspace_name.clone();
    let mut registry = WorkspaceRegistry::new(config);
    let added = registry
        .add(&canonical, name)
        .with_context(|| format!("authorizing workspace {}", path.display()))?
        .clone();
    let displayed = display_workspace_path(&added.path);
    if interactive {
        ui.success("Added workspace", Some(&displayed));
    } else {
        println!("Workspace... ok");
    }
    Ok(Some(displayed))
}

pub(crate) fn select_workspace_path(ui: SetupUi) -> Result<PathBuf> {
    let candidates = workspace_candidates();
    let mut options = candidates
        .iter()
        .map(|(path, label)| format!("{:<36} {}", display_workspace_path(path), label))
        .collect::<Vec<_>>();
    options.push("Enter another path...".to_owned());
    let selected = ui.select("Choose your first workspace:", &options, 0)?;
    if selected < candidates.len() {
        return Ok(candidates[selected].0.clone());
    }
    loop {
        let value = ui.input("Workspace path", None)?;
        let path = expand_home(PathBuf::from(value));
        match std::fs::canonicalize(&path) {
            Ok(path) if path.is_dir() => return Ok(path),
            Ok(_) => ui.error("Workspace must be a folder", None),
            Err(_) => ui.error(
                "Workspace path was not found",
                Some("Choose an existing folder and try again."),
            ),
        }
    }
}

pub(crate) fn workspace_candidates() -> Vec<(PathBuf, &'static str)> {
    let mut candidates = Vec::new();
    let mut add = |path: PathBuf, label: &'static str| {
        let Ok(canonical) = std::fs::canonicalize(path) else {
            return;
        };
        if !canonical.is_dir() || candidates.iter().any(|(item, _)| item == &canonical) {
            return;
        }
        candidates.push((canonical, label));
    };
    if let Ok(current) = std::env::current_dir() {
        let git_root = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&current)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| PathBuf::from(value.trim()));
        if let Some(root) = git_root {
            add(root, "git repository");
        }
        add(current.clone(), "current directory");
        if let Some(parent) = current.parent() {
            add(parent.to_path_buf(), "parent directory");
        }
    }
    if let Some(home) = home_directory() {
        add(home, "home directory");
    }
    candidates
}

#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
pub(crate) fn verify_setup(
    store: &ConfigStore,
    config: &pix_core::HostConfig,
    ui: SetupUi,
    pi_version: String,
    relay: Option<String>,
    service_installed: bool,
    elapsed: std::time::Duration,
) -> Result<()> {
    if !ui.interactive() {
        println!("Verifying setup... ok");
        println!("Setup complete.");
        return Ok(());
    }

    ui.crumb_header("Setup");
    ui.section("Verifying setup");
    ui.task("Checking host service...");
    let running = service_installed || HostServiceStatus::current(store.path()).is_some();
    if running {
        ui.task_done("Host service running");
    } else {
        ui.task_failed("Host service not running");
        ui.warning(
            "You can still run Pix manually",
            Some("Start it with `pix serve`."),
        );
    }
    ui.task("Checking connection...");
    if relay.is_some() {
        ui.task_done("Relay configured");
    } else {
        ui.task_done("Local network ready");
    }
    if config.devices.is_empty() {
        ui.muted("○ iPhone offline");
    } else {
        ui.success(
            &format!(
                "{} paired device{}",
                config.devices.len(),
                plural(config.devices.len())
            ),
            None,
        );
    }

    ui.brand_header(None);
    ui.success("Setup complete", None);
    ui.body("This computer is ready for remote Pi access.");
    println!();
    ui.hint("Host");
    println!(
        "  {}",
        if running {
            ui.green("● Online", false)
        } else {
            ui.paint("○ Offline", "\x1b[2m", false)
        }
    );
    ui.hint("Device");
    let device = config.devices.first().map_or_else(
        || "○ Offline".to_owned(),
        |item| format!("✓ {}", terminal_label(&item.name)),
    );
    println!("  {}", ui.paint(&device, "\x1b[97m", false));
    ui.hint("Workspace");
    let workspace = config.workspaces.first().map_or_else(
        || "None".to_owned(),
        |item| display_workspace_path(&item.path),
    );
    println!("  {}", ui.paint(&workspace, "\x1b[97m", false));
    ui.hint("Remote access");
    println!(
        "  {}",
        ui.paint(
            &relay
                .as_deref()
                .map_or("Local network only".to_owned(), |url| {
                    format!("✓ Pix Relay ({})", display_relay_url(url))
                }),
            "\x1b[97m",
            false
        )
    );
    println!();
    ui.body("Open Pix on your iPhone to start.");
    println!();
    ui.hint("Useful commands");
    println!("    pix status       Check this host");
    println!("    pix pair         Pair another device");
    println!("    pix workspace    Manage workspaces");
    println!();
    let seconds = elapsed.as_secs();
    ui.hint(&format!("Pi {pi_version}  •  Done in {seconds}s"));
    Ok(())
}

/// Attaches the interactive pairing flow to the already-running persistent
/// host service. The service remains available to the macOS menu app and other
/// clients throughout pairing; approving a request only updates durable host
/// state and does not restart Bonjour or the encrypted transport.
#[allow(clippy::too_many_lines)]
pub(crate) fn run_setup_pairing(store: &ConfigStore, pairing: SetupPairingOptions) -> Result<()> {
    let SetupPairingOptions {
        remote,
        yes,
        interactive,
        ui,
        keep_service,
    } = pairing;
    #[cfg(not(unix))]
    {
        let _ = (store, remote, yes, interactive, ui, keep_service);
        bail!("interactive pairing is currently supported on Unix hosts");
    }
    #[cfg(unix)]
    let service_was_running = HostServiceStatus::current(store.path()).is_some();
    #[cfg(unix)]
    let event_stream = {
        service::ensure_running(store)?;
        let stream = service::connect_events(store)
            .context("connecting to the running Pix host event stream")?;
        // Ask the persistent service to replay current state after this
        // subscriber is attached. This also covers a request created just
        // before the CLI connected.
        service::send_command(store, "pending")?;
        if remote {
            service::send_command(store, "pair-remote")?;
        }
        stream
    };
    #[cfg(unix)]
    let mut events = std::io::BufReader::new(event_stream);
    #[cfg(unix)]
    let mut line = String::new();
    #[cfg(unix)]
    let mut approved_device: Option<String> = None;
    #[cfg(unix)]
    let mut approved_connection: Option<String> = None;

    if interactive {
        ui.crumb_header("Setup");
        ui.section("Pair your phone");
        ui.body(pairing_instructions(remote));
        if !remote {
            ui.hint("Keep your iPhone and Mac on the same network and allow local discovery.");
            ui.hint("To pair by QR code instead, configure Pix Relay first.");
        }
    }
    let mut waiting_task = true;
    if remote {
        ui.task("Preparing secure pairing...");
    } else {
        ui.task("Waiting for your phone...");
    }

    #[cfg(unix)]
    loop {
        line.clear();
        let bytes = events
            .read_line(&mut line)
            .context("reading setup host events")?;
        if bytes == 0 {
            break;
        }
        let event: serde_json::Value =
            serde_json::from_str(line.trim()).context("decoding setup host event")?;
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("remote_pairing_ready") => {
                let payload = event
                    .get("qr_payload")
                    .and_then(serde_json::Value::as_str)
                    .context("setup host omitted QR payload")?;
                let join_code = event
                    .get("join_code")
                    .and_then(serde_json::Value::as_str)
                    .context("setup host omitted pairing code")?;
                let expires_at = event
                    .get("expires_at")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default();
                ui.task_done("Secure pairing ready");
                print!(
                    "{}",
                    render_remote_pairing_for_ui(ui, payload, join_code, expires_at)
                );
                std::io::stdout().flush().ok();
            }
            Some("pairing_requested") => {
                let id = event
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .context("setup host omitted pairing request ID")?;
                let device_name = event
                    .get("device_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Your phone")
                    .to_owned();
                let name = terminal_label(&device_name);
                let code = event
                    .get("confirmation_code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if std::mem::replace(&mut waiting_task, false) {
                    ui.task_done(&format!("{name} found"));
                }
                ui.section(&format!("{name} wants to pair"));
                ui.body("Confirm that this code matches the one on your phone:");
                println!(
                    "                 {}",
                    ui.cyan(&format_confirmation_code(code), true)
                );
                let approve = if yes {
                    true
                } else if interactive {
                    let choices = vec![format!("Pair {name}"), "Reject".to_owned()];
                    ui.select("", &choices, 0)? == 0
                } else {
                    bail!("pairing requires --yes when setup is non-interactive")
                };
                service::send_command(
                    store,
                    &format!("{} {id}", if approve { "approve" } else { "reject" }),
                )?;
                if approve {
                    // The service stays alive after approval. The phone can
                    // finish its authenticated snapshot/IK exchange without
                    // racing a parent process shutdown.
                    if keep_service {
                        ui.success("iPhone pairing approved", None);
                        return Ok(());
                    }
                    approved_device = Some(device_name);
                    ui.task("Finishing secure pairing...");
                } else {
                    ui.warning("Pairing rejected", Some("Waiting for another device."));
                    waiting_task = true;
                    ui.task("Waiting for your phone...");
                }
            }
            Some("connection_established") if !keep_service => {
                if approved_device.as_deref()
                    == event.get("device_name").and_then(serde_json::Value::as_str)
                {
                    approved_connection = event
                        .get("connection_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                }
            }
            Some("connection_closed") if !keep_service => {
                if approved_connection.as_deref()
                    == event
                        .get("connection_id")
                        .and_then(serde_json::Value::as_str)
                {
                    if !service_was_running {
                        service::stop(store).context("stopping the temporary setup service")?;
                    }
                    ui.success("iPhone paired", None);
                    return Ok(());
                }
            }
            Some("command_error") => {
                let message = event
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("setup host command failed");
                bail!("{message}");
            }
            Some("connection_failed") => {
                ui.warning(
                    "Device connection failed",
                    Some("Waiting for another attempt."),
                );
            }
            _ => {}
        }
    }

    #[cfg(unix)]
    bail!("Pix host event stream closed before pairing a device")
}

/// Relay setup is recoverable. A failed remote channel can be retried, moved
/// to another endpoint, or downgraded to LAN pairing without discarding the
/// workspace and host identity work that already completed.
#[allow(clippy::too_many_lines)]
pub(crate) fn run_setup_pairing_with_recovery(
    store: &ConfigStore,
    config: &mut pix_core::HostConfig,
    mut relay: Option<String>,
    yes: bool,
    interactive: bool,
    ui: SetupUi,
    keep_service: bool,
) -> Result<Option<String>> {
    loop {
        match run_setup_pairing(
            store,
            SetupPairingOptions {
                remote: relay.is_some(),
                yes,
                interactive,
                ui,
                keep_service,
            },
        ) {
            Ok(()) => return Ok(relay),
            Err(_error) if relay.is_some() && interactive => {
                let relay_label = relay
                    .as_deref()
                    .map_or_else(|| "configured relay".to_owned(), display_relay_url);
                ui.error("Couldn't connect to the relay", Some(&relay_label));
                let choices = vec![
                    "Try again".to_owned(),
                    "Change relay".to_owned(),
                    "Continue with local network only".to_owned(),
                    "Exit".to_owned(),
                ];
                match ui.select("Remote pairing", &choices, 0)? {
                    1 => loop {
                        let value = ui.input("Relay URL", relay.as_deref())?;
                        match validate_relay_url(&value) {
                            Ok(url) => {
                                config.preferences.relay_url = Some(url.clone());
                                config.preferences.relay_enabled = true;
                                commit_setup_relay_preference(store, config)?;
                                relay = Some(url);
                                break;
                            }
                            Err(_) => ui.error(
                                "Pix relay URL is invalid",
                                Some("Use wss:// for a relay, or ws:// for a local endpoint."),
                            ),
                        }
                    },
                    2 => {
                        config.preferences.relay_enabled = false;
                        commit_setup_relay_preference(store, config)?;
                        relay = None;
                    }
                    3 => bail!("setup cancelled"),
                    _ => {}
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn commit_setup_relay_preference(
    store: &ConfigStore,
    draft: &mut pix_core::HostConfig,
) -> Result<()> {
    let service_was_live = host_service_control_live(store)?;
    prepare_running_service_mutation(store)?;
    let transaction = store.transaction()?;
    let mut current = transaction
        .load()
        .context("loading current Pix configuration")?;
    current
        .preferences
        .relay_url
        .clone_from(&draft.preferences.relay_url);
    current.preferences.relay_enabled = draft.preferences.relay_enabled;
    transaction
        .save(&current)
        .context("saving relay configuration")?;
    drop(transaction);
    if service_was_live {
        restart_or_stop_for_configuration_change(store)?;
    }
    *draft = current;
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
pub(crate) fn install_setup_service(
    store: &ConfigStore,
    no_service: bool,
    ui: SetupUi,
) -> Result<bool> {
    let _ = ui.verbose();
    if no_service {
        if ui.interactive() {
            ui.muted("○ Background service skipped (--no-service)");
        } else {
            println!("Background service... skipped");
        }
        return Ok(false);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        loop {
            ui.task("Installing background service...");
            match service::install_for_setup(store) {
                Ok(unit_path) => {
                    ui.task_done("Background service installed");
                    if ui.verbose() {
                        ui.hint(&format!("Service: {}", unit_path.display()));
                    }
                    ui.task("Starting Pix...");
                    ui.task_done("Pix is running");
                    return Ok(true);
                }
                Err(error) if ui.interactive() => {
                    ui.task_failed("Background service installation failed");
                    ui.warning(
                        "Pix couldn't install the background service",
                        Some("Pairing and workspace setup are complete. You can still run Pix manually."),
                    );
                    let choices = vec![
                        "Continue without background service".to_owned(),
                        "Try again".to_owned(),
                        "Show details".to_owned(),
                    ];
                    match ui.select("", &choices, 0)? {
                        1 => {}
                        2 => {
                            ui.hint(&format!("{error:#}"));
                            return Ok(false);
                        }
                        _ => return Ok(false),
                    }
                }
                Err(error) => {
                    ui.task_failed("Background service installation failed");
                    if ui.verbose() {
                        ui.hint(&format!("{error:#}"));
                    }
                    return Ok(false);
                }
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = store;
        if ui.interactive() {
            ui.muted("○ Background service installation is available on Linux and macOS");
        } else {
            println!("Background service... unavailable on this platform");
        }
        Ok(false)
    }
}

use crate::commands::pi::configured_pi_version;
use crate::commands::relay::{display_relay_url, validate_relay_url};
use crate::commands::shared::{
    DEFAULT_RELAY_URL, default_host_name, display_workspace_path, expand_home,
    format_confirmation_code, home_directory, host_identity_path, host_service_control_live,
    load_host_identity, plural, prepare_running_service_mutation, refresh_running_service,
    restart_or_stop_for_configuration_change, terminal_label,
};
use crate::commands::workspace::add_workspace;
use crate::serve::{pairing_instructions, render_remote_pairing_for_ui};
use crate::service;
