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
        advanced: false,
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
    pub(crate) advanced: bool,
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
    // Nothing touches disk until the wizard commits: abandoning setup must
    // leave the host exactly as it was, including the home screen's
    // first-run state. Identity creation is deferred with it because its
    // keychain entry is keyed by the persisted host ID.
    let config = if config_was_present {
        store.load().context("loading Pix configuration")?
    } else {
        pix_core::HostConfig::new(default_host_name())
    };

    if config_was_present
        && setup_is_already_configured(store, &config)
        && !options.non_interactive
        && options.relay.is_none()
        && options.workspace.is_none()
        && !options.no_pair
        && !options.no_service
    {
        return setup_existing(store, &config, options, ui);
    }

    let started_at = std::time::Instant::now();
    if options.advanced {
        setup_advanced(store, config, options, ui, started_at)
    } else {
        setup_quick(store, config, options, ui, started_at)
    }
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
    config: &pix_core::HostConfig,
    _options: &SetupOptions,
    ui: SetupUi,
) -> Result<()> {
    ui.logo_header(None);
    ui.section("Pix is already set up on this computer");
    ui.success(
        &format!("Pi {}", configured_pi_version(store, config)),
        None,
    );
    if config.devices.is_empty() {
        ui.muted("○ No paired devices yet");
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
    if config.workspaces.is_empty() {
        ui.muted("○ No authorized workspaces yet");
    } else {
        ui.success(
            &format!(
                "{} workspace{}",
                config.workspaces.len(),
                plural(config.workspaces.len())
            ),
            None,
        );
    }
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
            config,
            ui,
            configured_pi_version(store, config),
            config.preferences.active_relay_url().map(str::to_owned),
            false,
            std::time::Duration::ZERO,
        );
    }

    // The Manage Pix menu is gone: pairing and workspaces live in their own
    // list views, relay in Settings, and a configured host only needs the
    // health verification when `pix setup` is run explicitly.
    verify_setup(
        store,
        config,
        ui,
        configured_pi_version(store, config),
        config.preferences.active_relay_url().map(str::to_owned),
        false,
        std::time::Duration::ZERO,
    )
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
    // The durable host ID exists only now; create the matching identity
    // before any service can observe a half-committed host.
    load_host_identity(store, current.host.id).context("preparing host identity")?;
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
        ui.logo_header(None);
        ui.hint("Tip: `pix setup --advanced` exposes every option.");
        ui.section("Checking this computer");
    }
    let pi_version = prepare_setup_environment(store, &mut config, options, ui)?;
    let relay = configure_setup_relay(&mut config, options, ui, false)?;
    configure_setup_workspace(&mut config, options, ui, ui.interactive(), false)?;
    commit_setup_draft(store, &baseline, &mut config)?;

    // The service is installed before pairing so a cancelled or failed
    // pairing can no longer discard the installation work.
    let service = install_setup_service(store, options.no_service, ui)?;
    let relay = maybe_pair_phone(store, &mut config, relay, options, ui, service)?;

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

/// Pairing is optional: the phone may simply not be at hand. Devices that
/// are already paired or an explicit `--no-pair` skip the wait; interactive
/// users choose between pairing now and a later `pix device pair`.
fn maybe_pair_phone(
    store: &ConfigStore,
    config: &mut pix_core::HostConfig,
    relay: Option<String>,
    options: &SetupOptions,
    ui: SetupUi,
    keep_service: bool,
) -> Result<Option<String>> {
    if !config.devices.is_empty() {
        if ui.interactive() {
            ui.success(
                &format!(
                    "{} paired device{} already configured",
                    config.devices.len(),
                    plural(config.devices.len())
                ),
                None,
            );
        } else {
            println!(
                "Pairing... {} device{} already configured",
                config.devices.len(),
                plural(config.devices.len())
            );
        }
        return Ok(relay);
    }
    if options.no_pair {
        if ui.interactive() {
            ui.muted("○ Device pairing skipped");
        } else {
            println!("Pairing... skipped");
        }
        return Ok(relay);
    }
    let pair_now = if options.yes || !ui.interactive() {
        true
    } else {
        ui.section("Pair your phone");
        ui.select(
            "Pair your phone now?",
            &["Pair now".to_owned(), "Pair later".to_owned()],
            0,
        )? == 0
    };
    if !pair_now {
        if ui.interactive() {
            ui.muted("○ Pair later with `pix device pair`");
        } else {
            println!("Pairing... skipped");
        }
        return Ok(relay);
    }
    run_setup_pairing_with_recovery(
        store,
        config,
        relay,
        options.yes,
        ui.interactive(),
        ui,
        keep_service,
    )
}

