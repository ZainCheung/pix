mod event;
mod terminal;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
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

use crate::app_ops::device as device_ops;
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
    RevokeDevice(RevokeDeviceOverlay),
    Pair(PairingOverlay),
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
pub(crate) struct RevokeDeviceOverlay {
    pub(crate) device_id: String,
    pub(crate) device_name: String,
    /// The destructive action is intentionally not the default.
    pub(crate) selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PairingPhase {
    Starting,
    Waiting { remote: bool },
    Request(PendingRequestItem),
    Approving,
    Denying,
    Success { device_name: String },
    Denied,
    Cancelled,
    Expired,
    TimedOut,
    Error(device_ops::PairingFailure),
}

/// Pairing UI state contains secret material only behind `PairingSecret`'s
/// redacted Debug implementation.  The fields are rendered solely by the
/// dedicated pairing overlay.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PairingOverlay {
    pub(crate) phase: PairingPhase,
    pub(crate) offer: Option<device_ops::PairingOffer>,
    pub(crate) request: Option<PendingRequestItem>,
}

impl std::fmt::Debug for PairingOverlay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingOverlay")
            .field("phase", &self.phase)
            .field("offer", &self.offer)
            .field("request", &self.request)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceItem {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) paired_at: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PendingRequestItem {
    pub(crate) id: Uuid,
    pub(crate) device_name: String,
    pub(crate) confirmation_code: String,
    pub(crate) expires_at: u64,
}

