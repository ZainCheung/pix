//! Explicitly authorized workspace folders.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pix_core::{ConfigStore, WorkspaceRegistry};

use crate::output::CommandOutput;
use crate::setup_ui::{MenuItem, MenuResult, SetupUi, UiTone};

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn add_workspace(
    config: &mut pix_core::HostConfig,
    path: PathBuf,
    name: Option<String>,
    ui: SetupUi,
) -> Result<Option<String>> {
    let canonical = std::fs::canonicalize(&path)
        .with_context(|| format!("resolving workspace {}", path.display()))?;
    if let Some(existing) = config
        .workspaces
        .iter()
        .find(|workspace| workspace.path == canonical)
    {
        return Ok(Some(display_workspace_path(&existing.path)));
    }
    let mut registry = WorkspaceRegistry::new(config);
    let added = registry
        .add(&canonical, name)
        .with_context(|| format!("authorizing workspace {}", path.display()))?
        .clone();
    let displayed = display_workspace_path(&added.path);
    if ui.interactive() {
        ui.success("Added workspace", Some(&displayed));
    }
    Ok(Some(displayed))
}

#[allow(clippy::too_many_lines)]
pub(crate) fn workspace(
    store: &ConfigStore,
    command: Option<WorkspaceCommand>,
    output: CommandOutput,
    interactive: bool,
) -> Result<()> {
    let Some(command) = command else {
        if !interactive {
            return Err(usage_error(
                "a workspace command is required outside an interactive terminal",
            ));
        }
        return workspace_menu(store, output);
    };
    match command {
        WorkspaceCommand::Add { path, name } => {
            prepare_running_service_mutation(store)?;
            let transaction = store.transaction()?;
            let mut config = transaction
                .load_or_create(default_host_name())
                .context("loading Pix configuration")?;
            let mut registry = WorkspaceRegistry::new(&mut config);
            let added = registry
                .add(&path, name)
                .with_context(|| format!("authorizing workspace {}", path.display()))?
                .clone();
            transaction
                .save(&config)
                .context("saving Pix configuration")?;
            drop(transaction);
            let service_refresh = refresh_running_service(store)?;
            if output.is_json() {
                return output.success(
                    "workspace.add",
                    &serde_json::json!({
                        "workspace": {
                            "id": added.id,
                            "name": added.name,
                            "path": added.path,
                            "created_at": added.created_at,
                        },
                        "service_refreshed": service_refresh.as_ref().map(|_| true),
                        "service_refresh": service_refresh,
                    }),
                );
            }
            println!("Authorized {} ({})", terminal_label(&added.name), added.id);
            println!("  {}", terminal_label(&added.path.display().to_string()));
            Ok(())
        }
        WorkspaceCommand::List => {
            let config = store.load().context("loading Pix configuration")?;
            if output.is_json() {
                let workspaces = config
                    .workspaces
                    .iter()
                    .map(|workspace| {
                        serde_json::json!({
                            "id": workspace.id,
                            "name": workspace.name,
                            "path": workspace.path,
                            "created_at": workspace.created_at,
                        })
                    })
                    .collect::<Vec<_>>();
                return output.success(
                    "workspace.list",
                    &serde_json::json!({"workspaces": workspaces}),
                );
            }
            if config.workspaces.is_empty() {
                println!("No authorized workspaces.");
                return Ok(());
            }
            for workspace in config.workspaces {
                println!("{}  {}", workspace.id, terminal_label(&workspace.name));
                println!(
                    "  {}",
                    terminal_label(&workspace.path.display().to_string())
                );
            }
            Ok(())
        }
        WorkspaceCommand::Remove { id } => {
            let config = store.load().context("loading Pix configuration")?;
            let confirm = interactive && id.is_none();
            let id = select_workspace_id(&config, id, interactive)?;
            let index = config
                .workspaces
                .iter()
                .position(|workspace| workspace.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown workspace: {id}"))?;
            let removed = config.workspaces[index].clone();
            if confirm {
                let ui = SetupUi::new(true, false);
                let choices = vec!["Remove workspace".to_owned(), "Cancel".to_owned()];
                if ui.select(
                    &format!(
                        "Remove authorization for {}?",
                        terminal_label(&removed.name)
                    ),
                    &choices,
                    1,
                )? != 0
                {
                    return Ok(());
                }
            }
            prepare_running_service_mutation(store)?;
            let transaction = store.transaction()?;
            let mut config = transaction
                .load()
                .context("loading current Pix configuration")?;
            let index = config
                .workspaces
                .iter()
                .position(|workspace| workspace.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown workspace: {id}"))?;
            let removed = config.workspaces.remove(index);
            transaction
                .save(&config)
                .context("saving Pix configuration")?;
            drop(transaction);
            let service_refresh = refresh_running_service(store)?;
            if output.is_json() {
                return output.success(
                    "workspace.remove",
                    &serde_json::json!({
                        "workspace": {
                            "id": removed.id,
                            "name": removed.name,
                            "path": removed.path,
                            "created_at": removed.created_at,
                        },
                        "service_refreshed": service_refresh.as_ref().map(|_| true),
                        "service_refresh": service_refresh,
                    }),
                );
            }
            println!("Removed {} ({})", terminal_label(&removed.name), removed.id);
            Ok(())
        }
    }
}

pub(crate) fn workspace_menu(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let config = load_or_ephemeral_config(store)?;
    let ui = SetupUi::new(true, false);
    ui.crumb_header("Workspaces");
    ui.status_row(
        "authorized",
        &format!(
            "{} workspace{}",
            config.workspaces.len(),
            plural(config.workspaces.len())
        ),
        if config.workspaces.is_empty() {
            UiTone::Warning
        } else {
            UiTone::Default
        },
    );
    println!();
    let mut actions = vec![(
        0_u8,
        MenuItem::new("Add a workspace", "Authorize a host folder for Pi"),
    )];
    if !config.workspaces.is_empty() {
        actions.push((
            1,
            MenuItem::new("Remove a workspace", "Revoke access without deleting files"),
        ));
        actions.push((
            2,
            MenuItem::new("List workspaces", "Show authorized folders"),
        ));
    }
    actions.push((3, MenuItem::new("Back", "Return to the shell")));
    let items = actions.iter().map(|(_, item)| *item).collect::<Vec<_>>();
    match ui.menu("Actions", &items, 0)? {
        MenuResult::Selected(index) => match actions[index].0 {
            0 => {
                let path = select_workspace_path(ui)?;
                workspace(
                    store,
                    Some(WorkspaceCommand::Add { path, name: None }),
                    output,
                    true,
                )
            }
            1 => workspace(
                store,
                Some(WorkspaceCommand::Remove { id: None }),
                output,
                true,
            ),
            2 => workspace(store, Some(WorkspaceCommand::List), output, true),
            _ => Ok(()),
        },
        MenuResult::Help => print_cli_help(),
        MenuResult::Quit => Ok(()),
    }
}

pub(crate) fn select_workspace_id(
    config: &pix_core::HostConfig,
    id: Option<uuid::Uuid>,
    interactive: bool,
) -> Result<uuid::Uuid> {
    if let Some(id) = id {
        return Ok(id);
    }
    if config.workspaces.is_empty() {
        bail!("no authorized workspaces");
    }
    if !interactive {
        return Err(usage_error(
            "workspace ID is required outside an interactive terminal",
        ));
    }
    let ui = SetupUi::new(true, false);
    let options = config
        .workspaces
        .iter()
        .map(|workspace| {
            format!(
                "{}  {}",
                terminal_label(&workspace.name),
                terminal_label(&workspace.path.display().to_string())
            )
        })
        .collect::<Vec<_>>();
    let selected = ui.select("Choose a workspace to remove", &options, 0)?;
    Ok(config.workspaces[selected].id)
}

use crate::WorkspaceCommand;
use crate::commands::setup::select_workspace_path;
use crate::commands::shared::{
    default_host_name, display_workspace_path, load_or_ephemeral_config, plural,
    prepare_running_service_mutation, refresh_running_service, terminal_label,
};
use crate::{print_cli_help, usage_error};