pub(crate) fn prepare_setup_environment(
    _store: &ConfigStore,
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
                remember_discovered_pi(config, &installation.executable);
                ui.task_done(&format!("Pi {}", installation.version));
                if options.verbose {
                    ui.hint(&format!(
                        "Executable: {}",
                        installation.executable.display()
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
                        Some("Run `pix status` for the resolved Pi executable and version."),
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
                    .map(|path| path.display().to_string());
                if let Some(path) = prompt_pi_executable(ui, current.as_deref())? {
                    config.preferences.pi_executable = Some(path);
                }
            }
            2 => bail!("setup cancelled"),
            _ => {}
        }
    }
}

fn remember_discovered_pi(config: &mut pix_core::HostConfig, executable: &std::path::Path) {
    if config.preferences.pi_executable.is_none() {
        // Setup may run with a richer terminal/login-shell PATH than the
        // launchd service receives later. Pin the verified executable so the
        // background host does not have to rediscover a version-manager shim.
        config.preferences.pi_executable = Some(executable.to_path_buf());
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
    ui.logo_header(None);
    ui.crumb_header("Advanced setup");

    // "Go back" restarts the form with every earlier answer kept as the
    // default, instead of discarding the draft and re-running quick setup.
    let (install_service, relay) = loop {
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
                .map(|value| value.display().to_string());
            if let Some(path) = prompt_pi_executable(ui, current_pi.as_deref())? {
                config.preferences.pi_executable = Some(path);
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
                let path = select_workspace_path(ui, "Add a workspace:")?;
                add_workspace(&mut config, path, None, ui)?;
            } else if let Some((path, _)) = candidates.get(index) {
                add_workspace(&mut config, path.clone(), None, ui)?;
            }
        }
        if config.workspaces.is_empty() {
            let path = select_workspace_path(ui, "Choose your first workspace:")?;
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
            2 => bail!("setup cancelled"),
            1 => {}
            _ => break (install_service, relay),
        }
    };

    if ui.interactive() {
        ui.section("Checking this computer");
    }
    let pi_version = prepare_setup_environment(store, &mut config, options, ui)?;
    commit_setup_draft(store, &baseline, &mut config)?;
    let service = if install_service {
        install_setup_service(store, false, ui)?
    } else {
        install_setup_service(store, true, ui)?
    };
    let relay = maybe_pair_phone(store, &mut config, relay, options, ui, service)?;
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
        None if interactive => select_workspace_path(ui, "Choose your first workspace:")?,
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

/// Prompts for a Pi executable with the same rigor as workspace paths:
/// `~` expands, the file must exist, and an empty answer keeps the current
/// setting instead of storing a literal `~` path that never resolves.
pub(crate) fn prompt_pi_executable(ui: SetupUi, current: Option<&str>) -> Result<Option<PathBuf>> {
    loop {
        let value = ui.input("Pi executable", current)?;
        if value.trim().is_empty() {
            return Ok(None);
        }
        let path = expand_home(PathBuf::from(value));
        match std::fs::canonicalize(&path) {
            Ok(path) if path.is_file() => return Ok(Some(path)),
            Ok(_) => ui.error("Pi executable must be a file", None),
            Err(_) => ui.error(
                "Pi executable was not found",
                Some(&format!("resolved to {}", path.display())),
            ),
        }
    }
}

pub(crate) fn select_workspace_path(ui: SetupUi, prompt: &str) -> Result<PathBuf> {
    let candidates = workspace_candidates();
    let mut options = candidates
        .iter()
        .map(|(path, label)| format!("{:<36} {}", display_workspace_path(path), label))
        .collect::<Vec<_>>();
    options.push("Enter another path...".to_owned());
    let selected = ui.select(prompt, &options, 0)?;
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

#[allow(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::too_many_lines
)]
pub(crate) fn verify_setup(
    store: &ConfigStore,
    config: &pix_core::HostConfig,
    ui: SetupUi,
    pi_version: String,
    relay: Option<String>,
    _service_installed: bool,
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
    let running = HostServiceStatus::current(store.path()).is_some();
    let installed = service::managed_service_installed(store).unwrap_or(false);
    if running {
        ui.task_done("Host service running");
    } else {
        ui.task_failed("Host service not running");
        if installed {
            ui.warning(
                "The service is installed but stopped",
                Some("Start it with `pix service start`."),
            );
        } else {
            ui.warning(
                "You can still run Pix manually",
                Some("Start it with `pix serve`."),
            );
        }
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

    ui.section("Setup complete");
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
    let device = match config.devices.len() {
        0 => "○ None paired yet".to_owned(),
        1 => format!("✓ {}", terminal_label(&config.devices[0].name)),
        count => format!(
            "✓ {} and {} more",
            terminal_label(&config.devices[0].name),
            count - 1
        ),
    };
    println!("  {}", ui.paint(&device, "\x1b[97m", false));
    ui.hint("Workspace");
    let workspace = match config.workspaces.len() {
        0 => "None".to_owned(),
        1 => display_workspace_path(&config.workspaces[0].path),
        count => format!(
            "{} (+{} more)",
            display_workspace_path(&config.workspaces[0].path),
            count - 1
        ),
    };
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
    println!("  pix status         Check this host");
    println!("  pix device pair    Pair another device");
    println!("  pix workspace      Manage workspaces");
    println!();
    let seconds = elapsed.as_secs();
    ui.hint(&format!("Pi {pi_version}  •  Done in {seconds}s"));
    Ok(())
}

/// Uninstalls the pairing-temporary service when dropped unless disarmed.
/// Covers `bail!` and `?` exits; a hard Ctrl+C kill is the one path that can
/// leave the unit behind.
#[cfg(unix)]
struct TempServiceGuard<'a> {
    store: &'a ConfigStore,
    armed: bool,
}

