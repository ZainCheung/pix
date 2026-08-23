//! Explicitly authorized workspace folders.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pix_core::{ConfigStore, PiSessionStore, WorkspaceRegistry};
use serde::Serialize;

use crate::output::CommandOutput;
use crate::setup_ui::{ListRow, PickerAction, SetupUi};

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
        WorkspaceCommand::Sessions { id } => {
            let mut config = store.load().context("loading Pix configuration")?;
            let workspace_record = config
                .workspaces
                .iter()
                .find(|workspace| workspace.id == id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown workspace: {id}"))?;
            let root = WorkspaceRegistry::new(&mut config)
                .authorized_root(id)
                .with_context(|| format!("resolving workspace {id}"))?;
            let discovered = PiSessionStore::for_workspace(&root)
                .context("locating workspace sessions")?
                .list()
                .context("listing workspace sessions")?;
            let sessions = discovered
                .iter()
                .map(|session| WorkspaceSessionOutput::from_summary(&session.summary))
                .collect::<Vec<_>>();
            if output.is_json() {
                return output.success(
                    "workspace.sessions",
                    &serde_json::json!({
                        "workspace": {
                            "id": workspace_record.id,
                            "name": workspace_record.name,
                        },
                        "sessions": sessions,
                    }),
                );
            }
            if sessions.is_empty() {
                println!("No Pi sessions stored in this workspace yet.");
                return Ok(());
            }
            for session in sessions {
                println!(
                    "{}  {}  {} message{}",
                    session.id,
                    session.modified_at,
                    session.message_count,
                    plural(session.message_count)
                );
                if let Some(title) = session.title {
                    println!("  {}", terminal_label(&title));
                }
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
    loop {
        let config = load_or_ephemeral_config(store)?;
        let ui = SetupUi::new(true, false);
        ui.crumb_header("Workspaces");
        let columns: Vec<(String, String)> = config
            .workspaces
            .iter()
            .map(|workspace| (workspace.name.clone(), workspace.path.display().to_string()))
            .collect();
        let rows: Vec<ListRow<'_>> = columns
            .iter()
            .map(|(name, path)| ListRow::new(name, path))
            .collect();
        let hints: &[(&str, &str)] = if rows.is_empty() {
            &[("A", "add")]
        } else {
            &[("A", "add"), ("R", "remove"), ("enter", "sessions")]
        };
        match ui.picker(&rows, hints, "No authorized workspaces yet.")? {
            PickerAction::Quit => return Ok(()),
            PickerAction::Key { key: 'a', .. } => {
                let path = select_workspace_path(ui, "Add a workspace:")?;
                workspace(
                    store,
                    Some(WorkspaceCommand::Add { path, name: None }),
                    output,
                    true,
                )?;
            }
            PickerAction::Key { key: 'r', selected } => {
                let Some(record) = config.workspaces.get(selected) else {
                    continue;
                };
                let id = record.id;
                let name = record.name.clone();
                let choices = vec!["Remove workspace".to_owned(), "Cancel".to_owned()];
                if ui.select(
                    &format!("Remove authorization for {}?", terminal_label(&name)),
                    &choices,
                    1,
                )? != 0
                {
                    return Ok(());
                }
                workspace(
                    store,
                    Some(WorkspaceCommand::Remove { id: Some(id) }),
                    output,
                    true,
                )?;
            }
            PickerAction::Select(index) => {
                let Some(record) = config.workspaces.get(index) else {
                    continue;
                };
                workspace(
                    store,
                    Some(WorkspaceCommand::Sessions { id: record.id }),
                    output,
                    true,
                )?;
            }
            PickerAction::Key { .. } => {}
        }
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
use crate::usage_error;

#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceSessionOutput {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) modified_at: String,
    pub(crate) message_count: usize,
}

impl WorkspaceSessionOutput {
    pub(crate) fn from_summary(summary: &pix_core::SessionSummary) -> Self {
        let title = summary
            .name
            .as_deref()
            .and_then(compact_session_title)
            .or_else(|| {
                summary
                    .first_user_message
                    .as_deref()
                    .and_then(compact_session_title)
            });
        Self {
            id: summary.id.to_string(),
            title,
            modified_at: summary.modified_at.to_rfc3339(),
            message_count: summary.message_count,
        }
    }
}

/// Collapses session titles to single-line menu rows capped at 120 chars.
pub(crate) fn compact_session_title(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let mut characters = normalized.chars();
    let mut title = characters.by_ref().take(120).collect::<String>();
    if characters.next().is_some() {
        title.push('…');
    }
    Some(title)
}

#[cfg(test)]
mod tests {
    use super::compact_session_title;

    #[test]
    fn session_titles_are_compacted_for_menu_rows() {
        assert_eq!(
            compact_session_title("  fix   menu\nhover  "),
            Some("fix menu hover".to_owned())
        );
        assert_eq!(compact_session_title("   \n\t "), None);
        let long = "x".repeat(140);
        let compact = compact_session_title(&long).expect("compacted");
        assert_eq!(compact.chars().count(), 121);
        assert!(compact.ends_with('…'));
    }
}