impl std::fmt::Debug for PendingRequestItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingRequestItem")
            .field("id", &self.id)
            .field("device_name", &self.device_name)
            .field("confirmation_code", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl From<device_ops::PendingPairing> for PendingRequestItem {
    fn from(request: device_ops::PendingPairing) -> Self {
        Self {
            id: request.id,
            device_name: request.device_name,
            confirmation_code: request.confirmation_code,
            expires_at: request.expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeviceRowId {
    Pending(Uuid),
    Paired(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingAction {
    Add(PathBuf),
    Remove(Uuid),
    OpenSessions(Uuid),
    RefreshDevices,
    Approve(Uuid),
    Deny(Uuid),
    RevokeDevice(String),
    Pair { remote: bool },
}

impl PendingAction {
    const fn label(&self) -> &'static str {
        match self {
            Self::Add(_) => "Adding workspace…",
            Self::Remove(_) => "Removing workspace…",
            Self::OpenSessions(_) => "Loading sessions…",
            Self::RefreshDevices => "Loading devices…",
            Self::Approve(_) => "Approving pairing…",
            Self::Deny(_) => "Denying pairing…",
            Self::RevokeDevice(_) => "Revoking device…",
            Self::Pair { .. } => "Pairing…",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredNavigation {
    Back,
    Quit,
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
    Devices {
        devices: Vec<DeviceItem>,
        pending: Vec<PendingRequestItem>,
        pending_error: bool,
    },
    DeviceOperationFailed {
        action: &'static str,
    },
    DeviceOperationSucceeded {
        action: &'static str,
        devices: Vec<DeviceItem>,
        pending: Vec<PendingRequestItem>,
        pending_error: bool,
    },
    Pairing(device_ops::PairingOutcome),
}

#[derive(Debug)]
enum WorkerEvent {
    Result(WorkerResult),
    PairingProgress(device_ops::PairingProgress),
}

struct Worker {
    receiver: Receiver<WorkerEvent>,
    handle: Option<JoinHandle<()>>,
    mutation: bool,
    cancel: Option<Arc<AtomicBool>>,
    pairing_commands: Option<Sender<device_ops::PairingCommand>>,
}

impl Worker {
    fn join(mut self) -> bool {
        self.handle
            .take()
            .is_some_and(|handle| handle.join().is_err())
    }

    fn cancel(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
    }

    fn send_pairing_command(&self, command: device_ops::PairingCommand) {
        if let Some(sender) = &self.pairing_commands {
            let _ = sender.send(command);
        }
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
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct App {
    pub(crate) route: Route,
    pub(crate) history: Vec<Route>,
    selection_history: Vec<usize>,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) toast: Option<Toast>,
    pub(crate) should_quit: bool,
    deferred_navigation: Option<DeferredNavigation>,
    pub(crate) selected: usize,
    pub(crate) terminal_size: TerminalSize,
    pub(crate) workspaces: Vec<WorkspaceItem>,
    pub(crate) sessions: Vec<SessionItem>,
    pub(crate) paired_devices: Vec<DeviceItem>,
    pub(crate) pending_requests: Vec<PendingRequestItem>,
    pub(crate) selected_device_id: Option<String>,
    pub(crate) selected_request_id: Option<Uuid>,
    pub(crate) active_workspace_id: Option<Uuid>,
    pub(crate) selected_workspace_id: Option<Uuid>,
    toast_until: Option<Instant>,
    pending_action: Option<PendingAction>,
    busy: Option<String>,
    mutation_in_flight: bool,
    device_refresh_requested: bool,
    pending_pairing_command: Option<device_ops::PairingCommand>,
    cancel_requested: bool,
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
            deferred_navigation: None,
            selected: 0,
            terminal_size: TerminalSize::default(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
            paired_devices: Vec::new(),
            pending_requests: Vec::new(),
            selected_device_id: None,
            selected_request_id: None,
            active_workspace_id: None,
            selected_workspace_id: None,
            toast_until: None,
            pending_action: None,
            busy: None,
            mutation_in_flight: false,
            device_refresh_requested: false,
            pending_pairing_command: None,
            cancel_requested: false,
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

    fn apply_devices(&mut self, devices: Vec<DeviceItem>, pending: Vec<PendingRequestItem>) {
        let selected_row = self.selected_device_row();
        self.paired_devices = devices;
        self.pending_requests = pending;
        let rows = self.device_rows();
        let next_index = selected_row
            .and_then(|row| rows.iter().position(|candidate| *candidate == row))
            .unwrap_or_else(|| self.selected.min(rows.len().saturating_sub(1)));
        self.select_index(next_index);
    }

    fn device_rows(&self) -> Vec<DeviceRowId> {
        self.pending_requests
            .iter()
            .map(|request| DeviceRowId::Pending(request.id))
            .chain(
                self.paired_devices
                    .iter()
                    .map(|device| DeviceRowId::Paired(device.id.clone())),
            )
            .collect()
    }

    fn selected_device_row(&self) -> Option<DeviceRowId> {
        self.selected_request_id
            .map(DeviceRowId::Pending)
            .or_else(|| self.selected_device_id.clone().map(DeviceRowId::Paired))
            .or_else(|| self.device_rows().get(self.selected).cloned())
    }

    fn selected_pending_request(&self) -> Option<&PendingRequestItem> {
        let DeviceRowId::Pending(id) = self.selected_device_row()? else {
            return None;
        };
        self.pending_requests
            .iter()
            .find(|request| request.id == id)
    }

    fn selected_device(&self) -> Option<&DeviceItem> {
        let DeviceRowId::Paired(id) = self.selected_device_row()? else {
            return None;
        };
        self.paired_devices.iter().find(|device| device.id == id)
    }

    fn pairing_overlay(&self) -> Option<&PairingOverlay> {
        match self.overlay.as_ref() {
            Some(Overlay::Pair(pairing)) => Some(pairing),
            _ => None,
        }
    }

    fn pairing_overlay_mut(&mut self) -> Option<&mut PairingOverlay> {
        match self.overlay.as_mut() {
            Some(Overlay::Pair(pairing)) => Some(pairing),
            _ => None,
        }
    }

    fn begin_pairing(&mut self) {
        self.overlay = Some(Overlay::Pair(PairingOverlay {
            phase: PairingPhase::Starting,
            offer: None,
            request: None,
        }));
        self.pending_action = Some(PendingAction::Pair { remote: false });
    }

    fn request_pairing_command(&mut self, command: device_ops::PairingCommand) {
        self.pending_pairing_command = Some(command);
    }

    fn cancel_pairing(&mut self, navigation: DeferredNavigation) {
        if self.pairing_overlay().is_none() {
            return;
        }
        self.deferred_navigation = Some(navigation);
        let can_cancel = self.pairing_overlay().is_some_and(|pairing| {
            !matches!(
                pairing.phase,
                PairingPhase::Approving | PairingPhase::Denying
            )
        });
        self.cancel_requested |= can_cancel;
        self.overlay = None;
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
            if self.pairing_overlay().is_some() {
                self.cancel_pairing(DeferredNavigation::Quit);
                return;
            }
            self.deferred_navigation = Some(DeferredNavigation::Quit);
            self.overlay = None;
        } else {
            self.should_quit = true;
        }
    }

    fn request_back(&mut self) {
        if self.mutation_in_flight {
            if self.pairing_overlay().is_some() {
                self.cancel_pairing(DeferredNavigation::Back);
                return;
            }
            if self.deferred_navigation != Some(DeferredNavigation::Quit) {
                self.deferred_navigation = Some(DeferredNavigation::Back);
            }
            self.overlay = None;
        } else {
            self.go_back_or_quit();
        }
    }

    fn finish_deferred_navigation(&mut self) {
        match self.deferred_navigation.take() {
            Some(DeferredNavigation::Back) => {
                self.overlay = None;
                self.go_back_or_quit();
            }
            Some(DeferredNavigation::Quit) => {
                self.overlay = None;
                self.should_quit = true;
            }
            None => {}
        }
    }

    #[allow(clippy::too_many_lines)]
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
            if self.mutation_in_flight {
                if self.pairing_overlay().is_some() {
                    match key.code {
                        KeyCode::Char('a' | 'A') => {
                            if let Some(request) = self
                                .pairing_overlay()
                                .and_then(|pairing| pairing.request.as_ref())
                            {
                                self.request_pairing_command(device_ops::PairingCommand::Approve(
                                    request.id,
                                ));
                                if let Some(pairing) = self.pairing_overlay_mut() {
                                    pairing.phase = PairingPhase::Approving;
                                }
                            }
                        }
                        KeyCode::Char('d' | 'D') => {
                            if let Some(request) = self
                                .pairing_overlay()
                                .and_then(|pairing| pairing.request.as_ref())
                            {
                                self.request_pairing_command(device_ops::PairingCommand::Deny(
                                    request.id,
                                ));
                                if let Some(pairing) = self.pairing_overlay_mut() {
                                    pairing.phase = PairingPhase::Denying;
                                }
                            }
                        }
                        KeyCode::Esc | KeyCode::Char('q' | 'Q') => self.request_back(),
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q' | 'Q') => self.request_back(),
                        _ => {}
                    }
                }
            } else if event::is_quit(event) && self.overlay.take().is_none() {
                self.go_back_or_quit();
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
            KeyCode::Char('p' | 'P') if self.route == Route::Devices => self.begin_pairing(),
            KeyCode::Char('a' | 'A') if self.route == Route::Devices => {
                if let Some(request) = self.selected_pending_request() {
                    self.pending_action = Some(PendingAction::Approve(request.id));
                }
            }
            KeyCode::Char('d' | 'D') if self.route == Route::Devices => {
                if let Some(request) = self.selected_pending_request() {
                    self.pending_action = Some(PendingAction::Deny(request.id));
                }
            }
            KeyCode::Char('r' | 'R') if self.route == Route::Devices => {
                if let Some(device) = self.selected_device().cloned() {
                    self.overlay = Some(Overlay::RevokeDevice(RevokeDeviceOverlay {
                        device_id: device.id,
                        device_name: device.name,
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
            Route::Devices => self.device_rows().len().saturating_sub(1),
            Route::Settings | Route::Status => 0,
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
        match self.route {
            Route::Workspaces => {
                self.selected_workspace_id = self.workspaces.get(self.selected).map(|item| item.id);
            }
            Route::Devices => {
                self.selected_request_id = None;
                self.selected_device_id = None;
                match self.device_rows().get(self.selected) {
                    Some(DeviceRowId::Pending(id)) => self.selected_request_id = Some(*id),
                    Some(DeviceRowId::Paired(id)) => self.selected_device_id = Some(id.clone()),
                    None => {}
                }
            }
            _ => {}
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
            Route::Devices => {
                if self.selected_pending_request().is_some() {
                    self.set_toast(
                        "Use A to approve or D to deny the selected request",
                        ToastTone::Info,
                    );
                } else if self.selected_device().is_some() {
                    self.set_toast("Use R to revoke the selected device", ToastTone::Info);
                } else {
                    self.begin_pairing();
                }
            }
            Route::Settings | Route::Status => {
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
        self.selected_request_id = None;
        self.selected_device_id = None;
        if route == Route::Devices {
            self.device_refresh_requested = true;
        }
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
            Some(Overlay::RevokeDevice(overlay)) => match code {
                KeyCode::Up | KeyCode::Left => {
                    overlay.selected = overlay.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Right => overlay.selected = (overlay.selected + 1).min(1),
                KeyCode::Home => overlay.selected = 0,
                KeyCode::End => overlay.selected = 1,
                KeyCode::Enter if overlay.selected == 0 => {
                    action = Some(PendingAction::RevokeDevice(overlay.device_id.clone()));
                }
                KeyCode::Esc | KeyCode::Char('q' | 'Q') | KeyCode::Enter => self.overlay = None,
                _ => {}
            },
            Some(Overlay::Pair(pairing)) => {
                if matches!(
                    pairing.phase,
                    PairingPhase::Success { .. }
                        | PairingPhase::Denied
                        | PairingPhase::Cancelled
                        | PairingPhase::Expired
                        | PairingPhase::TimedOut
                        | PairingPhase::Error(_)
                ) && matches!(
                    code,
                    KeyCode::Esc | KeyCode::Char('q' | 'Q') | KeyCode::Enter
                ) {
                    self.overlay = None;
                }
            }
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
            WorkerResult::Devices {
                devices,
                pending,
                pending_error,
            } => {
                self.apply_devices(devices, pending);
                self.device_refresh_requested = false;
                overview.devices = self.paired_devices.len();
                if pending_error && self.route == Route::Devices {
                    self.set_toast("Pending pairing requests are unavailable", ToastTone::Info);
                }
            }
            WorkerResult::DeviceOperationFailed { action } => {
                self.device_refresh_requested = action != "refresh";
                if action != "refresh" {
                    self.overlay = None;
                }
                self.set_toast(
                    match action {
                        "approve" => "Could not approve pairing request",
                        "deny" => "Could not deny pairing request",
                        "revoke" => "Could not revoke device",
                        "refresh" => "Could not load devices",
                        _ => "Device operation failed",
                    },
                    ToastTone::Error,
                );
            }
            WorkerResult::DeviceOperationSucceeded {
                action,
                devices,
                pending,
                pending_error,
            } => {
                self.apply_devices(devices, pending);
                self.device_refresh_requested = false;
                self.overlay = None;
                overview.devices = self.paired_devices.len();
                let message = match action {
                    "approve" => "Pairing request approved",
                    "deny" => "Pairing request denied",
                    "revoke" => "Device revoked",
                    _ => "Device operation complete",
                };
                self.set_toast(message, ToastTone::Success);
                if pending_error && self.route == Route::Devices {
                    self.set_toast("Pending pairing requests are unavailable", ToastTone::Info);
                }
            }
            WorkerResult::Pairing(outcome) => {
                self.device_refresh_requested = true;
                match outcome {
                    device_ops::PairingOutcome::Success { device_name } => {
                        self.overlay = Some(Overlay::Pair(PairingOverlay {
                            phase: PairingPhase::Success { device_name },
                            offer: None,
                            request: None,
                        }));
                        self.set_toast("Device paired", ToastTone::Success);
                    }
                    device_ops::PairingOutcome::Denied => {
                        if let Some(pairing) = self.pairing_overlay_mut() {
                            pairing.phase = PairingPhase::Denied;
                            pairing.offer = None;
                            pairing.request = None;
                        }
                        self.set_toast("Pairing denied", ToastTone::Info);
                    }
                    device_ops::PairingOutcome::Cancelled => {
                        if let Some(pairing) = self.pairing_overlay_mut() {
                            pairing.phase = PairingPhase::Cancelled;
                            pairing.offer = None;
                            pairing.request = None;
                        }
                    }
                    device_ops::PairingOutcome::TimedOut => {
                        if let Some(pairing) = self.pairing_overlay_mut() {
                            pairing.phase = PairingPhase::TimedOut;
                            pairing.offer = None;
                            pairing.request = None;
                        }
                        self.set_toast("Pairing timed out", ToastTone::Error);
                    }
                    device_ops::PairingOutcome::Expired => {
                        if let Some(pairing) = self.pairing_overlay_mut() {
                            pairing.phase = PairingPhase::Expired;
                            pairing.offer = None;
                            pairing.request = None;
                        }
                        self.set_toast("Pairing offer expired", ToastTone::Error);
                    }
                    device_ops::PairingOutcome::Error(error) => {
                        if let Some(pairing) = self.pairing_overlay_mut() {
                            pairing.phase = PairingPhase::Error(error);
                            pairing.offer = None;
                            pairing.request = None;
                        }
                        self.set_toast(error.message(), ToastTone::Error);
                    }
                }
            }
        }
        self.finish_deferred_navigation();
    }

    fn apply_worker_event(
        &mut self,
        event: WorkerEvent,
        store: &ConfigStore,
        overview: &mut HostOverview,
    ) {
        match event {
            WorkerEvent::Result(result) => self.apply_worker_result(result, store, overview),
            WorkerEvent::PairingProgress(progress) => match progress {
                device_ops::PairingProgress::Waiting { offer } => {
                    if let Some(pairing) = self.pairing_overlay_mut() {
                        pairing.phase = PairingPhase::Waiting {
                            remote: offer.remote,
                        };
                        pairing.offer = Some(offer);
                    }
                }
                device_ops::PairingProgress::OfferReady(offer) => {
                    if let Some(pairing) = self.pairing_overlay_mut() {
                        pairing.phase = PairingPhase::Waiting { remote: true };
                        pairing.offer = Some(offer);
                    }
                }
                device_ops::PairingProgress::Request(request) => {
                    let request = PendingRequestItem::from(request);
                    if !self
                        .pending_requests
                        .iter()
                        .any(|candidate| candidate.id == request.id)
                    {
                        self.pending_requests.push(request.clone());
                    }
                    self.selected_request_id = Some(request.id);
                    self.selected_device_id = None;
                    if let Some(pairing) = self.pairing_overlay_mut() {
                        pairing.phase = PairingPhase::Request(request.clone());
                        pairing.offer = None;
                        pairing.request = Some(request);
                    }
                }
                device_ops::PairingProgress::Approving => {
                    if let Some(pairing) = self.pairing_overlay_mut() {
                        pairing.phase = PairingPhase::Approving;
                    }
                }
                device_ops::PairingProgress::Denying => {
                    if let Some(pairing) = self.pairing_overlay_mut() {
                        pairing.phase = PairingPhase::Denying;
                    }
                }
            },
        }
    }
}

#[allow(clippy::too_many_lines)]
fn spawn_worker(store: &ConfigStore, action: PendingAction) -> Worker {
    let (sender, receiver) = mpsc::channel();
    let store = store.clone();
    let mutation = matches!(
        &action,
        PendingAction::Add(_)
            | PendingAction::Remove(_)
            | PendingAction::Approve(_)
            | PendingAction::Deny(_)
            | PendingAction::RevokeDevice(_)
            | PendingAction::Pair { .. }
    );
    let (cancel, pairing_commands, pairing_command_rx) =
        if matches!(&action, PendingAction::Pair { .. }) {
            let cancel = Arc::new(AtomicBool::new(false));
            let (command_sender, command_receiver) = mpsc::channel();
            (Some(cancel), Some(command_sender), Some(command_receiver))
        } else {
            (None, None, None)
        };
    let worker_cancel = cancel.clone();
    let handle = std::thread::spawn(move || {
        let result = match action {
            PendingAction::Add(path) => {
                let path = commands::shared::expand_home(path);
                match commands::workspace::authorize_workspace(&store, &path, None) {
                    Ok(workspace) => WorkerEvent::Result(WorkerResult::Added {
                        service_error: commands::shared::refresh_running_service(&store)
                            .err()
                            .map(|error| error.to_string()),
                        workspace,
                    }),
                    Err(error) => WorkerEvent::Result(WorkerResult::AddFailed(error.to_string())),
                }
            }
            PendingAction::Remove(id) => match commands::workspace::revoke_workspace(&store, id) {
                Ok(workspace) => WorkerEvent::Result(WorkerResult::Removed {
                    service_error: commands::shared::refresh_running_service(&store)
                        .err()
                        .map(|error| error.to_string()),
                    workspace,
                }),
                Err(error) => WorkerEvent::Result(WorkerResult::RemoveFailed(error.to_string())),
            },
            PendingAction::OpenSessions(id) => match load_sessions(&store, id) {
                Ok((workspace, sessions)) => WorkerEvent::Result(WorkerResult::Sessions {
                    workspace,
                    sessions,
                }),
                Err(error) => WorkerEvent::Result(WorkerResult::SessionsFailed(error.to_string())),
            },
            PendingAction::RefreshDevices => load_devices_event(&store),
            PendingAction::Approve(id) => device_operation_event(&store, id, true),
            PendingAction::Deny(id) => device_operation_event(&store, id, false),
            PendingAction::RevokeDevice(id) => revoke_device_event(&store, &id),
            PendingAction::Pair { remote } => {
                let Some(cancel) = worker_cancel else {
                    return_sender(
                        &sender,
                        WorkerEvent::Result(WorkerResult::Pairing(
                            device_ops::PairingOutcome::Error(
                                device_ops::PairingFailure::ServiceUnavailable,
                            ),
                        )),
                    );
                    return;
                };
                let Some(command_rx) = pairing_command_rx else {
                    return_sender(
                        &sender,
                        WorkerEvent::Result(WorkerResult::Pairing(
                            device_ops::PairingOutcome::Error(
                                device_ops::PairingFailure::ServiceUnavailable,
                            ),
                        )),
                    );
                    return;
                };
                let mut session = match device_ops::PairingSession::start(&store, remote) {
                    Ok(session) => session,
                    Err(error) => {
                        return_sender(
                            &sender,
                            WorkerEvent::Result(WorkerResult::Pairing(
                                device_ops::PairingOutcome::Error(error),
                            )),
                        );
                        return;
                    }
                };
                return_sender(
                    &sender,
                    WorkerEvent::PairingProgress(device_ops::PairingProgress::Waiting {
                        offer: device_ops::PairingOffer {
                            remote,
                            qr_payload: None,
                            join_code: None,
                            expires_at: None,
                        },
                    }),
                );
                loop {
                    if let Ok(command) = command_rx.try_recv() {
                        match command {
                            device_ops::PairingCommand::Approve(id) => {
                                return_sender(
                                    &sender,
                                    WorkerEvent::PairingProgress(
                                        device_ops::PairingProgress::Approving,
                                    ),
                                );
                                let device_name = match session.approve(id) {
                                    Ok(device_name) => device_name,
                                    Err(error) => {
                                        session.cancel();
                                        return_sender(
                                            &sender,
                                            WorkerEvent::Result(WorkerResult::Pairing(
                                                device_ops::PairingOutcome::Error(error),
                                            )),
                                        );
                                        return;
                                    }
                                };
                                return_sender(
                                    &sender,
                                    WorkerEvent::Result(WorkerResult::Pairing(
                                        device_ops::PairingOutcome::Success { device_name },
                                    )),
                                );
                                return;
                            }
                            device_ops::PairingCommand::Deny(id) => {
                                return_sender(
                                    &sender,
                                    WorkerEvent::PairingProgress(
                                        device_ops::PairingProgress::Denying,
                                    ),
                                );
                                let outcome = match session.deny(id) {
                                    Ok(()) => {
                                        // A denied remote request has no
                                        // reason to keep its temporary relay
                                        // channel alive until TTL expiry.
                                        session.cancel();
                                        device_ops::PairingOutcome::Denied
                                    }
                                    Err(error) => {
                                        session.cancel();
                                        device_ops::PairingOutcome::Error(error)
                                    }
                                };
                                return_sender(
                                    &sender,
                                    WorkerEvent::Result(WorkerResult::Pairing(outcome)),
                                );
                                return;
                            }
                        }
                    }
                    match session.poll(&cancel) {
                        Ok(Some(device_ops::PairingEvent::Progress(progress))) => {
                            return_sender(&sender, WorkerEvent::PairingProgress(progress));
                        }
                        Ok(Some(device_ops::PairingEvent::Outcome(outcome))) => {
                            if matches!(outcome, device_ops::PairingOutcome::Error(_)) {
                                session.cancel();
                            }
                            return_sender(
                                &sender,
                                WorkerEvent::Result(WorkerResult::Pairing(outcome)),
                            );
                            return;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                        Err(error) => {
                            session.cancel();
                            return_sender(
                                &sender,
                                WorkerEvent::Result(WorkerResult::Pairing(
                                    device_ops::PairingOutcome::Error(error),
                                )),
                            );
                            return;
                        }
                    }
                }
            }
        };
        return_sender(&sender, result);
    });
    Worker {
        receiver,
        handle: Some(handle),
        mutation,
        cancel,
        pairing_commands,
    }
}

fn return_sender(sender: &Sender<WorkerEvent>, event: WorkerEvent) {
    let _ = sender.send(event);
}

fn load_devices_event(store: &ConfigStore) -> WorkerEvent {
    let devices = match device_ops::list_devices(store)
        .or_else(|_| commands::shared::load_or_ephemeral_config(store).map(|config| config.devices))
    {
        Ok(devices) => devices
            .into_iter()
            .map(|device| DeviceItem {
                id: device.id,
                name: device.name,
                paired_at: device.paired_at.to_rfc3339(),
            })
            .collect(),
        Err(_) => {
            return WorkerEvent::Result(WorkerResult::DeviceOperationFailed { action: "refresh" });
        }
    };
    let (pending, pending_error) = match device_ops::list_pending(store) {
        Ok(pending) => (pending.into_iter().map(Into::into).collect(), false),
        Err(_) => (Vec::new(), true),
    };
    WorkerEvent::Result(WorkerResult::Devices {
        devices,
        pending,
        pending_error,
    })
}

fn device_operation_event(store: &ConfigStore, id: Uuid, approve: bool) -> WorkerEvent {
    let result = if approve {
        device_ops::approve(store, id)
    } else {
        device_ops::deny(store, id)
    };
    match result {
        Ok(_) => operation_success_event(
            load_devices_event(store),
            if approve { "approve" } else { "deny" },
        ),
        Err(_) => WorkerEvent::Result(WorkerResult::DeviceOperationFailed {
            action: if approve { "approve" } else { "deny" },
        }),
    }
}

fn revoke_device_event(store: &ConfigStore, id: &str) -> WorkerEvent {
    match device_ops::revoke(store, id) {
        Ok(_) => operation_success_event(load_devices_event(store), "revoke"),
        Err(_) => WorkerEvent::Result(WorkerResult::DeviceOperationFailed { action: "revoke" }),
    }
}

fn operation_success_event(event: WorkerEvent, action: &'static str) -> WorkerEvent {
    match event {
        WorkerEvent::Result(WorkerResult::Devices {
            devices,
            pending,
            pending_error,
        }) => WorkerEvent::Result(WorkerResult::DeviceOperationSucceeded {
            action,
            devices,
            pending,
            pending_error,
        }),
        event => event,
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
            && app.route == Route::Devices
            && app.device_refresh_requested
            && app.pending_action.is_none()
        {
            app.pending_action = Some(PendingAction::RefreshDevices);
        }
        if worker.is_none()
            && let Some(action) = app.pending_action.take()
        {
            let action = match action {
                PendingAction::Pair { remote: false } => PendingAction::Pair {
                    remote: store
                        .load()
                        .ok()
                        .is_some_and(|config| config.preferences.active_relay_url().is_some()),
                },
                action => action,
            };
            app.busy = Some(action.label().to_owned());
            let active = spawn_worker(store, action);
            app.mutation_in_flight = active.mutation;
            worker = Some(active);
        }
        if app.cancel_requested {
            if let Some(active) = worker.as_ref() {
                active.cancel();
            }
            app.cancel_requested = false;
        }
        if let Some(command) = app.pending_pairing_command.take()
            && let Some(active) = worker.as_ref()
        {
            active.send_pairing_command(command);
        }
        if let Some(result) = worker.as_ref().map(|active| active.receiver.try_recv()) {
            match result {
                Ok(WorkerEvent::PairingProgress(progress)) => {
                    app.apply_worker_event(
                        WorkerEvent::PairingProgress(progress),
                        store,
                        &mut overview,
                    );
                }
                Ok(WorkerEvent::Result(result)) => {
                    let active = worker.take().expect("worker exists while receiving result");
                    let worker_panicked = active.join();
                    app.apply_worker_event(WorkerEvent::Result(result), store, &mut overview);
                    if worker_panicked && !app.should_quit {
                        app.set_toast("Operation stopped unexpectedly", ToastTone::Error);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    let active = worker.take().expect("worker exists while disconnected");
                    let _ = active.join();
                    app.busy = None;
                    app.mutation_in_flight = false;
                    app.finish_deferred_navigation();
                    if !app.should_quit {
                        app.set_toast("Operation stopped unexpectedly", ToastTone::Error);
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
        Route::Devices => render_devices(frame, body, app, overview),
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
    // Each session uses two lines so title and metadata remain readable at
    // the supported 48-column minimum instead of clipping the right side.
    let capacity = (usize::from(list_area.height) / 2).max(1);
    let (start, end) = visible_range(app.selected, app.sessions.len(), capacity);
    let items = app.sessions[start..end]
        .iter()
        .map(|session| {
            let title = session.title.as_deref().unwrap_or("Untitled session");
            let title = commands::shared::terminal_label(title);
            let title = truncate_end(
                &title,
                usize::from(list_area.width).saturating_sub(2).max(1),
            );
            let count = format!(
                "{} message{}",
                session.message_count,
                if session.message_count == 1 { "" } else { "s" }
            );
            let metadata = format!(
                "    {} · {count}",
                compact_session_modified_at(&session.modified_at)
            );
            ListItem::new(vec![
                Line::from(Span::styled(title, Style::default().fg(Color::White))),
                Line::from(Span::styled(metadata, Style::default().fg(Color::DarkGray))),
            ])
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

fn render_devices(frame: &mut Frame<'_>, area: Rect, app: &App, _overview: &HostOverview) {
    let [header, list_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .areas(area);
    render_workspace_header(
        frame,
        header,
        "Devices",
        &format!("{} paired", app.paired_devices.len()),
    );

    if app.paired_devices.is_empty() && app.pending_requests.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No paired devices or pending requests.",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from("Press P to pair a device."),
            ])
            .wrap(Wrap { trim: true }),
            list_area,
        );
        return;
    }

    let selected_row = app.selected_device_row();
    let mut items = Vec::new();
    let mut row_indices = Vec::new();
    if !app.pending_requests.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "Pending",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ))));
        for request in &app.pending_requests {
            row_indices.push((DeviceRowId::Pending(request.id), items.len()));
            let name_width = usize::from(list_area.width).saturating_sub(28).max(10);
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    truncate_end(
                        &commands::shared::terminal_label(&request.device_name),
                        name_width,
                    ),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!(
                        "  code {}  waiting approval",
                        commands::shared::format_confirmation_code(&request.confirmation_code)
                    ),
                    Style::default().fg(Color::Yellow),
                ),
            ])));
        }
    }
    if !app.paired_devices.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "Paired",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ))));
        for device in &app.paired_devices {
            row_indices.push((DeviceRowId::Paired(device.id.clone()), items.len()));
            let name_width = usize::from(list_area.width).saturating_sub(18).max(10);
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    truncate_end(&commands::shared::terminal_label(&device.name), name_width),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("  paired {}", paired_date(&device.paired_at)),
                    Style::default().fg(Color::DarkGray),
                ),
            ])));
        }
    }

    let selected_item = selected_row
        .and_then(|row| {
            row_indices
                .iter()
                .find(|(candidate, _)| *candidate == row)
                .map(|(_, index)| *index)
        })
        .unwrap_or(0);
    let capacity = usize::from(list_area.height).max(1);
    let (start, end) = visible_range(selected_item, items.len(), capacity);
    let mut state = ListState::default();
    state.select(Some(selected_item.saturating_sub(start)));
    frame.render_stateful_widget(
        List::new(items[start..end].to_vec())
            .highlight_symbol("❯ ")
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        list_area,
        &mut state,
    );
}

fn paired_date(value: &str) -> String {
    value
        .get(0..10)
        .map_or_else(|| value.to_owned(), ToOwned::to_owned)
}

fn compact_session_modified_at(value: &str) -> String {
    let Some((date, time)) = value.split_once('T') else {
        return value.to_owned();
    };
    let time = time.get(..5).unwrap_or(time);
    format!("{date} {time}")
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
        Route::Devices => unreachable!("Devices has a dedicated persistent renderer"),
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
        Route::Devices | Route::Home | Route::Workspaces | Route::WorkspaceSessions => {
            unreachable!("persistent routes are rendered separately")
        }
    };
    frame.render_widget(Paragraph::new(content).wrap(Wrap { trim: true }), area);
}

fn render_overlay(frame: &mut Frame<'_>, area: Rect, app: &App, overlay: &Overlay) {
    match overlay {
        Overlay::Help => render_help(frame, area, app.route, app.workspaces.len()),
        Overlay::Add(add) => render_add_overlay(frame, area, add),
        Overlay::Remove(remove) => render_remove_overlay(frame, area, remove),
        Overlay::RevokeDevice(revoke) => render_revoke_device_overlay(frame, area, revoke),
        Overlay::Pair(pairing) => render_pairing_overlay(frame, area, pairing),
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
    // Reserve cells for the optional prefix/suffix ellipses and the visual
    // cursor so the decorated span still fits in the modal input row.
    let text_budget = max_chars.saturating_sub(3).max(1);
    let mut start = cursor.saturating_sub(text_budget / 2);
    let mut end = (start + text_budget).min(chars.len());
    if end.saturating_sub(start) < text_budget {
        start = end.saturating_sub(text_budget);
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

fn render_revoke_device_overlay(frame: &mut Frame<'_>, area: Rect, overlay: &RevokeDeviceOverlay) {
    let Some(popup) = centered_popup(area, 68, 10) else {
        return;
    };
    frame.render_widget(Clear, popup);
    let content = vec![
        Line::from(Span::styled(
            format!(
                "Revoke {}?",
                commands::shared::terminal_label(&overlay.device_name)
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "This closes the device's active connections.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(if overlay.selected == 0 {
            "❯ Revoke device"
        } else {
            "  Revoke device"
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
                    .title(" Confirm revocation "),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn pairing_qr_lines(offer: &device_ops::PairingOffer) -> Option<Vec<String>> {
    let payload = offer.qr_payload.as_ref()?;
    let code = qrcode::QrCode::new(payload.expose().as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(false)
            .build()
            .lines()
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn pairing_qr_fits(area: Rect, lines: &[String]) -> bool {
    let popup_width = 76.min(area.width.saturating_sub(2));
    let inner_width = usize::from(popup_width.saturating_sub(2));
    let qr_width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let popup_height = u16::try_from(lines.len().saturating_add(6)).unwrap_or(u16::MAX);
    qr_width <= inner_width && popup_height <= area.height.saturating_sub(2)
}

#[allow(clippy::too_many_lines)]
fn render_pairing_overlay(frame: &mut Frame<'_>, area: Rect, overlay: &PairingOverlay) {
    let qr_lines = match &overlay.phase {
        PairingPhase::Waiting { remote: true } => overlay.offer.as_ref().and_then(pairing_qr_lines),
        _ => None,
    };
    let show_qr = qr_lines
        .as_deref()
        .is_some_and(|lines| pairing_qr_fits(area, lines));
    let height = match &overlay.phase {
        PairingPhase::Waiting { remote: true } if show_qr => qr_lines
            .as_ref()
            .and_then(|lines| u16::try_from(lines.len().saturating_add(6)).ok())
            .unwrap_or(18),
        PairingPhase::Waiting { remote: true } if overlay.offer.is_some() => 10,
        PairingPhase::Request(_) => 12,
        _ => 9,
    };
    let Some(popup) = centered_popup(area, 76, height) else {
        return;
    };
    frame.render_widget(Clear, popup);
    let mut lines = Vec::new();
    match &overlay.phase {
        PairingPhase::Starting => lines.push(Line::from("Preparing secure pairing…")),
        PairingPhase::Waiting { remote } => {
            lines.push(Line::from(Span::styled(
                if *remote {
                    "Scan the QR code or enter the join code on your device."
                } else {
                    "Waiting for your device on the local network…"
                },
                Style::default().fg(Color::Cyan),
            )));
            if let Some(offer) = &overlay.offer {
                if show_qr {
                    lines.extend(
                        qr_lines
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .cloned()
                            .map(Line::from),
                    );
                } else if offer.qr_payload.is_some() {
                    lines.push(Line::from(Span::styled(
                        "QR needs a larger terminal; use the join code.",
                        Style::default().fg(Color::Yellow),
                    )));
                }
                if let Some(join_code) = &offer.join_code {
                    lines.push(Line::from(vec![
                        Span::styled("Join code ", Style::default().fg(Color::DarkGray)),
                        Span::styled(join_code.expose(), Style::default().fg(Color::Yellow)),
                    ]));
                }
            }
        }
        PairingPhase::Request(request) => {
            lines.push(Line::from(Span::styled(
                format!(
                    "{} wants to pair",
                    commands::shared::terminal_label(&request.device_name)
                ),
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::from(format!(
                "Verify code {} on your device.",
                commands::shared::format_confirmation_code(&request.confirmation_code)
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("A Approve     D Deny     Esc Cancel"));
        }
        PairingPhase::Approving => lines.push(Line::from("Approving pairing…")),
        PairingPhase::Denying => lines.push(Line::from("Denying pairing…")),
        PairingPhase::Success { device_name } => lines.push(Line::from(Span::styled(
            format!("✓ {} paired", commands::shared::terminal_label(device_name)),
            Style::default().fg(Color::Green),
        ))),
        PairingPhase::Denied => lines.push(Line::from("Pairing denied.")),
        PairingPhase::Cancelled => lines.push(Line::from("Pairing cancelled.")),
        PairingPhase::Expired => lines.push(Line::from("Pairing offer expired.")),
        PairingPhase::TimedOut => lines.push(Line::from("Pairing timed out.")),
        PairingPhase::Error(error) => lines.push(Line::from(Span::styled(
            error.message(),
            Style::default().fg(Color::Red),
        ))),
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if matches!(
            &overlay.phase,
            PairingPhase::Success { .. }
                | PairingPhase::Denied
                | PairingPhase::Cancelled
                | PairingPhase::Expired
                | PairingPhase::TimedOut
                | PairingPhase::Error(_)
        ) {
            "Enter/Esc close"
        } else {
            "Esc cancel"
        },
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Pair device "),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn render_feedback(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines = Vec::new();
    if let Some(busy) = &app.busy {
        let busy = if app.deferred_navigation == Some(DeferredNavigation::Quit) {
            "Finishing operation…"
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
    let hints = if app.route == Route::Devices {
        device_footer_hints(
            area.width,
            !app.pending_requests.is_empty(),
            !app.paired_devices.is_empty(),
        )
    } else {
        footer_hints(area.width, app.route, app.workspaces.len())
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled(hints, Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn device_footer_hints(width: u16, has_pending: bool, has_paired: bool) -> &'static str {
    match (width < 72, has_pending, has_paired) {
        (_, false, false) => "P Pair   Esc Back   ? Help",
        (true, true, false) => "↑↓ Move  P Pair  A Approve  D Deny  Esc  ?",
        (true, false, true) => "↑↓ Move  P Pair  R Revoke  Esc  ?",
        (true, true, true) => "↑↓ Move P Pair A Approve D Deny R Revoke Esc ?",
        (false, true, false) => "↑↓/jk Navigate   P Pair   A Approve   D Deny   Esc Back   ? Help",
        (false, false, true) => "↑↓/jk Navigate   P Pair   R Revoke   Esc Back   ? Help",
        (false, true, true) => {
            "↑↓/jk Navigate   P Pair   A Approve   D Deny   R Revoke   Esc Back   ? Help"
        }
    }
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
    let device_shortcuts: &[&str] = &[
        "↑↓ or j/k   navigate devices and requests",
        "P           start pairing",
        "A / D       approve or deny request",
        "R           revoke paired device",
    ];
    let shortcuts = if route == Route::Devices {
        device_shortcuts
    } else {
        help_shortcuts(route, workspace_count)
    };
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
        AddOverlay, App, DeferredNavigation, DeviceItem, InteractionMode, MIN_HEIGHT, Overlay,
        PairingOverlay, PairingPhase, PendingRequestItem, Route, SessionItem, TerminalSize,
        TextInput, TtyState, WorkerResult, WorkspaceItem, footer_hints, help_shortcuts, input_line,
        interaction_mode, pairing_qr_fits, pairing_qr_lines, render, should_launch_tui,
        truncate_end, truncate_path, version_mismatch_message, visible_range,
    };
    use crate::app_ops::device as device_ops;
    use crate::app_ops::device::PairingSecret;
    use crate::home::{AccessOverview, PiOverview, PiSource, ServiceOverview};
    use crate::output::OutputFormat;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use pix_core::{ConfigStore, HostConfig, WorkspaceRegistry};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tempfile::tempdir;
    use uuid::Uuid;

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
        assert_eq!(footer_hints(80, Route::Settings, 1), "Esc/q Back   ? Help");
        assert!(footer_hints(80, Route::Workspaces, 1).contains("A Add"));
        assert!(!footer_hints(80, Route::Workspaces, 0).contains("Sessions"));
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
    fn mutation_q_requests_back_after_the_worker_result() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.history.push(Route::Home);
        app.busy = Some("Removing workspace…".to_owned());
        app.mutation_in_flight = true;

        app.handle_event(&key(KeyCode::Char('q')));

        assert_eq!(app.deferred_navigation, Some(DeferredNavigation::Back));
        assert!(!app.should_quit);
        assert!(app.overlay.is_none());

        let store = ConfigStore::new("/tmp/pix-tui-test-config.json");
        let mut overview = test_overview(0);
        app.apply_worker_result(
            WorkerResult::RemoveFailed("test failure".to_owned()),
            &store,
            &mut overview,
        );

        assert_eq!(app.route, Route::Home);
        assert!(!app.should_quit);
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

        assert_eq!(app.deferred_navigation, Some(DeferredNavigation::Quit));
        assert!(!app.should_quit);
    }

    #[test]
    fn mutation_escape_defers_back_until_the_worker_result() {
        let mut app = App::new();
        app.route = Route::Workspaces;
        app.history.push(Route::Home);
        app.selection_history.push(1);
        app.busy = Some("Removing workspace…".to_owned());
        app.mutation_in_flight = true;

        app.handle_event(&key(KeyCode::Esc));

        assert_eq!(app.deferred_navigation, Some(DeferredNavigation::Back));
        assert!(!app.should_quit);

        let store = ConfigStore::new("/tmp/pix-tui-test-config.json");
        let mut overview = test_overview(0);
        app.apply_worker_result(
            WorkerResult::RemoveFailed("test failure".to_owned()),
            &store,
            &mut overview,
        );

        assert_eq!(app.route, Route::Home);
        assert!(!app.should_quit);
        assert!(app.deferred_navigation.is_none());
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
        assert!(text.contains("2026-09-19 00:00"));
        assert!(text.contains("3 messages"));
        assert!(!text.contains("Pi sessions"));
        assert!(!text.contains('┌'));
    }

    #[test]
    fn narrow_sessions_keep_title_time_and_message_count_visible() {
        let mut app = App::new();
        app.route = Route::WorkspaceSessions;
        app.workspaces.push(WorkspaceItem {
            id: uuid::Uuid::new_v4(),
            name: "my-workspace".to_owned(),
            path: PathBuf::from("/tmp/my-workspace"),
        });
        app.active_workspace_id = app.workspaces.first().map(|workspace| workspace.id);
        app.sessions.push(SessionItem {
            id: "session-1".to_owned(),
            title: Some("Fix the menu".to_owned()),
            modified_at: "2026-09-19T00:00:00Z".to_owned(),
            message_count: 3,
        });
        let overview = test_overview(1);
        let mut terminal = Terminal::new(TestBackend::new(48, 19)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("narrow sessions frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Fix the menu"));
        assert!(text.contains("2026-09-19 00:00"));
        assert!(text.contains("3 messages"));
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

    #[test]
    fn devices_empty_state_only_advertises_pairing() {
        let mut app = App::new();
        app.route = Route::Devices;
        let overview = test_overview(0);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("devices frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("No paired devices or pending requests"));
        assert!(text.contains("P Pair"));
        assert!(!text.contains("Approve"));
        assert!(!text.contains('┌'));
    }

    #[test]
    fn devices_render_paired_pending_and_mixed_states() {
        let paired = DeviceItem {
            id: "device-1".to_owned(),
            name: "Zain's iPhone".to_owned(),
            paired_at: "2026-09-18T00:00:00Z".to_owned(),
        };
        let pending = PendingRequestItem {
            id: Uuid::new_v4(),
            device_name: "iPhone 17 Pro".to_owned(),
            confirmation_code: "482931".to_owned(),
            expires_at: 1_800_000_000,
        };
        let mut app = App::new();
        app.route = Route::Devices;
        app.paired_devices = vec![paired];
        app.pending_requests = vec![pending.clone()];
        app.select_index(0);
        let overview = test_overview(1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("mixed devices frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Pending"));
        assert!(text.contains("iPhone 17 Pro"));
        assert!(text.contains("482 931"));
        assert!(text.contains("Paired"));
        assert!(text.contains("Zain's iPhone"));
        assert!(text.contains("A Approve"));
        assert!(text.contains("D Deny"));
        assert!(text.contains("R Revoke"));
    }

    #[test]
    fn devices_render_paired_only_and_pending_only_states() {
        let mut app = App::new();
        app.route = Route::Devices;
        app.paired_devices.push(DeviceItem {
            id: "device-1".to_owned(),
            name: "Paired phone".to_owned(),
            paired_at: "2026-09-18T00:00:00Z".to_owned(),
        });
        app.select_index(0);
        let overview = test_overview(1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("paired frame");
        assert!(buffer_text(terminal.backend()).contains("Paired phone"));

        app.paired_devices.clear();
        app.pending_requests.push(PendingRequestItem {
            id: Uuid::new_v4(),
            device_name: "Pending phone".to_owned(),
            confirmation_code: "654321".to_owned(),
            expires_at: 0,
        });
        app.select_index(0);
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("pending frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Pending phone"));
        assert!(text.contains("654 321"));
    }

    #[test]
    fn device_selection_follows_stable_ids_when_lists_change() {
        let first = DeviceItem {
            id: "first".to_owned(),
            name: "First".to_owned(),
            paired_at: "2026-09-01T00:00:00Z".to_owned(),
        };
        let second = DeviceItem {
            id: "second".to_owned(),
            name: "Second".to_owned(),
            paired_at: "2026-09-02T00:00:00Z".to_owned(),
        };
        let request = PendingRequestItem {
            id: Uuid::new_v4(),
            device_name: "Pending".to_owned(),
            confirmation_code: "111222".to_owned(),
            expires_at: 0,
        };
        let request_id = request.id;
        let mut app = App::new();
        app.route = Route::Devices;
        app.paired_devices = vec![first, second];
        app.pending_requests = vec![request];
        app.select_index(2);
        assert_eq!(app.selected_device_id.as_deref(), Some("second"));
        app.apply_devices(
            vec![
                DeviceItem {
                    id: "second".to_owned(),
                    name: "Second".to_owned(),
                    paired_at: "2026-09-02T00:00:00Z".to_owned(),
                },
                DeviceItem {
                    id: "first".to_owned(),
                    name: "First".to_owned(),
                    paired_at: "2026-09-01T00:00:00Z".to_owned(),
                },
            ],
            vec![],
        );
        assert_eq!(app.selected_device_id.as_deref(), Some("second"));
        assert_eq!(app.selected_request_id, None);

        app.apply_devices(
            Vec::new(),
            vec![PendingRequestItem {
                id: request_id,
                device_name: "Pending".to_owned(),
                confirmation_code: "111222".to_owned(),
                expires_at: 0,
            }],
        );
        assert_eq!(app.selected_request_id, Some(request_id));
    }

    #[test]
    fn device_lists_scroll_and_keep_the_last_row_visible() {
        let mut app = App::new();
        app.route = Route::Devices;
        app.paired_devices = (0..24)
            .map(|index| DeviceItem {
                id: format!("device-{index}"),
                name: format!("Device {index}"),
                paired_at: "2026-09-01T00:00:00Z".to_owned(),
            })
            .collect();
        app.select_index(app.max_selection());
        let overview = test_overview(app.paired_devices.len());
        let mut terminal = Terminal::new(TestBackend::new(48, 19)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("scrolling devices frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Device 23"));
        assert!(!text.contains("Device 0"));
    }

    #[test]
    fn revoke_device_confirmation_defaults_to_cancel_and_keeps_id() {
        let mut app = App::new();
        app.route = Route::Devices;
        app.paired_devices.push(DeviceItem {
            id: "device-1".to_owned(),
            name: "iPhone".to_owned(),
            paired_at: "2026-09-01T00:00:00Z".to_owned(),
        });
        app.select_index(0);
        app.handle_event(&key(KeyCode::Char('r')));
        let Some(Overlay::RevokeDevice(overlay)) = &app.overlay else {
            panic!("expected device confirmation");
        };
        assert_eq!(overlay.device_id, "device-1");
        assert_eq!(overlay.selected, 1);
        app.handle_event(&key(KeyCode::Enter));
        assert!(app.overlay.is_none());
        assert!(app.pending_action.is_none());

        app.handle_event(&key(KeyCode::Char('r')));
        app.handle_event(&key(KeyCode::Up));
        app.handle_event(&key(KeyCode::Enter));
        assert!(
            matches!(app.pending_action, Some(super::PendingAction::RevokeDevice(id)) if id == "device-1")
        );
    }

    #[test]
    fn pairing_overlay_renders_waiting_request_success_and_error_without_borders_on_page() {
        let request = PendingRequestItem {
            id: Uuid::new_v4(),
            device_name: "iPhone".to_owned(),
            confirmation_code: "123456".to_owned(),
            expires_at: 0,
        };
        let mut app = App::new();
        app.route = Route::Devices;
        app.overlay = Some(Overlay::Pair(PairingOverlay {
            phase: PairingPhase::Request(request.clone()),
            offer: None,
            request: Some(request),
        }));
        let overview = test_overview(0);
        let mut terminal = Terminal::new(TestBackend::new(48, 19)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("pairing request frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("wants to pair"));
        assert!(text.contains("123 456"));
        assert!(text.contains("A Approve"));
        assert!(text.contains('┌'), "pairing is a real modal");

        app.overlay = Some(Overlay::Pair(PairingOverlay {
            phase: PairingPhase::Error(device_ops::PairingFailure::RelayUnavailable),
            offer: None,
            request: None,
        }));
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("pairing error frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("Remote pairing is unavailable"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn pairing_overlay_renders_terminal_outcomes() {
        let overview = test_overview(0);
        let phases = [
            (
                PairingPhase::Waiting { remote: false },
                "Waiting for your device",
            ),
            (
                PairingPhase::Success {
                    device_name: "iPhone".to_owned(),
                },
                "iPhone paired",
            ),
            (PairingPhase::Denied, "Pairing denied"),
            (PairingPhase::Cancelled, "Pairing cancelled"),
            (PairingPhase::Expired, "Pairing offer expired"),
            (PairingPhase::TimedOut, "Pairing timed out"),
            (
                PairingPhase::Error(device_ops::PairingFailure::ConnectionFailed),
                "Device connection failed",
            ),
        ];
        for (phase, expected) in phases {
            let mut app = App::new();
            app.route = Route::Devices;
            app.overlay = Some(Overlay::Pair(PairingOverlay {
                phase,
                offer: None,
                request: None,
            }));
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
            terminal
                .draw(|frame| render(frame, &app, &overview))
                .expect("pairing outcome frame");
            assert!(
                buffer_text(terminal.backend()).contains(expected),
                "{expected}"
            );
        }
    }

    #[test]
    fn pairing_qr_is_whole_or_replaced_with_join_code_guidance() {
        let offer = device_ops::PairingOffer {
            remote: true,
            qr_payload: Some(PairingSecret::from_test("pix://pair?secret=hidden")),
            join_code: Some(PairingSecret::from_test("ABCD-EFGH")),
            expires_at: Some(1_900_000_000),
        };
        let qr_lines = pairing_qr_lines(&offer).expect("QR renders");
        assert!(
            qr_lines.len() > 9,
            "test payload must exercise full QR output"
        );
        assert!(!pairing_qr_fits(
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 48,
                height: 19,
            },
            &qr_lines
        ));
        assert!(pairing_qr_fits(
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 60,
            },
            &qr_lines
        ));

        let mut app = App::new();
        app.route = Route::Devices;
        app.overlay = Some(Overlay::Pair(PairingOverlay {
            phase: PairingPhase::Waiting { remote: true },
            offer: Some(offer.clone()),
            request: None,
        }));
        let overview = test_overview(0);
        let mut narrow = Terminal::new(TestBackend::new(48, 19)).expect("terminal");
        narrow
            .draw(|frame| render(frame, &app, &overview))
            .expect("narrow pairing frame");
        let narrow_text = buffer_text(narrow.backend());
        assert!(narrow_text.contains("QR needs a larger terminal"));
        assert!(narrow_text.contains("ABCD-EFGH"));

        let mut wide = Terminal::new(TestBackend::new(120, 60)).expect("terminal");
        wide.draw(|frame| render(frame, &app, &overview))
            .expect("wide pairing frame");
        let wide_text = buffer_text(wide.backend());
        assert!(!wide_text.contains("QR needs a larger terminal"));
        assert!(wide_text.contains("ABCD-EFGH"));
    }

    #[test]
    fn pairing_secret_debug_is_redacted() {
        let offer = device_ops::PairingOffer {
            remote: true,
            qr_payload: Some(PairingSecret::from_test("pix://pair?secret=hidden")),
            join_code: Some(PairingSecret::from_test("ABCD-EFGH")),
            expires_at: Some(1),
        };
        let debug = format!("{offer:?}");
        assert!(!debug.contains("hidden"));
        assert!(!debug.contains("ABCD-EFGH"));
        assert!(debug.contains("redacted"));

        let request = device_ops::PendingPairing {
            id: Uuid::new_v4(),
            device_name: "iPhone".to_owned(),
            confirmation_code: "123456".to_owned(),
            expires_at: 1,
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("123456"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn device_mutation_defers_back_until_worker_result() {
        let mut app = App::new();
        app.route = Route::Devices;
        app.history.push(Route::Home);
        app.busy = Some("Revoking device…".to_owned());
        app.mutation_in_flight = true;
        app.handle_event(&key(KeyCode::Esc));
        assert_eq!(app.deferred_navigation, Some(DeferredNavigation::Back));
        assert!(!app.should_quit);
        let store = ConfigStore::new("/tmp/pix-tui-device-test-config.json");
        let mut overview = test_overview(0);
        app.apply_worker_result(
            WorkerResult::DeviceOperationFailed { action: "revoke" },
            &store,
            &mut overview,
        );
        assert_eq!(app.route, Route::Home);
        assert!(!app.should_quit);
    }

    #[test]
    fn pairing_ctrl_c_defers_quit_until_worker_completion() {
        let mut app = App::new();
        app.route = Route::Devices;
        app.busy = Some("Pairing…".to_owned());
        app.mutation_in_flight = true;
        app.overlay = Some(Overlay::Pair(PairingOverlay {
            phase: PairingPhase::Waiting { remote: false },
            offer: None,
            request: None,
        }));
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.deferred_navigation, Some(DeferredNavigation::Quit));
        assert!(app.cancel_requested);
        assert!(!app.should_quit);

        let store = ConfigStore::new("/tmp/pix-tui-pairing-test-config.json");
        let mut overview = test_overview(0);
        app.apply_worker_result(
            WorkerResult::Pairing(device_ops::PairingOutcome::Cancelled),
            &store,
            &mut overview,
        );
        assert!(app.should_quit);
    }

    #[test]
    fn narrow_devices_keep_required_actions_visible() {
        let mut app = App::new();
        app.route = Route::Devices;
        app.pending_requests.push(PendingRequestItem {
            id: Uuid::new_v4(),
            device_name: "Phone".to_owned(),
            confirmation_code: "123456".to_owned(),
            expires_at: 0,
        });
        app.paired_devices.push(DeviceItem {
            id: "device".to_owned(),
            name: "iPhone".to_owned(),
            paired_at: "2026-09-01T00:00:00Z".to_owned(),
        });
        app.select_index(0);
        let overview = test_overview(1);
        let mut terminal = Terminal::new(TestBackend::new(48, 19)).expect("terminal");
        terminal
            .draw(|frame| render(frame, &app, &overview))
            .expect("narrow devices frame");
        let text = buffer_text(terminal.backend());
        assert!(text.contains("A Approve"));
        assert!(text.contains("D Deny"));
        assert!(text.contains("R Revoke"));
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
