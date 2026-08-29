//! Active Pi runtime sessions held by the host service.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;
use serde::{Deserialize, Serialize};

use crate::output::CommandOutput;
use crate::setup_ui::{MenuItem, MenuResult, SetupUi, UiTone};
use crate::status::HostServiceControl;

pub(crate) fn release_session(
    service: &pix_core::HostServiceHandle,
    session_id: Option<&str>,
    local_request_id: Option<uuid::Uuid>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) -> Result<()> {
    let session_id = session_id.context("release requires session ID")?;
    service
        .release_session(session_id)
        .context("releasing session")?;
    emit_event(
        &ServeEvent::SessionReleased {
            session_id: session_id.to_owned(),
            local_request_id,
        },
        output,
        log,
        control,
    );
    emit_sessions(service, output, log, control);
    Ok(())
}

pub(crate) fn emit_sessions(
    service: &pix_core::HostServiceHandle,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) {
    emit_sessions_for(service, None, output, log, control);
}

pub(crate) fn emit_sessions_for(
    service: &pix_core::HostServiceHandle,
    local_request_id: Option<uuid::Uuid>,
    output: ServeOutput,
    log: &HostLog,
    control: &mut HostServiceControl,
) {
    let sessions = service
        .active_sessions()
        .into_iter()
        .map(|session| SessionEvent {
            id: session.session_id.to_string(),
            workspace: session.workspace.display().to_string(),
            clients: session.client_count,
            state: session.state_name(),
            backend: session.backend.as_str(),
        })
        .collect();
    emit_event(
        &ServeEvent::SessionList {
            sessions,
            local_request_id,
        },
        output,
        log,
        control,
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ActiveSessionSummary {
    id: String,
    workspace: String,
    clients: usize,
    state: String,
    #[serde(default = "default_backend")]
    backend: String,
}

fn default_backend() -> String {
    "rpc".to_owned()
}

pub(crate) fn session(
    store: &ConfigStore,
    command: Option<SessionCommand>,
    output: CommandOutput,
    interactive: bool,
) -> Result<()> {
    let Some(command) = command else {
        if !interactive {
            return Err(usage_error(
                "a session command is required outside an interactive terminal",
            ));
        }
        return session_menu(store, output);
    };
    match command {
        SessionCommand::List => {
            let sessions = active_sessions(store)?;
            if output.is_json() {
                return output.success("session.list", &serde_json::json!({"sessions": sessions}));
            }
            if sessions.is_empty() {
                println!("No active Pix sessions.");
                return Ok(());
            }
            for session in sessions {
                println!("{}  {}", session.id, session.state);
                println!(
                    "  workspace {}  clients {}",
                    terminal_label(&session.workspace),
                    session.clients
                );
            }
            Ok(())
        }
        SessionCommand::Release { id } => {
            let sessions = active_sessions(store)?;
            let session = select_active_session(&sessions, id, interactive)?;
            let event = service_client::request_event(
                store,
                &format!("release {}", session.id),
                "session_released",
                Duration::from_secs(8),
            )?;
            if event.get("session_id").and_then(serde_json::Value::as_str)
                != Some(session.id.as_str())
            {
                bail!("Pix host returned a mismatched session release event");
            }
            if output.is_json() {
                return output.success(
                    "session.release",
                    &serde_json::json!({"session": session, "released": true}),
                );
            }
            println!("Released session {}.", session.id);
            Ok(())
        }
    }
}

pub(crate) fn active_sessions(store: &ConfigStore) -> Result<Vec<ActiveSessionSummary>> {
    let event =
        service_client::request_event(store, "sessions", "session_list", Duration::from_secs(5))?;
    serde_json::from_value(
        event
            .get("sessions")
            .cloned()
            .context("Pix host omitted active sessions")?,
    )
    .context("decoding active Pix sessions")
}

pub(crate) fn select_active_session(
    sessions: &[ActiveSessionSummary],
    id: Option<String>,
    interactive: bool,
) -> Result<ActiveSessionSummary> {
    if sessions.is_empty() {
        bail!("no active Pix sessions");
    }
    if let Some(id) = id {
        return sessions
            .iter()
            .find(|session| session.id == id)
            .cloned()
            .with_context(|| format!("unknown active session: {id}"));
    }
    if !interactive {
        return Err(usage_error(
            "session ID is required outside an interactive terminal",
        ));
    }
    let ui = SetupUi::new(true, false);
    let options = sessions
        .iter()
        .map(|session| format!("{}  {}", session.state, terminal_label(&session.workspace)))
        .collect::<Vec<_>>();
    let selected = ui.select("Choose a session to release", &options, 0)?;
    Ok(sessions[selected].clone())
}

pub(crate) fn session_menu(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let sessions = active_sessions(store)?;
    let ui = SetupUi::new(true, false);
    ui.crumb_header("Sessions");
    ui.status_row(
        "active",
        &format!("{} session{}", sessions.len(), plural(sessions.len())),
        if sessions.is_empty() {
            UiTone::Muted
        } else {
            UiTone::Default
        },
    );
    println!();
    let mut actions = Vec::new();
    if !sessions.is_empty() {
        actions.push((
            0_u8,
            MenuItem::new("Release a session", "Return ownership to standard Pi"),
        ));
        actions.push((
            1,
            MenuItem::new("List sessions", "Show active runtime details"),
        ));
    }
    actions.push((2, MenuItem::new("Back", "Return to the shell")));
    let items = actions.iter().map(|(_, item)| *item).collect::<Vec<_>>();
    match ui.menu("Actions", &items, 0)? {
        MenuResult::Selected(index) => match actions[index].0 {
            0 => session(
                store,
                Some(SessionCommand::Release { id: None }),
                output,
                true,
            ),
            1 => session(store, Some(SessionCommand::List), output, true),
            _ => Ok(()),
        },
        MenuResult::Help => print_cli_help(),
        MenuResult::Quit => Ok(()),
    }
}

use crate::SessionCommand;
use crate::commands::shared::{plural, terminal_label};
use crate::serve::SessionEvent;
use crate::serve::{HostLog, ServeEvent, ServeOutput, emit_event};
use crate::service_client;
use crate::{print_cli_help, usage_error};