#[cfg(unix)]
impl TempServiceGuard<'_> {
    fn cleanup(&mut self) -> Result<()> {
        if self.armed {
            self.armed = false;
            service::uninstall_for_setup(self.store)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for TempServiceGuard<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Distinguishes an explicit user cancellation from a relay failure so the
/// recovery menu never traps someone who asked to leave.
fn is_user_cancel(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("cancelled")
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
    // When pairing had to install the service itself, the host must end up
    // uninstalled again on every exit path — the user asked for no service.
    #[cfg(unix)]
    let mut temp_service = TempServiceGuard {
        store,
        armed: !service_was_running && !keep_service,
    };
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
        ui.hint("Press Ctrl+C at any time to cancel pairing.");
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
                    temp_service
                        .cleanup()
                        .context("removing the temporary setup service")?;
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
            Err(error) if is_user_cancel(&error) => return Err(error),
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
                        2 => ui.hint(&format!("{error:#}")),
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
    format_confirmation_code, home_directory, host_service_control_live, load_host_identity,
    plural, prepare_running_service_mutation, refresh_running_service,
    restart_or_stop_for_configuration_change, terminal_label,
};
use crate::commands::workspace::add_workspace;
use crate::serve::{pairing_instructions, render_remote_pairing_for_ui};
use crate::service;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use pix_core::HostConfig;

    use super::{is_user_cancel, remember_discovered_pi};

    #[test]
    fn cancellations_are_distinguished_from_relay_failures() {
        assert!(is_user_cancel(&anyhow::anyhow!("cancelled by user")));
        assert!(is_user_cancel(&anyhow::anyhow!(
            "outer: {}",
            anyhow::anyhow!("setup cancelled by user")
        )));
        assert!(!is_user_cancel(&anyhow::anyhow!(
            "the relay channel failed to join"
        )));
    }

    #[test]
    fn setup_pins_auto_discovered_pi_but_preserves_an_explicit_choice() {
        let mut discovered = HostConfig::new("Test host");
        remember_discovered_pi(&mut discovered, Path::new("/managed/node/bin/pi"));
        assert_eq!(
            discovered.preferences.pi_executable,
            Some(PathBuf::from("/managed/node/bin/pi"))
        );

        let mut explicit = HostConfig::new("Test host");
        explicit.preferences.pi_executable = Some(PathBuf::from("/custom/pi"));
        remember_discovered_pi(&mut explicit, Path::new("/managed/node/bin/pi"));
        assert_eq!(
            explicit.preferences.pi_executable,
            Some(PathBuf::from("/custom/pi"))
        );
    }
}
