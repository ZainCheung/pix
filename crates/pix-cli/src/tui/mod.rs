mod event;
mod terminal;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use uuid::Uuid;

use crate::commands;
use crate::home::{AccessMode, ConfigState, HostOverview, ServiceState};
use crate::output::OutputFormat;
use crate::setup_ui::LOGO;
use pix_core::config::WorkspaceRecord;
use pix_core::{ConfigStore, PiSessionStore, WorkspaceRegistry};

pub(crate) use terminal::TerminalGuard;

const MIN_WIDTH: u16 = 48;
const HOME_BANNER_HEIGHT: u16 = 8;
const HOME_SUMMARY_HEIGHT: u16 = 6;
const HOME_MENU_HEIGHT: u16 = 4;
const FOOTER_HEIGHT: u16 = 1;
const MIN_HEIGHT: u16 = HOME_BANNER_HEIGHT + HOME_SUMMARY_HEIGHT + HOME_MENU_HEIGHT + FOOTER_HEIGHT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractionMode {
    Tui,
    HumanCli,
    Machine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TtyState {
    Attached,
    Detached,
}

impl TtyState {
    pub(crate) const fn from_is_tty(is_tty: bool) -> Self {
        if is_tty {
            Self::Attached
        } else {
            Self::Detached
        }
    }
}

/// Resolves the frontend before any operation can print or mutate terminal
/// state. Explicit commands always remain human CLI even when attached to a
/// TTY; only bare, usable interactive terminals may enter the TUI.
pub(crate) fn interaction_mode(
    has_command: bool,
    no_input: bool,
    output: OutputFormat,
    stdin: TtyState,
    stdout: TtyState,
    term: Option<&str>,
) -> InteractionMode {
    if output == OutputFormat::Json {
        return InteractionMode::Machine;
    }
    if has_command
        || no_input
        || stdin == TtyState::Detached
        || stdout == TtyState::Detached
        || !usable_term(term)
    {
        return InteractionMode::HumanCli;
    }
    InteractionMode::Tui
}

pub(crate) fn should_launch_tui(
    has_command: bool,
    no_input: bool,
    output: OutputFormat,
    stdin: TtyState,
    stdout: TtyState,
    term: Option<&str>,
) -> bool {
    interaction_mode(has_command, no_input, output, stdin, stdout, term) == InteractionMode::Tui
}

fn usable_term(term: Option<&str>) -> bool {
    term.is_some_and(|value| {
        let value = value.trim();
        !value.is_empty() && !value.eq_ignore_ascii_case("dumb")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Home,
    Devices,
    Workspaces,
    WorkspaceSessions,
    Settings,
    Status,
}

impl Route {
    const fn title(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Devices => "Devices",
            Self::Workspaces => "Workspaces",
            Self::WorkspaceSessions => "Sessions",
            Self::Settings => "Settings",
            Self::Status => "Status",
        }
    }

    const fn from_home_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Devices),
            1 => Some(Self::Workspaces),
            2 => Some(Self::Settings),
            3 => Some(Self::Status),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Overlay {
    Help,
    Add(AddOverlay),
    Remove(RemoveOverlay),
}

fn load_sessions(
    store: &ConfigStore,
    workspace_id: Uuid,
) -> anyhow::Result<(WorkspaceItem, Vec<SessionItem>)> {
    let mut config = commands::shared::load_or_ephemeral_config(store)?;
    let record = config
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("unknown workspace: {workspace_id}"))?;
    let root = WorkspaceRegistry::new(&mut config).authorized_root(workspace_id)?;
    let discovered = PiSessionStore::for_workspace(&root)?.list()?;
    let sessions = discovered
        .iter()
        .map(|session| {
            let output =
                commands::workspace::WorkspaceSessionOutput::from_summary(&session.summary);
            SessionItem {
                id: output.id,
                title: output.title,
                modified_at: output.modified_at,
                message_count: output.message_count,
            }
        })
        .collect();
    Ok((
        WorkspaceItem {
            id: record.id,
            name: record.name,
            path: record.path,
        },
        sessions,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastTone {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Toast {
    pub(crate) message: String,
    pub(crate) tone: ToastTone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceItem {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionItem {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) modified_at: String,
    pub(crate) message_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceCandidate {
    pub(crate) path: PathBuf,
    pub(crate) label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextInput {
    pub(crate) value: String,
    pub(crate) cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddOverlay {
    pub(crate) candidates: Vec<WorkspaceCandidate>,
    pub(crate) selected: usize,
    pub(crate) input: Option<TextInput>,
}

impl AddOverlay {
    fn new() -> Self {
        let candidates = commands::setup::workspace_candidates()
            .into_iter()
            .map(|(path, label)| WorkspaceCandidate {
                path,
                label: label.to_owned(),
            })
            .collect::<Vec<_>>();
        Self {
            selected: 0,
            candidates,
            input: None,
        }
    }

    fn option_count(&self) -> usize {
        self.candidates.len() + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoveOverlay {
    pub(crate) workspace: WorkspaceItem,
    /// The destructive action is intentionally not the default.
    pub(crate) selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingAction {
    Add(PathBuf),
    Remove(Uuid),
    OpenSessions(Uuid),
}

impl PendingAction {
    const fn label(&self) -> &'static str {
        match self {
            Self::Add(_) => "Adding workspace…",
            Self::Remove(_) => "Removing workspace…",
            Self::OpenSessions(_) => "Loading sessions…",
        }
    }
}

#[derive(Debug)]
enum WorkerResult {
    Added {
        workspace: WorkspaceRecord,
        service_error: Option<String>,
    },
    AddFailed(String),
    Removed {
        workspace: WorkspaceRecord,
        service_error: Option<String>,
    },
    RemoveFailed(String),
    Sessions {
        workspace: WorkspaceItem,
        sessions: Vec<SessionItem>,
    },
    SessionsFailed(String),
}

struct Worker {
    receiver: Receiver<WorkerResult>,
    handle: Option<JoinHandle<()>>,
    mutation: bool,
}

impl Worker {
    fn join(mut self) -> bool {
        self.handle
            .take()
            .is_some_and(|handle| handle.join().is_err())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.mutation
            && let Some(handle) = self.handle.take()
        {
            let _ = handle.join();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TerminalSize {
    pub(crate) width: u16,
    pub(crate) height: u16,
}

/// Persistent state for the interactive Pix application.
///
/// Screens only render from this state. They do not own a nested event loop,
/// raw mode, or terminal cleanup, which keeps route changes inside one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct App {
    pub(crate) route: Route,
    pub(crate) history: Vec<Route>,
    selection_history: Vec<usize>,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) toast: Option<Toast>,
    pub(crate) should_quit: bool,
    quit_requested: bool,
    pub(crate) selected: usize,
    pub(crate) terminal_size: TerminalSize,
    pub(crate) workspaces: Vec<WorkspaceItem>,
    pub(crate) sessions: Vec<SessionItem>,
    pub(crate) active_workspace_id: Option<Uuid>,
    pub(crate) selected_workspace_id: Option<Uuid>,
    toast_until: Option<Instant>,
    pending_action: Option<PendingAction>,
    busy: Option<String>,
    mutation_in_flight: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub(crate) fn new() -> Self {
        Self {
            route: Route::Home,
            history: Vec::new(),
            selection_history: Vec::new(),
            overlay: None,
            toast: None,
            should_quit: false,
            quit_requested: false,
            selected: 0,
            terminal_size: TerminalSize::default(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
            active_workspace_id: None,
            selected_workspace_id: None,
            toast_until: None,
            pending_action: None,
            busy: None,
            mutation_in_flight: false,
        }
    }

    pub(crate) const fn is_home(&self) -> bool {
        matches!(self.route, Route::Home)
    }

    pub(crate) fn set_terminal_size(&mut self, size: TerminalSize) {
        self.terminal_size = size;
    }

    pub(crate) fn refresh_workspaces(&mut self, store: &ConfigStore) -> anyhow::Result<()> {
        let selected_id = self.selected_workspace_id;
        let config = commands::shared::load_or_ephemeral_config(store)?;
        self.workspaces = config
            .workspaces
            .into_iter()
            .map(|workspace| WorkspaceItem {
                id: workspace.id,
                name: workspace.name,
                path: workspace.path,
            })
            .collect();
        self.selected_workspace_id =
            selected_id.filter(|id| self.workspaces.iter().any(|workspace| workspace.id == *id));
        if self.selected_workspace_id.is_none() {
            self.selected_workspace_id = self
                .workspaces
                .get(self.selected)
                .map(|workspace| workspace.id);
        }
        self.selected = self
            .selected_workspace_id
            .and_then(|id| {
                self.workspaces
                    .iter()
                    .position(|workspace| workspace.id == id)
            })
            .unwrap_or_else(|| self.selected.min(self.workspaces.len().saturating_sub(1)));
        Ok(())
    }

    pub(crate) fn selected_workspace(&self) -> Option<&WorkspaceItem> {
        self.workspaces.get(self.selected)
    }

    fn set_toast(&mut self, message: impl Into<String>, tone: ToastTone) {
        self.toast = Some(Toast {
            message: message.into(),
            tone,
        });
        self.toast_until = Some(Instant::now() + Duration::from_secs(4));
    }

    fn clear_expired_toast(&mut self) {
        if self
            .toast_until
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.toast = None;
            self.toast_until = None;
        }
    }

    fn request_quit(&mut self) {
        if self.mutation_in_flight {
            self.quit_requested = true;
            self.overlay = None;
        } else {
            self.should_quit = true;
        }
    }

    pub(crate) fn handle_event(&mut self, event: &Event) {
        if let Event::Resize(width, height) = event {
            self.set_terminal_size(TerminalSize {
                width: *width,
                height: *height,
            });
            return;
        }

        let Event::Key(key) = event else {
            return;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }

        // Ctrl-C is a global action. Handle it before overlays so the help
        // surface cannot swallow the quit contract it advertises.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            self.request_quit();
            return;
        }

        // Background mutations own the pending action until their result is
        // applied. Keep navigation/quit responsive, but do not enqueue a
        // second mutation against a stale list while one is in flight.
        if self.busy.is_some() {
            if event::is_quit(event) {
                if self.mutation_in_flight {
                    self.request_quit();
                } else if self.overlay.take().is_none() {
                    self.go_back_or_quit();
                }
            }
            return;
        }

        if self.overlay.is_some() {
            self.handle_overlay_key(key.code);
            return;
        }

        if event::is_quit(event) {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                self.should_quit = true;
            } else {
                self.go_back_or_quit();
            }
            return;
        }

        match key.code {
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Char('a' | 'A') if self.route == Route::Workspaces => {
                self.overlay = Some(Overlay::Add(AddOverlay::new()));
            }
            KeyCode::Char('r' | 'R') if self.route == Route::Workspaces => {
                if let Some(workspace) = self.selected_workspace().cloned() {
                    self.overlay = Some(Overlay::Remove(RemoveOverlay {
                        workspace,
                        selected: 1,
                    }));
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home => self.select_index(0),
            KeyCode::End => self.select_index(self.max_selection()),
            KeyCode::Enter => self.activate_selection(),
            KeyCode::Char(value) if value.is_ascii_digit() && self.is_home() => {
                if let Some(index) = value
                    .to_digit(10)
                    .and_then(|value| usize::try_from(value).ok())
                    .and_then(|value| value.checked_sub(1))
                    .and_then(Route::from_home_index)
                {
                    self.open(index);
                }
            }
            _ => {}
        }
    }

    fn max_selection(&self) -> usize {
        match self.route {
            Route::Home => 3,
            Route::Workspaces => self.workspaces.len().saturating_sub(1),
            Route::WorkspaceSessions => self.sessions.len().saturating_sub(1),
            Route::Devices | Route::Settings | Route::Status => 0,
        }
    }

    fn move_selection(&mut self, direction: i8) {
        let max = self.max_selection();
        let next = if direction.is_negative() {
            self.selected.saturating_sub(1)
        } else {
            (self.selected + 1).min(max)
        };
        self.select_index(next);
    }

    fn select_index(&mut self, index: usize) {
        self.selected = index.min(self.max_selection());
        if self.route == Route::Workspaces {
            self.selected_workspace_id = self.workspaces.get(self.selected).map(|item| item.id);
        }
    }

    fn activate_selection(&mut self) {
        match self.route {
            Route::Home => {
                if let Some(route) = Route::from_home_index(self.selected) {
                    self.open(route);
                }
            }
            Route::Workspaces => {
                if let Some(workspace) = self.selected_workspace() {
                    self.pending_action = Some(PendingAction::OpenSessions(workspace.id));
                } else {
                    self.overlay = Some(Overlay::Add(AddOverlay::new()));
                }
            }
            Route::WorkspaceSessions => {}
            Route::Devices | Route::Settings | Route::Status => {
                self.set_toast(
                    format!(
                        "{} actions are coming in a follow-up issue",
                        self.route.title()
                    ),
                    ToastTone::Info,
                );
            }
        }
    }

    fn open(&mut self, route: Route) {
        if route == self.route {
            return;
        }
        self.history.push(self.route);
        self.selection_history.push(self.selected);
        self.route = route;
        self.selected = 0;
        self.toast = None;
        self.toast_until = None;
    }

    fn go_back_or_quit(&mut self) {
        if let Some(route) = self.history.pop() {
            self.route = route;
            self.selected = self.selection_history.pop().unwrap_or(0);
            self.toast = None;
            self.toast_until = None;
        } else {
            self.should_quit = true;
        }
    }

    #[allow(clippy::too_many_lines)]
    fn handle_overlay_key(&mut self, code: KeyCode) {
        let mut action = None;
        match self.overlay.as_mut() {
            Some(Overlay::Help) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?' | 'q' | 'Q')) {
                    self.overlay = None;
                }
            }
            Some(Overlay::Remove(overlay)) => match code {
                KeyCode::Up | KeyCode::Left => {
                    overlay.selected = overlay.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Right => overlay.selected = (overlay.selected + 1).min(1),
                KeyCode::Home => overlay.selected = 0,
                KeyCode::End => overlay.selected = 1,
                KeyCode::Enter if overlay.selected == 0 => {
                    action = Some(PendingAction::Remove(overlay.workspace.id));
                }
                KeyCode::Esc | KeyCode::Char('q' | 'Q') | KeyCode::Enter => self.overlay = None,
                _ => {}
            },
            Some(Overlay::Add(overlay)) => {
                if let Some(input) = overlay.input.as_mut() {
                    match code {
                        KeyCode::Esc => overlay.input = None,
                        KeyCode::Enter => {
                            let value = input.value.trim().to_owned();
                            if value.is_empty() {
                                self.set_toast("Enter a workspace path", ToastTone::Error);
                            } else {
                                action = Some(PendingAction::Add(PathBuf::from(value)));
                            }
                        }
                        KeyCode::Char(character) => {
                            input.value.insert(input.cursor, character);
                            input.cursor += character.len_utf8();
                        }
                        KeyCode::Backspace => {
                            if input.cursor > 0 {
                                let previous = input.value[..input.cursor]
                                    .char_indices()
                                    .next_back()
                                    .map_or(0, |(index, _)| index);
                                input.value.drain(previous..input.cursor);
                                input.cursor = previous;
                            }
                        }
                        KeyCode::Delete => {
                            if input.cursor < input.value.len() {
                                let next = input.value[input.cursor..]
                                    .char_indices()
                                    .nth(1)
                                    .map_or(input.value.len(), |(index, _)| input.cursor + index);
                                input.value.drain(input.cursor..next);
                            }
                        }
                        KeyCode::Left => {
                            input.cursor = input.value[..input.cursor]
                                .char_indices()
                                .next_back()
                                .map_or(0, |(index, _)| index);
                        }
                        KeyCode::Right => {
                            input.cursor = input.value[input.cursor..]
                                .char_indices()
                                .nth(1)
                                .map_or(input.value.len(), |(index, _)| input.cursor + index);
                        }
                        KeyCode::Home => input.cursor = 0,
                        KeyCode::End => input.cursor = input.value.len(),
                        _ => {}
                    }
                } else {
                    match code {
                        KeyCode::Esc | KeyCode::Char('q' | 'Q') => self.overlay = None,
                        KeyCode::Up | KeyCode::Char('k') => {
                            overlay.selected = overlay.selected.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            overlay.selected = (overlay.selected + 1)
                                .min(overlay.option_count().saturating_sub(1));
                        }
                        KeyCode::Home => overlay.selected = 0,
                        KeyCode::End => overlay.selected = overlay.option_count().saturating_sub(1),
                        KeyCode::Enter => {
                            if let Some(candidate) = overlay.candidates.get(overlay.selected) {
                                action = Some(PendingAction::Add(candidate.path.clone()));
                            } else {
                                overlay.input = Some(TextInput {
                                    value: String::new(),
                                    cursor: 0,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            None => {}
        }
        if action.is_some() {
            self.pending_action = action;
        }
    }

    #[allow(clippy::too_many_lines)]
    fn apply_worker_result(
        &mut self,
        result: WorkerResult,
        store: &ConfigStore,
        overview: &mut HostOverview,
    ) {
        self.busy = None;
        self.mutation_in_flight = false;
        match result {
            WorkerResult::Added {
                workspace,
                service_error,
            } => {
                let id = workspace.id;
                if let Err(error) = self.refresh_workspaces(store) {
                    self.set_toast(
                        format!("Added workspace, reload failed: {error}"),
                        ToastTone::Error,
                    );
                } else {
                    self.selected_workspace_id = Some(id);
                    self.select_index(
                        self.workspaces
                            .iter()
                            .position(|item| item.id == id)
                            .unwrap_or(0),
                    );
                    let service_failed = service_error.is_some();
                    let message = service_error.map_or_else(
                        || format!("Added workspace · {}", workspace.name),
                        |error| format!("Added workspace · host refresh failed: {error}"),
                    );
                    self.set_toast(
                        message,
                        if service_failed {
                            ToastTone::Error
                        } else {
                            ToastTone::Success
                        },
                    );
                }
                overview.workspaces = self.workspaces.len();
                self.overlay = None;
            }
            WorkerResult::AddFailed(error) => {
                self.set_toast(
                    format!("Could not add workspace: {error}"),
                    ToastTone::Error,
                );
            }
            WorkerResult::Removed {
                workspace,
                service_error,
            } => {
                let old_index = self.selected;
                if let Err(error) = self.refresh_workspaces(store) {
                    self.set_toast(
                        format!("Removed workspace, reload failed: {error}"),
                        ToastTone::Error,
                    );
                } else {
                    self.selected = old_index.min(self.workspaces.len().saturating_sub(1));
                    self.selected_workspace_id =
                        self.workspaces.get(self.selected).map(|item| item.id);
                    let service_failed = service_error.is_some();
                    let message = service_error.map_or_else(
                        || format!("Removed workspace · {}", workspace.name),
                        |error| format!("Removed workspace · host refresh failed: {error}"),
                    );
                    self.set_toast(
                        message,
                        if service_failed {
                            ToastTone::Error
                        } else {
                            ToastTone::Success
                        },
                    );
                }
                overview.workspaces = self.workspaces.len();
                self.overlay = None;
            }
            WorkerResult::RemoveFailed(error) => {
                self.set_toast(
                    format!("Could not remove workspace: {error}"),
                    ToastTone::Error,
                );
                self.overlay = None;
            }
            WorkerResult::Sessions {
                workspace,
                sessions,
            } => {
                self.active_workspace_id = Some(workspace.id);
                self.sessions = sessions;
                // `open` stores the current workspace selection before it
                // resets the sessions cursor. Keep this assignment in
                // `open`, not before it, so Esc returns to the same row.
                if self.route == Route::Workspaces {
                    self.open(Route::WorkspaceSessions);
                }
            }
            WorkerResult::SessionsFailed(error) => {
                self.set_toast(
                    format!("Could not load sessions: {error}"),
                    ToastTone::Error,
                );
            }
        }
        if self.quit_requested {
            self.should_quit = true;
        }
    }
}

fn spawn_worker(store: &ConfigStore, action: PendingAction) -> Worker {
    let (sender, receiver) = mpsc::channel();
    let store = store.clone();
    let mutation = matches!(&action, PendingAction::Add(_) | PendingAction::Remove(_));
    let handle = std::thread::spawn(move || {
        let result = match action {
            PendingAction::Add(path) => {
                let path = commands::shared::expand_home(path);
                match commands::workspace::authorize_workspace(&store, &path, None) {
                    Ok(workspace) => WorkerResult::Added {
                        service_error: commands::shared::refresh_running_service(&store)
                            .err()
                            .map(|error| error.to_string()),
                        workspace,
                    },
                    Err(error) => WorkerResult::AddFailed(error.to_string()),
                }
            }
            PendingAction::Remove(id) => match commands::workspace::revoke_workspace(&store, id) {
                Ok(workspace) => WorkerResult::Removed {
                    service_error: commands::shared::refresh_running_service(&store)
                        .err()
                        .map(|error| error.to_string()),
                    workspace,
                },
                Err(error) => WorkerResult::RemoveFailed(error.to_string()),
            },
            PendingAction::OpenSessions(id) => match load_sessions(&store, id) {
                Ok((workspace, sessions)) => WorkerResult::Sessions {
                    workspace,
                    sessions,
                },
                Err(error) => WorkerResult::SessionsFailed(error.to_string()),
            },
        };
        let _ = sender.send(result);
    });
    Worker {
        receiver,
        handle: Some(handle),
        mutation,
    }
}

/// Runs the persistent application shell. Every route renders from one state
/// machine and one alternate-screen terminal, so navigation never appends old
/// screens to terminal history.
pub(crate) fn run(overview: &HostOverview, store: &ConfigStore) -> Result<()> {
    let mut guard = TerminalGuard::enter()?;
    let result = run_loop(&mut guard, store, overview);
    let cleanup = guard.restore();

    match result {
        Err(error) => Err(error),
        Ok(()) => cleanup.map_err(Into::into),
    }
}

fn run_loop(guard: &mut TerminalGuard, store: &ConfigStore, overview: &HostOverview) -> Result<()> {
    let mut app = App::new();
    let mut overview = overview.clone();
    let mut worker: Option<Worker> = None;
    if let Err(error) = app.refresh_workspaces(store) {
        app.set_toast(
            format!("Could not load workspaces: {error}"),
            ToastTone::Error,
        );
    }
    loop {
        app.clear_expired_toast();
        if worker.is_none()
            && let Some(action) = app.pending_action.take()
        {
            app.busy = Some(action.label().to_owned());
            let active = spawn_worker(store, action);
            app.mutation_in_flight = active.mutation;
            worker = Some(active);
        }
        let worker_result = worker.as_ref().map(|active| active.receiver.try_recv());
        if let Some(result) = worker_result {
            match result {
                Ok(result) => {
                    let active = worker.take().expect("worker exists while receiving result");
                    let worker_panicked = active.join();
                    app.apply_worker_result(result, store, &mut overview);
                    if worker_panicked && !app.should_quit {
                        app.set_toast("Workspace operation stopped unexpectedly", ToastTone::Error);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    let active = worker.take().expect("worker exists while disconnected");
                    let _ = active.join();
                    app.busy = None;
                    app.mutation_in_flight = false;
                    if app.quit_requested {
                        app.should_quit = true;
                    }
                    if !app.should_quit {
                        app.set_toast("Workspace operation stopped unexpectedly", ToastTone::Error);
                    }
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        guard.terminal_mut().draw(|frame| {
            app.set_terminal_size(TerminalSize {
                width: frame.area().width,
                height: frame.area().height,
            });
            render(frame, &app, &overview);
        })?;

        if app.should_quit {
            // A mutation never reaches this branch while it is in flight:
            // request_quit() defers should_quit until its result is applied.
            // Pure session reads may be abandoned when Ctrl-C is pressed.
            if let Some(active) = worker.take()
                && active.mutation
            {
                let _ = active.join();
            }
            break;
        }
        if let Some(event) = event::poll(Duration::from_millis(50))? {
            app.handle_event(&event);
        }
    }
    Ok(())
}

fn render(frame: &mut Frame<'_>, app: &App, overview: &HostOverview) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_small_terminal(frame, area);
        if let Some(overlay) = &app.overlay {
            render_overlay(frame, area, app, overlay);
        }
        return;
    }

    let feedback_height = u16::from(app.toast.is_some()) + u16::from(app.busy.is_some());
    let [body, feedback, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(feedback_height),
            Constraint::Length(1),
        ])
        .areas(area);

    match app.route {
        Route::Home => render_home(frame, body, app, overview),
        Route::Workspaces => render_workspaces(frame, body, app),
        Route::WorkspaceSessions => render_sessions(frame, body, app),
        route => render_child(frame, body, route, overview),
    }
    render_feedback(frame, feedback, app);
    render_footer(frame, footer, app);

    if let Some(overlay) = &app.overlay {
        render_overlay(frame, area, app, overlay);
    }
}

fn render_small_terminal(frame: &mut Frame<'_>, area: Rect) {
    let message = vec![
        Line::from(Span::styled(
            "Pix needs a larger terminal",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Resize to at least {MIN_WIDTH}×{MIN_HEIGHT}; press q to go back or quit."
        )),
    ];
    frame.render_widget(
        Paragraph::new(message)
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

#[allow(clippy::too_many_lines)]
fn render_home(frame: &mut Frame<'_>, area: Rect, app: &App, overview: &HostOverview) {
    let [banner, summary, menu] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HOME_BANNER_HEIGHT),
            Constraint::Length(HOME_SUMMARY_HEIGHT),
            Constraint::Min(1),
        ])
        .areas(area);

    let logo_lines = LOGO
        .lines()
        .enumerate()
        .map(|(index, value)| {
            let mut spans = vec![Span::styled(
                format!("  {value:<20}"),
                Style::default().fg(Color::Cyan),
            )];
            if index == 4 {
                spans.push(Span::styled(
                    "pix.deepoke.com",
                    Style::default().fg(Color::Cyan),
                ));
            } else if index == 5 {
                spans.push(Span::styled(
                    "Remote access for the Pi agent",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        })
        .chain([
            Line::from(Span::styled(
                format!("  pix {}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(Color::DarkGray),
            )),
            version_mismatch_hint(overview),
        ])
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(logo_lines), banner);

    let summary_lines = vec![
        status_line(
            "Host",
            overview.host.as_deref().unwrap_or("not configured"),
            Style::default().fg(match overview.config_state {
                ConfigState::Ready => Color::White,
                ConfigState::Missing => Color::Yellow,
                ConfigState::Invalid => Color::Red,
            }),
        ),
        status_line(
            "Pi",
            pi_summary(overview),
            Style::default().fg(Color::DarkGray),
        ),
        status_line(
            "Service",
            service_summary(overview),
            service_style(overview),
        ),
        status_line("Relay", relay_summary(overview), relay_style(overview)),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!(
                    "{} paired  ·  {} authorized",
                    overview.devices, overview.workspaces
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(summary_lines), summary);

    let descriptions = [
        (
            "Devices",
            "Pair, approve, or revoke a phone",
            format!("{} paired", overview.devices),
        ),
        (
            "Workspaces",
            "Manage authorized folders",
            format!("{} authorized", overview.workspaces),
        ),
        ("Settings", "Configure remote access", String::new()),
        ("Status", "Inspect detailed host state", String::new()),
    ];
    let show_descriptions = area.width >= 68;
    let menu_lines = descriptions
        .iter()
        .enumerate()
        .map(|(index, (label, description, detail))| {
            let selected = index == app.selected;
            let marker_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let label_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let detail_style = Style::default().fg(Color::DarkGray);
            let detail_text = if show_descriptions && detail.is_empty() {
                (*description).to_string()
            } else if show_descriptions {
                format!("{description}  {detail}")
            } else {
                detail.clone()
            };
            Line::from(vec![
                Span::styled(if selected { "❯" } else { " " }, marker_style),
                Span::raw(" "),
                Span::styled(format!("{}. {:<12}", index + 1, label), label_style),
                Span::styled(detail_text, detail_style),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(menu_lines).wrap(Wrap { trim: true }), menu);
}

fn render_workspace_header(frame: &mut Frame<'_>, area: Rect, title: &str, detail: &str) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "pix",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" › ", Style::default().fg(Color::DarkGray)),
            Span::styled(title, Style::default().fg(Color::White)),
            Span::styled(format!("  {detail}"), Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn render_workspaces(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let [header, list_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .areas(area);
    render_workspace_header(
        frame,
        header,
        "Workspaces",
        &format!("{} authorized", app.workspaces.len()),
    );
    if app.workspaces.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No authorized workspaces yet.",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from("Press A to add a workspace."),
            ])
            .wrap(Wrap { trim: true }),
            list_area,
        );
        return;
    }

    let capacity = usize::from(list_area.height).max(1);
    let (start, end) = visible_range(app.selected, app.workspaces.len(), capacity);
    let items = app.workspaces[start..end]
        .iter()
        .map(|workspace| {
            let name_width = usize::from(list_area.width.saturating_sub(8)).clamp(12, 32);
            let path_width = usize::from(list_area.width)
                .saturating_sub(name_width + 6)
                .max(12);
            let name = truncate_end(
                &commands::shared::terminal_label(&workspace.name),
                name_width,
            );
            let path = truncate_path(&workspace.path, path_width);
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{name:<name_width$}"),
                    Style::default().fg(Color::White),
                ),
                Span::styled(path, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(app.selected.saturating_sub(start)));
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("❯ ").highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        list_area,
        &mut state,
    );
}

fn render_sessions(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let [header, list_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .areas(area);
    let workspace_name = app
        .active_workspace_id
        .and_then(|id| app.workspaces.iter().find(|workspace| workspace.id == id))
        .map_or_else(
            || "Workspace".to_owned(),
            |workspace| commands::shared::terminal_label(&workspace.name),
        );
    render_workspace_header(
        frame,
        header,
        &format!("Sessions · {workspace_name}"),
        &format!(
            "{} session{}",
            app.sessions.len(),
            if app.sessions.len() == 1 { "" } else { "s" }
        ),
    );
    if app.sessions.is_empty() {
        frame.render_widget(
            Paragraph::new("No Pi sessions stored in this workspace yet.")
                .wrap(Wrap { trim: true }),
            list_area,
        );
        return;
    }
    let capacity = usize::from(list_area.height).max(1);
    let (start, end) = visible_range(app.selected, app.sessions.len(), capacity);
    let items = app.sessions[start..end]
        .iter()
        .map(|session| {
            let title = session.title.as_deref().unwrap_or("Untitled session");
            let title = commands::shared::terminal_label(title);
            let title = truncate_end(
                &title,
                usize::from(list_area.width).saturating_sub(42).max(16),
            );
            let count = format!(
                "{} message{}",
                session.message_count,
                if session.message_count == 1 { "" } else { "s" }
            );
            ListItem::new(Line::from(vec![
                Span::styled(title, Style::default().fg(Color::White)),
                Span::styled(
                    format!("  {}  {count}", session.modified_at),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(app.selected.saturating_sub(start)));
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("❯ ").highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        list_area,
        &mut state,
    );
}

fn visible_range(selected: usize, length: usize, capacity: usize) -> (usize, usize) {
    if length == 0 {
        return (0, 0);
    }
    let capacity = capacity.max(1).min(length);
    let start = selected
        .min(length - 1)
        .saturating_sub(capacity.saturating_sub(1));
    let start = start.min(length.saturating_sub(capacity));
    (start, (start + capacity).min(length))
}

fn truncate_end(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 1 {
        return "…".chars().take(max_chars).collect();
    }
    let mut result = value.chars().take(max_chars - 1).collect::<String>();
    result.push('…');
    result
}

fn truncate_path(path: &std::path::Path, max_chars: usize) -> String {
    let display = commands::shared::terminal_label(&commands::shared::display_workspace_path(path));
    if display.chars().count() <= max_chars {
        return display;
    }
    if max_chars <= 1 {
        return "…".chars().take(max_chars).collect();
    }
    let suffix = display
        .chars()
        .rev()
        .take(max_chars - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("…{suffix}")
}

fn render_child(frame: &mut Frame<'_>, area: Rect, route: Route, overview: &HostOverview) {
    let detail = match route {
        Route::Devices => format!("{} paired", overview.devices),
        Route::Settings
        | Route::Status
        | Route::Home
        | Route::Workspaces
        | Route::WorkspaceSessions => String::new(),
    };
    let header = Line::from(vec![
        Span::styled(
            "pix",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" › ", Style::default().fg(Color::DarkGray)),
        Span::styled(route.title(), Style::default().fg(Color::White)),
        if detail.is_empty() {
            Span::raw("")
        } else {
            Span::styled(format!("  {detail}"), Style::default().fg(Color::DarkGray))
        },
    ]);

    let content = match route {
        Route::Devices => {
            let [message, detail] = placeholder_message(route);
            vec![
                header,
                Line::from(""),
                Line::from(message),
                Line::from(detail),
            ]
        }
        Route::Settings => vec![
            header,
            Line::from(""),
            status_line("Relay", relay_summary(overview), relay_style(overview)),
            status_line(
                "Pi",
                pi_summary(overview),
                Style::default().fg(Color::DarkGray),
            ),
            Line::from("Settings controls will be added without changing explicit CLI commands."),
        ],
        Route::Status => vec![
            header,
            Line::from(""),
            status_line(
                "Config",
                match overview.config_state {
                    ConfigState::Ready => "ready",
                    ConfigState::Missing => "not configured",
                    ConfigState::Invalid => "needs attention",
                },
                Style::default().fg(Color::DarkGray),
            ),
            status_line(
                "Host",
                overview.host.as_deref().unwrap_or("not configured"),
                Style::default(),
            ),
            status_line(
                "Service",
                service_summary(overview),
                service_style(overview),
            ),
            status_line(
                "Config path",
                &overview.config_path,
                Style::default().fg(Color::DarkGray),
            ),
        ],
        Route::Home | Route::Workspaces | Route::WorkspaceSessions => {
            unreachable!("persistent routes are rendered separately")
        }
    };
    frame.render_widget(Paragraph::new(content).wrap(Wrap { trim: true }), area);
}

fn placeholder_message(route: Route) -> [&'static str; 2] {
    match route {
        Route::Devices => [
            "Device management will be available here after",
            "the Devices TUI migration.",
        ],
        Route::Workspaces => [
            "Workspace management will be available here after",
            "the Workspaces TUI migration.",
        ],
        Route::Home | Route::WorkspaceSessions | Route::Settings | Route::Status => {
            unreachable!("only device and workspace routes have placeholder messages")
        }
    }
}

fn render_overlay(frame: &mut Frame<'_>, area: Rect, app: &App, overlay: &Overlay) {
    match overlay {
        Overlay::Help => render_help(frame, area, app.route, app.workspaces.len()),
        Overlay::Add(add) => render_add_overlay(frame, area, add),
        Overlay::Remove(remove) => render_remove_overlay(frame, area, remove),
    }
}

fn centered_popup(area: Rect, width: u16, height: u16) -> Option<Rect> {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    if width == 0 || height == 0 {
        return None;
    }
    Some(Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    })
}

fn render_add_overlay(frame: &mut Frame<'_>, area: Rect, overlay: &AddOverlay) {
    let height = u16::try_from(overlay.option_count())
        .unwrap_or(u16::MAX)
        .saturating_add(5)
        .min(area.height.saturating_sub(2));
    let Some(popup) = centered_popup(area, 78, height.max(7)) else {
        return;
    };
    frame.render_widget(Clear, popup);
    if let Some(input) = &overlay.input {
        let content = vec![
            Line::from(Span::styled(
                "Enter another workspace path",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("› ", Style::default().fg(Color::Cyan)),
                input_line(input, usize::from(popup.width).saturating_sub(6)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Enter confirm   Esc cancel",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        frame.render_widget(
            Paragraph::new(content)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Add workspace "),
                )
                .wrap(Wrap { trim: true }),
            popup,
        );
        return;
    }

    let mut items = overlay
        .candidates
        .iter()
        .map(|candidate| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    truncate_end(
                        &commands::shared::terminal_label(
                            &commands::shared::display_workspace_path(&candidate.path),
                        ),
                        52,
                    ),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("  ({})", candidate.label),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    items.push(ListItem::new(Line::from(Span::styled(
        "Enter another path…",
        Style::default().fg(Color::Yellow),
    ))));
    let mut state = ListState::default();
    state.select(Some(overlay.selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Add workspace · choose a folder "),
            )
            .highlight_symbol("❯ ")
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        popup,
        &mut state,
    );
}

/// Renders an editable path with the visual cursor at the same byte boundary
/// used by the input state. The compact horizontal window keeps the cursor
/// visible while editing paths longer than the modal.
fn input_line(input: &TextInput, max_chars: usize) -> Span<'static> {
    let chars = input.value.chars().collect::<Vec<_>>();
    let cursor = input.value[..input.cursor].chars().count();
    let max_chars = max_chars.max(3);
    let mut start = cursor.saturating_sub(max_chars / 2);
    let mut end = (start + max_chars).min(chars.len());
    if end.saturating_sub(start) < max_chars {
        start = end.saturating_sub(max_chars);
    }
    if cursor < start {
        start = cursor;
    }
    if cursor > end {
        end = cursor.min(chars.len());
    }
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < chars.len() { "…" } else { "" };
    let before = chars[start..cursor.min(end)].iter().collect::<String>();
    let after = chars[cursor.min(end)..end].iter().collect::<String>();
    Span::styled(
        format!("{prefix}{before}▌{after}{suffix}"),
        Style::default().fg(Color::White),
    )
}

fn render_remove_overlay(frame: &mut Frame<'_>, area: Rect, overlay: &RemoveOverlay) {
    let Some(popup) = centered_popup(area, 68, 10) else {
        return;
    };
    frame.render_widget(Clear, popup);
    let content = vec![
        Line::from(Span::styled(
            format!(
                "Remove authorization for {}?",
                commands::shared::terminal_label(&overlay.workspace.name)
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            truncate_path(
                &overlay.workspace.path,
                usize::from(popup.width).saturating_sub(6),
            ),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(if overlay.selected == 0 {
            "❯ Remove workspace"
        } else {
            "  Remove workspace"
        }),
        Line::from(if overlay.selected == 1 {
            "❯ Cancel"
        } else {
            "  Cancel"
        }),
        Line::from(""),
        Line::from(Span::styled(
            "↑↓ choose   Enter confirm   Esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Confirm removal "),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn render_feedback(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines = Vec::new();
    if let Some(busy) = &app.busy {
        let busy = if app.quit_requested {
            "Finishing workspace operation…"
        } else {
            busy
        };
        lines.push(Line::from(vec![
            Span::styled("  ⠋ ", Style::default().fg(Color::Cyan)),
            Span::styled(busy, Style::default().fg(Color::Cyan)),
        ]));
    }
    if let Some(toast) = &app.toast {
        let style = match toast.tone {
            ToastTone::Info => Style::default().fg(Color::Cyan),
            ToastTone::Success => Style::default().fg(Color::Green),
            ToastTone::Error => Style::default().fg(Color::Red),
        };
        lines.push(Line::from(vec![
            Span::styled("  ", style),
            Span::styled(&toast.message, style),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let hints = footer_hints(area.width, app.route, app.workspaces.len());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled(hints, Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn footer_hints(width: u16, route: Route, workspace_count: usize) -> &'static str {
    if width < 72 {
        match route {
            Route::Home => "↑↓/jk Move   ↵ Open   ? Help   Q Quit",
            Route::Workspaces if workspace_count == 0 => "A Add  ↵ Add  Esc  ?",
            Route::Workspaces => "↑↓ Move  ↵ Sessions  A Add  R Remove  Esc  ?",
            Route::WorkspaceSessions => "↑↓/jk Navigate   Esc Back   ? Help",
            Route::Devices | Route::Settings | Route::Status => "Esc/q Back   ? Help",
        }
    } else {
        match route {
            Route::Home => "↑↓/jk Navigate   ↵ Open   1–4 Jump   ? Help   Q Quit",
            Route::Workspaces if workspace_count == 0 => "A Add   ↵ Add   Esc Back   ? Help",
            Route::Workspaces => {
                "↑↓/jk Navigate   ↵ Sessions   A Add   R Remove   Esc Back   ? Help"
            }
            Route::WorkspaceSessions => "↑↓/jk Navigate   Esc Back   ? Help",
            Route::Devices | Route::Settings | Route::Status => "Esc/q Back   ? Help",
        }
    }
}

fn render_help(frame: &mut Frame<'_>, area: Rect, route: Route, workspace_count: usize) {
    let width = 60.min(area.width.saturating_sub(2));
    if width == 0 {
        return;
    }
    let mut help_lines = vec![
        Line::from(Span::styled(
            "Pix navigation",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    let shortcuts = help_shortcuts(route, workspace_count);
    help_lines.extend(shortcuts.iter().copied().map(Line::from));
    if !shortcuts.is_empty() {
        help_lines.push(Line::from(""));
    }
    help_lines.extend([
        Line::from("Esc / q     go back; quit from Home"),
        Line::from("?           close this help"),
        Line::from("Ctrl-C      quit Pix"),
        Line::from(""),
        Line::from("Resize the terminal to redraw the current frame."),
    ]);
    let inner_width = usize::from(width.saturating_sub(2)).max(1);
    let content_height = help_lines
        .iter()
        .map(|line| line.width().div_ceil(inner_width).max(1))
        .sum::<usize>();
    let requested_height = content_height.saturating_add(2);
    let height = u16::try_from(requested_height)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2));
    if height == 0 {
        return;
    }
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(help_lines)
            .block(Block::default().borders(Borders::ALL).title(" Help "))
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn help_shortcuts(route: Route, workspace_count: usize) -> &'static [&'static str] {
    match route {
        Route::Home => &["↑↓ or j/k   navigate", "Enter       open"],
        Route::Workspaces if workspace_count == 0 => &["A or Enter  add workspace"],
        Route::Workspaces => &[
            "↑↓ or j/k   navigate workspaces",
            "Enter       show sessions",
            "A           add workspace",
            "R           remove workspace",
        ],
        Route::WorkspaceSessions => &["↑↓ or j/k   navigate sessions"],
        Route::Devices | Route::Settings | Route::Status => &[],
    }
}

fn version_mismatch_message(host_version: &str) -> String {
    format!(
        "  CLI {} · Host {host_version} — restart the host service to use the current CLI version",
        env!("CARGO_PKG_VERSION")
    )
}

fn version_mismatch_hint(overview: &HostOverview) -> Line<'static> {
    match overview.service.pix_version.as_deref() {
        Some(version) if version != env!("CARGO_PKG_VERSION") => Line::from(Span::styled(
            version_mismatch_message(version),
            Style::default().fg(Color::Yellow),
        )),
        _ => Line::from(""),
    }
}

fn status_line(label: &str, value: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<10}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value.into(), style),
    ])
}

fn pi_summary(overview: &HostOverview) -> String {
    match overview.pi.version.as_deref() {
        Some(version) => version.to_owned(),
        None => match overview.pi.compatibility {
            Some(pix_core::PiCompatibilityStatus::NotFound) => "not found".to_owned(),
            Some(pix_core::PiCompatibilityStatus::CannotLaunch) => "could not start".to_owned(),
            _ => "not detected".to_owned(),
        },
    }
}

fn service_summary(overview: &HostOverview) -> String {
    match overview.service.state {
        ServiceState::Running => "● running".to_owned(),
        ServiceState::Stopped => "○ installed, stopped".to_owned(),
        ServiceState::NotInstalled => "○ not installed".to_owned(),
    }
}

fn service_style(overview: &HostOverview) -> Style {
    Style::default().fg(match overview.service.state {
        ServiceState::Running => Color::Green,
        ServiceState::Stopped => Color::Yellow,
        ServiceState::NotInstalled => Color::DarkGray,
    })
}

fn relay_summary(overview: &HostOverview) -> String {
    match overview.access.mode {
        AccessMode::Relay => "● enabled".to_owned(),
        AccessMode::RelayDisabled => "○ disabled".to_owned(),
        AccessMode::Local => "○ local network only".to_owned(),
        AccessMode::Unknown => "unknown".to_owned(),
    }
}

fn relay_style(overview: &HostOverview) -> Style {
    Style::default().fg(match overview.access.mode {
        AccessMode::Relay => Color::Green,
        AccessMode::RelayDisabled => Color::Yellow,
        AccessMode::Local => Color::DarkGray,
        AccessMode::Unknown => Color::Red,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        AddOverlay, App, InteractionMode, MIN_HEIGHT, Overlay, Route, SessionItem, TerminalSize,
        TextInput, TtyState, WorkerResult, WorkspaceItem, footer_hints, help_shortcuts, input_line,
        interaction_mode, placeholder_message, render, should_launch_tui, truncate_end,
        truncate_path, version_mismatch_message, visible_range,
    };
    use crate::home::{AccessOverview, PiOverview, PiSource, ServiceOverview};
    use crate::output::OutputFormat;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use pix_core::{ConfigStore, HostConfig, WorkspaceRegistry};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tempfile::tempdir;

    const TTY: TtyState = TtyState::Attached;

    #[test]
    fn bare_interactive_human_terminal_enters_tui() {
        assert!(should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TTY,
            TTY,
            Some("xterm-256color")
        ));
        assert_eq!(
            interaction_mode(
                false,
                false,
                OutputFormat::Human,
                TTY,
                TTY,
                Some("xterm-256color")
            ),
            InteractionMode::Tui
        );
    }

    #[test]
    fn explicit_status_command_stays_human_cli() {
        assert_eq!(
            interaction_mode(true, false, OutputFormat::Human, TTY, TTY, Some("xterm")),
            InteractionMode::HumanCli
        );
    }

    #[test]
    fn explicit_workspace_command_stays_human_cli() {
        assert!(!should_launch_tui(
            true,
            false,
            OutputFormat::Human,
            TTY,
            TTY,
            Some("xterm")
        ));
    }

    #[test]
    fn machine_output_never_enters_tui() {
        assert_eq!(
            interaction_mode(false, false, OutputFormat::Json, TTY, TTY, Some("xterm")),
            InteractionMode::Machine
        );
    }

    #[test]
    fn no_input_and_non_tty_never_enter_tui() {
        assert!(!should_launch_tui(
            false,
            true,
            OutputFormat::Human,
            TTY,
            TTY,
            Some("xterm")
        ));
        assert!(!should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TtyState::Detached,
            TTY,
            Some("xterm")
        ));
        assert!(!should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TTY,
            TtyState::Detached,
            Some("xterm")
        ));
    }

    #[test]
    fn unusable_term_never_enters_tui() {
        assert!(!should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TTY,
            TTY,
            None
        ));
        assert!(!should_launch_tui(
            false,
            false,
            OutputFormat::Human,
            TTY,
            TTY,
            Some("dumb")
        ));
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn home_navigation_opens_and_back_returns_to_the_same_shell() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Down));
        app.handle_event(&key(KeyCode::Enter));
        assert_eq!(app.route, Route::Workspaces);
        assert_eq!(app.history, vec![Route::Home]);

        app.handle_event(&key(KeyCode::Esc));
        assert_eq!(app.route, Route::Home);
        assert_eq!(app.selected, 1);
        assert!(app.history.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn home_number_shortcuts_open_routes() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Char('4')));
        assert_eq!(app.route, Route::Status);
        app.handle_event(&key(KeyCode::Char('q')));
        assert_eq!(app.route, Route::Home);
        assert!(!app.should_quit);
        app.handle_event(&key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn help_is_a_shared_overlay_and_escape_closes_it() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Char('?')));
        assert!(app.overlay.is_some());
        app.handle_event(&key(KeyCode::Esc));
        assert!(app.overlay.is_none());
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_quits_even_when_help_overlay_is_open() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Char('?')));
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit);
    }

    #[test]
    fn minimum_terminal_height_leaves_room_for_the_home_menu() {
        assert_eq!(MIN_HEIGHT, 19);
    }

    #[test]
    fn placeholder_footers_only_advertise_implemented_actions() {
        for route in [Route::Devices, Route::Settings] {
            assert_eq!(footer_hints(80, route, 1), "Esc/q Back   ? Help");
        }
        assert!(footer_hints(80, Route::Workspaces, 1).contains("A Add"));
        assert!(!footer_hints(80, Route::Workspaces, 0).contains("Sessions"));
    }

    #[test]
    fn placeholder_pages_do_not_advertise_selection_actions() {
        assert_eq!(
            placeholder_message(Route::Devices),
            [
                "Device management will be available here after",
                "the Devices TUI migration."
            ]
        );
        assert_eq!(
            placeholder_message(Route::Workspaces),
            [
                "Workspace management will be available here after",
                "the Workspaces TUI migration."
            ]
        );
    }

    #[test]
    fn help_only_shows_selection_actions_on_home() {
        assert_eq!(
            help_shortcuts(Route::Home, 1),
            &["↑↓ or j/k   navigate", "Enter       open"]
        );
        for route in [Route::Devices, Route::Settings, Route::Status] {
            assert!(help_shortcuts(route, 1).is_empty());
        }
        assert!(!help_shortcuts(Route::Workspaces, 1).is_empty());
        assert_eq!(
            help_shortcuts(Route::Workspaces, 0),
            &["A or Enter  add workspace"]
        );
    }

    #[test]
    fn mutation_quit_waits_for_the_worker_result() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.busy = Some("Removing workspace…".to_owned());
        app.mutation_in_flight = true;

        app.handle_event(&key(KeyCode::Char('q')));

        assert!(app.quit_requested);
        assert!(!app.should_quit);
        assert!(app.overlay.is_none());

        let store = ConfigStore::new("/tmp/pix-tui-test-config.json");
        let mut overview = test_overview(0);
        app.apply_worker_result(
            WorkerResult::RemoveFailed("test failure".to_owned()),
            &store,
            &mut overview,
        );

        assert!(app.should_quit);
        assert!(!app.mutation_in_flight);
        assert!(app.busy.is_none());
    }

    #[test]
    fn ctrl_c_requests_graceful_quit_during_a_mutation() {
        let mut app = App::new();
        app.busy = Some("Adding workspace…".to_owned());
        app.mutation_in_flight = true;

        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));

        assert!(app.quit_requested);
        assert!(!app.should_quit);
    }

    #[test]
    fn session_navigation_restores_the_original_workspace_selection() {
        let first = WorkspaceItem {
            id: uuid::Uuid::new_v4(),
            name: "first".to_owned(),
            path: PathBuf::from("/tmp/first"),
        };
        let second = WorkspaceItem {
            id: uuid::Uuid::new_v4(),
            name: "second".to_owned(),
            path: PathBuf::from("/tmp/second"),
        };
        let second_id = second.id;
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.workspaces = vec![first, second.clone()];
        app.select_index(1);

        let store = ConfigStore::new("/tmp/pix-tui-test-config.json");
        let mut overview = test_overview(2);
        app.apply_worker_result(
            WorkerResult::Sessions {
                workspace: second,
                sessions: Vec::new(),
            },
            &store,
            &mut overview,
        );

        assert_eq!(app.route, Route::WorkspaceSessions);
        assert_eq!(app.selected, 0);
        app.handle_event(&key(KeyCode::Esc));
        assert_eq!(app.route, Route::Workspaces);
        assert_eq!(app.selected, 1);
        assert_eq!(app.selected_workspace_id, Some(second_id));
    }

    #[test]
    fn path_input_cursor_is_rendered_at_the_editing_position() {
        let input = TextInput {
            value: "/tmp/workspace".to_owned(),
            cursor: "/tmp/".len(),
        };
        assert_eq!(input_line(&input, 40).content, "/tmp/▌workspace");
    }

    #[test]
    fn version_mismatch_message_is_neutral_about_update_direction() {
        for host_version in ["0.1.6", "0.1.8"] {
            let message = version_mismatch_message(host_version);
            assert!(message.contains(&format!("CLI {}", env!("CARGO_PKG_VERSION"))));
            assert!(message.contains(&format!("Host {host_version}")));
            assert!(message.contains("restart the host service"));
            assert!(!message.contains("after updating"));
        }
    }

    #[test]
    fn resize_updates_the_app_without_changing_the_route() {
        let mut app = App::new();
        app.handle_event(&Event::Resize(120, 40));
        assert_eq!(
            app.terminal_size,
            TerminalSize {
                width: 120,
                height: 40
            }
        );
        assert_eq!(app.route, Route::Home);
    }

    #[test]
    fn q_quits_only_after_the_route_stack_is_empty() {
        let mut app = App::new();
        app.handle_event(&key(KeyCode::Enter));
        app.handle_event(&key(KeyCode::Char('q')));
        assert_eq!(app.route, Route::Home);
        assert!(!app.should_quit);
        app.handle_event(&key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn add_overlay_keeps_custom_path_entry_inside_the_tui() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.handle_event(&key(KeyCode::Char('a')));
        assert!(matches!(app.overlay, Some(Overlay::Add(_))));

        app.handle_event(&key(KeyCode::End));
        app.handle_event(&key(KeyCode::Enter));
        assert!(matches!(
            app.overlay,
            Some(Overlay::Add(AddOverlay { input: Some(_), .. }))
        ));
        for character in "/tmp/my-workspace".chars() {
            app.handle_event(&key(KeyCode::Char(character)));
        }
        app.handle_event(&key(KeyCode::Enter));
        assert!(matches!(app.overlay, Some(Overlay::Add(_))));
        assert_eq!(app.route, Route::Workspaces);
        assert!(app.pending_action.is_some());
    }

    #[test]
    fn remove_overlay_defaults_to_cancel() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.workspaces.push(WorkspaceItem {
            id: uuid::Uuid::new_v4(),
            name: "workspace".to_owned(),
            path: PathBuf::from("/tmp/workspace"),
        });
        app.handle_event(&key(KeyCode::Char('R')));
        let Some(Overlay::Remove(remove)) = &app.overlay else {
            panic!("expected remove confirmation");
        };
        assert_eq!(remove.selected, 1, "Cancel is the safe default");
        app.handle_event(&key(KeyCode::Enter));
        assert!(app.overlay.is_none());
        assert!(app.pending_action.is_none());
    }

    #[test]
    fn selection_follows_workspace_id_when_the_list_is_reordered() {
        let directory = tempdir().expect("temp directory");
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::create_dir_all(&first).expect("first workspace");
        std::fs::create_dir_all(&second).expect("second workspace");
        let store = ConfigStore::new(directory.path().join("config.json"));
        let mut config = HostConfig::new("test");
        let mut registry = WorkspaceRegistry::new(&mut config);
        let first_id = registry.add(&first, None).expect("first auth").id;
        let second_id = registry.add(&second, None).expect("second auth").id;
        store.save(&config).expect("save config");

        let mut app = App::new();
        app.refresh_workspaces(&store).expect("load workspaces");
        app.selected_workspace_id = Some(second_id);
        app.select_index(1);

        let mut reordered = store.load().expect("load config");
        reordered.workspaces.swap(0, 1);
        store.save(&reordered).expect("save reordered config");
        app.refresh_workspaces(&store).expect("refresh workspaces");

        assert_eq!(app.selected_workspace_id, Some(second_id));
        assert_eq!(app.selected, 0);
        assert_ne!(first_id, second_id);
    }

    #[test]
    fn long_lists_scroll_inside_the_available_viewport() {
        assert_eq!(visible_range(0, 20, 5), (0, 5));
        assert_eq!(visible_range(9, 20, 5), (5, 10));
        assert_eq!(visible_range(19, 20, 5), (15, 20));
    }

    #[test]
    fn paths_and_names_are_truncated_without_panicking() {
        assert_eq!(truncate_end("workspace", 20), "workspace");
        assert_eq!(truncate_end("abcdefghijkl", 5), "abcd…");
        let path = PathBuf::from("/a/very/long/workspace/path/that/does/not/fit");
        let shortened = truncate_path(&path, 12);
        assert_eq!(shortened.chars().count(), 12);
        assert!(shortened.starts_with('…'));
    }

    #[test]
    fn workspace_and_session_routes_render_in_one_frame() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.workspaces.push(WorkspaceItem {
            id: uuid::Uuid::new_v4(),
            name: "my-workspace".to_owned(),
            path: PathBuf::from("/tmp/my-workspace"),
        });
        let overview = test_overview(1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("workspace frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Workspaces"));
        assert!(text.contains("my-workspace"));
        assert!(!text.contains("Authorized folders"));
        assert!(!text.contains('┌'));

        app.route = Route::WorkspaceSessions;
        app.active_workspace_id = app.workspaces.first().map(|workspace| workspace.id);
        app.sessions.push(SessionItem {
            id: "session-1".to_owned(),
            title: Some("Fix the menu".to_owned()),
            modified_at: "2026-09-19T00:00:00Z".to_owned(),
            message_count: 3,
        });
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("sessions frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Sessions"));
        assert!(text.contains("Fix the menu"));
        assert!(text.contains("3 messages"));
        assert!(!text.contains("Pi sessions"));
        assert!(!text.contains('┌'));
    }

    #[test]
    fn narrow_workspace_footer_keeps_escape_action_visible() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.workspaces.push(WorkspaceItem {
            id: uuid::Uuid::new_v4(),
            name: "workspace".to_owned(),
            path: PathBuf::from("/tmp/workspace"),
        });
        let overview = test_overview(1);
        let mut terminal = Terminal::new(TestBackend::new(48, 19)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("narrow workspace frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Esc"));
        assert!(text.contains("R Remove"));
    }

    #[test]
    fn workspace_help_expands_to_show_all_guidance() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.overlay = Some(Overlay::Help);
        app.workspaces.push(WorkspaceItem {
            id: uuid::Uuid::new_v4(),
            name: "workspace".to_owned(),
            path: PathBuf::from("/tmp/workspace"),
        });
        let overview = test_overview(1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("help frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Resize the terminal"));
        assert!(text.contains("current frame."));
    }

    #[test]
    fn narrow_terminal_render_is_safe_for_workspace_state() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.workspaces.push(WorkspaceItem {
            id: uuid::Uuid::new_v4(),
            name: "narrow".to_owned(),
            path: PathBuf::from("/tmp/narrow"),
        });
        let overview = test_overview(1);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("narrow frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("larger terminal"));
    }

    fn test_overview(workspaces: usize) -> super::HostOverview {
        super::HostOverview {
            config_path: "/tmp/pix-config.json".to_owned(),
            config_state: super::ConfigState::Ready,
            config_error: None,
            host: Some("Test Host".to_owned()),
            pi: PiOverview {
                source: PiSource::Path,
                executable: None,
                version: None,
                supported: None,
                compatibility: None,
            },
            service: ServiceOverview {
                state: super::ServiceState::Stopped,
                installed: false,
                pid: None,
                port: None,
                started_at: None,
                pix_version: None,
            },
            access: AccessOverview {
                mode: super::AccessMode::Local,
                relay_enabled: false,
                relay_url: None,
            },
            devices: 0,
            workspaces,
        }
    }

    fn buffer_text(backend: &TestBackend) -> String {
        backend
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
    }
}
