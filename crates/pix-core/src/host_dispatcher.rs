use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, Instant};

use pix_wire::{
    ClientEnvelope, ClientRequest, ErrorCode, HostSnapshot, HostSummary, PROTOCOL_MAJOR,
    RelayAccess, ServerEnvelope, ServerEvent, SessionState, SessionSummary as WireSessionSummary,
    WorkspaceAvailability, WorkspaceSummary,
};
use thiserror::Error;
use uuid::Uuid;

use crate::config::{ConfigError, HostConfig};
use crate::pi_bridge::{self, PiBridgeError};
use crate::pi_rpc::{
    ExtensionUiAnswer as PiExtensionUiAnswer, PiCommand, PiEvent, ThinkingLevel as PiThinkingLevel,
};
use crate::runtime_manager::{RuntimeManager, RuntimeManagerError};
use crate::session::{DiscoveredSession, PiSessionStore, SessionError, SessionMetadataIndex};
use crate::session_lock::SessionId;
use crate::workspace::{WorkspaceError, WorkspaceRegistry};

const WORKSPACE_AVAILABILITY_TTL: Duration = Duration::from_secs(10);
const MAX_SESSION_LIST: u32 = 200;

/// Shared, conversation-free host configuration visible to all connections.
pub struct HostState {
    config: RwLock<HostConfig>,
    catalog: Mutex<HostCatalog>,
}

#[derive(Default)]
struct HostCatalog {
    workspaces: HashMap<Uuid, CachedWorkspaceAvailability>,
    sessions: SessionMetadataIndex,
}

struct CachedWorkspaceAvailability {
    checked_at: Instant,
    availability: WorkspaceAvailability,
}

impl HostState {
    #[must_use]
    pub fn new(config: HostConfig) -> Self {
        Self {
            config: RwLock::new(config),
            catalog: Mutex::new(HostCatalog::default()),
        }
    }

    /// Replaces the in-memory view after a host-authorized configuration edit.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the replacement violates durable host
    /// configuration invariants.
    pub fn replace(&self, config: HostConfig) -> Result<(), ConfigError> {
        config.validate()?;
        *self
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .workspaces
            .clear();
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> HostConfig {
        self.config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Connection-scoped protocol dispatcher for one authenticated device.
///
/// The dispatcher owns no conversation history. It authorizes every workspace
/// lookup against [`HostState`] and delegates native session state to Pi.
pub struct HostProtocolDispatcher {
    host: Arc<HostState>,
    runtimes: Arc<RuntimeManager>,
    device_id: Option<String>,
    attached_sessions: HashSet<SessionId>,
    event_receivers: HashMap<SessionId, mpsc::Receiver<PiEvent>>,
}

impl HostProtocolDispatcher {
    #[must_use]
    pub fn new(host: Arc<HostState>, runtimes: Arc<RuntimeManager>) -> Self {
        Self {
            host,
            runtimes,
            device_id: None,
            attached_sessions: HashSet::new(),
            event_receivers: HashMap::new(),
        }
    }

    /// Identifies the authenticated device so per-device data, currently the
    /// relay channel inside `host.snapshot`, can be scoped to the requester.
    pub fn set_device(&mut self, device_id: impl Into<String>) {
        self.device_id = Some(device_id.into());
    }

    /// Executes one validated protocol request and returns all immediate
    /// responses. Accepted mutations return `request.ack`; subsequent snapshot
    /// failures are reported after that acknowledgement to prevent unsafe
    /// automatic retries.
    #[must_use]
    pub fn dispatch(&mut self, envelope: ClientEnvelope) -> Vec<ServerEnvelope> {
        self.prepare_dispatch(envelope)
            .into_iter()
            .map(|pending| self.resolve_response(pending))
            .collect()
    }

    /// Subscribes to live Pi events for a session attached by this connection.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError`] when the connection is not attached or the
    /// runtime has stopped.
    pub fn subscribe(&self, session_id: &str) -> Result<mpsc::Receiver<PiEvent>, DispatchError> {
        let session_id = parse_session_id(session_id)?;
        if !self.attached_sessions.contains(&session_id) {
            return Err(DispatchError::NotAttached(session_id));
        }
        Ok(self.runtimes.subscribe(session_id)?)
    }

    /// Converts one event from an attached Pi runtime to an unsolicited Pix
    /// envelope. Unknown informational Pi events return `None`.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError`] if the session is not attached or a known Pi
    /// event violates the verified compatibility shape.
    pub fn map_pi_event(
        &self,
        session_id: &str,
        event: PiEvent,
    ) -> Result<Option<ServerEnvelope>, DispatchError> {
        let session_id = parse_session_id(session_id)?;
        if !self.attached_sessions.contains(&session_id) {
            return Err(DispatchError::NotAttached(session_id));
        }
        let mapped = pi_bridge::event(session_id, event)?;
        if let Some(event) = &mapped {
            match event {
                ServerEvent::SessionState {
                    state: SessionState::Idle,
                    ..
                } => self.runtimes.mark_completed(session_id, true),
                ServerEvent::SessionState {
                    state: SessionState::Running,
                    ..
                }
                | ServerEvent::Compaction { .. } => {
                    self.runtimes.mark_completed(session_id, false);
                }
                _ => {}
            }
        }
        Ok(mapped.map(unsolicited))
    }

    /// Drains all currently available Pi events for attached sessions.
    ///
    /// Event mapping happens on the connection thread so the authenticated
    /// Noise transport keeps one ordered writer. Malformed compatibility
    /// events become payload-free public errors rather than tearing down an
    /// otherwise healthy phone connection.
    pub fn drain_events(&mut self) -> Vec<ServerEnvelope> {
        let mut raw_events = Vec::new();
        let mut disconnected = Vec::new();
        for (&session_id, receiver) in &self.event_receivers {
            loop {
                match receiver.try_recv() {
                    Ok(event) => raw_events.push((session_id, event)),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected.push(session_id);
                        break;
                    }
                }
            }
        }
        for session_id in disconnected {
            self.event_receivers.remove(&session_id);
        }

        raw_events
            .into_iter()
            .filter_map(|(session_id, event)| {
                match self.map_pi_event(&session_id.to_string(), event) {
                    Ok(mapped) => mapped,
                    Err(error) => Some(unsolicited(error.public_event())),
                }
            })
            .collect()
    }

    /// Detaches this connection from every session without stopping Pi work.
    pub fn disconnect(&mut self) {
        for session_id in self.attached_sessions.drain() {
            let _ = self.runtimes.detach(session_id);
        }
        self.event_receivers.clear();
    }

    pub(crate) fn prepare_dispatch(&mut self, envelope: ClientEnvelope) -> Vec<PendingResponse> {
        let request_id = envelope.request_id;
        match self.handle(envelope.request) {
            Ok(events) => events
                .into_iter()
                .map(|event| PendingResponse { request_id, event })
                .collect(),
            Err(error) => vec![PendingResponse {
                request_id,
                event: PendingEvent::Ready(error.public_event()),
            }],
        }
    }

    pub(crate) fn resolve_response(&self, pending: PendingResponse) -> ServerEnvelope {
        let event = match pending.event {
            PendingEvent::Ready(event) => event,
            PendingEvent::SessionSnapshot(session_id) => self
                .session_snapshot_event(session_id)
                .unwrap_or_else(|error| error.public_event()),
        };
        response(pending.request_id, event)
    }

    #[allow(clippy::too_many_lines)]
    fn handle(&mut self, request: ClientRequest) -> Result<Vec<PendingEvent>, DispatchError> {
        match request {
            ClientRequest::HostSnapshot => Ok(vec![ready(ServerEvent::HostSnapshot {
                snapshot: self.host_snapshot(),
            })]),
            ClientRequest::HostDefaults => {
                let defaults = self.runtimes.pi_model_defaults();
                crate::diagnostics::record(
                    "host.defaults",
                    &[
                        ("model_present", u64::from(defaults.model.is_some())),
                        (
                            "model_count",
                            u64::try_from(defaults.models.len()).unwrap_or(u64::MAX),
                        ),
                        (
                            "thinking_present",
                            u64::from(defaults.thinking_level.is_some()),
                        ),
                    ],
                );
                Ok(vec![ready(ServerEvent::HostDefaults { defaults })])
            }
            ClientRequest::WorkspaceList => Ok(vec![ready(ServerEvent::WorkspaceList {
                workspaces: self.workspace_summaries(),
            })]),
            ClientRequest::SessionList {
                workspace_id,
                limit,
            } => Ok(vec![ready(self.session_list(workspace_id, limit)?)]),
            ClientRequest::SessionCreate { workspace_id, name } => {
                let name = name.and_then(|value| {
                    let trimmed = value.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_owned())
                });
                self.create_session(workspace_id, name)
            }
            ClientRequest::SessionAttach { session_id } => {
                Ok(vec![ready(self.attach_session(&session_id)?)])
            }
            ClientRequest::SessionRename { session_id, name } => {
                let session_id = self.require_attached(&session_id)?;
                self.runtimes
                    .request(session_id, &PiCommand::SetSessionName { name })?;
                Ok(snapshot_after_ack(session_id))
            }
            ClientRequest::SessionRelease { session_id } => {
                let session_id = self.require_attached(&session_id)?;
                self.runtimes.release(session_id)?;
                self.attached_sessions.remove(&session_id);
                self.event_receivers.remove(&session_id);
                Ok(vec![
                    ready(ServerEvent::RequestAck),
                    ready(ServerEvent::SessionState {
                        session_id: session_id.to_string(),
                        state: SessionState::Sleeping,
                    }),
                ])
            }
            ClientRequest::SessionPrompt {
                session_id,
                content,
            } => self.command_ack(
                &session_id,
                &PiCommand::Prompt {
                    message: content,
                    streaming_behavior: None,
                },
            ),
            ClientRequest::SessionSteer {
                session_id,
                content,
            } => self.command_ack(&session_id, &PiCommand::Steer { message: content }),
            ClientRequest::SessionFollowUp {
                session_id,
                content,
            } => self.command_ack(&session_id, &PiCommand::FollowUp { message: content }),
            ClientRequest::SessionAbort { session_id } => {
                self.command_ack(&session_id, &PiCommand::Abort)
            }
            ClientRequest::SessionCompact {
                session_id,
                instructions,
            } => self.command_ack(
                &session_id,
                &PiCommand::Compact {
                    custom_instructions: instructions,
                },
            ),
            ClientRequest::ModelList { session_id } => {
                let session_id = self.require_attached(&session_id)?;
                let response = self
                    .runtimes
                    .request(session_id, &PiCommand::GetAvailableModels)?;
                Ok(vec![ready(ServerEvent::ModelList {
                    session_id: session_id.to_string(),
                    models: pi_bridge::available_models(&response)?,
                })])
            }
            ClientRequest::ModelSet {
                session_id,
                provider,
                model_id,
            } => self.command_ack(&session_id, &PiCommand::SetModel { provider, model_id }),
            ClientRequest::ThinkingSet { session_id, level } => self.command_ack(
                &session_id,
                &PiCommand::SetThinkingLevel {
                    level: thinking_level(level),
                },
            ),
            ClientRequest::ExtensionUiRespond {
                session_id,
                extension_request_id,
                answer,
            } => self.command_ack(
                &session_id,
                &PiCommand::ExtensionUiResponse {
                    id: extension_request_id,
                    response: extension_answer(answer),
                },
            ),
        }
    }

    fn host_snapshot(&self) -> HostSnapshot {
        let started = Instant::now();
        let config = self.host.snapshot();
        let snapshot = HostSnapshot {
            host: HostSummary {
                id: config.host.id,
                display_name: config.host.display_name.clone(),
            },
            workspaces: self.cached_workspace_summaries(&config),
            relay: self.relay_access(&config),
        };
        let response_bytes = serde_json::to_vec(&snapshot)
            .map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        crate::diagnostics::record(
            "host.snapshot",
            &[
                ("validation_ms", crate::diagnostics::elapsed_ms(started)),
                (
                    "workspace_count",
                    u64::try_from(snapshot.workspaces.len()).unwrap_or(u64::MAX),
                ),
                ("response_bytes", response_bytes),
            ],
        );
        snapshot
    }

    /// Relay reachability for the requesting device. The channel secret is
    /// per-device material and is only ever sent to its own authenticated
    /// device inside the encrypted channel.
    fn relay_access(&self, config: &HostConfig) -> Option<RelayAccess> {
        let url = config.preferences.active_relay_url()?;
        let device_id = self.device_id.as_deref()?;
        let device = config
            .devices
            .iter()
            .find(|device| device.id == device_id)?;
        Some(RelayAccess {
            url: url.to_owned(),
            channel_secret: device.relay_channel.clone(),
        })
    }

    fn workspace_summaries(&self) -> Vec<WorkspaceSummary> {
        let config = self.host.snapshot();
        self.cached_workspace_summaries(&config)
    }

    fn cached_workspace_summaries(&self, config: &HostConfig) -> Vec<WorkspaceSummary> {
        let mut catalog = self
            .host
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        config
            .workspaces
            .iter()
            .map(|workspace| {
                let availability = match catalog.workspaces.get(&workspace.id) {
                    Some(cached)
                        if now.duration_since(cached.checked_at) < WORKSPACE_AVAILABILITY_TTL =>
                    {
                        cached.availability
                    }
                    _ => {
                        let availability = if WorkspaceRegistry::new(&mut config.clone())
                            .authorized_root(workspace.id)
                            .is_ok()
                        {
                            WorkspaceAvailability::Available
                        } else {
                            WorkspaceAvailability::Unavailable
                        };
                        catalog.workspaces.insert(
                            workspace.id,
                            CachedWorkspaceAvailability {
                                checked_at: now,
                                availability,
                            },
                        );
                        availability
                    }
                };
                WorkspaceSummary {
                    id: workspace.id,
                    name: workspace.name.clone(),
                    availability,
                }
            })
            .collect()
    }

    fn session_list(
        &self,
        workspace_id: Uuid,
        limit: Option<u32>,
    ) -> Result<ServerEvent, DispatchError> {
        let workspace = self.authorized_workspace(workspace_id)?;
        let store = PiSessionStore::for_workspace(&workspace)?;
        let limit = limit
            .filter(|value| *value > 0)
            .map(|value| usize::try_from(value.min(MAX_SESSION_LIST)).unwrap_or(200));
        let (discovered, timing) = {
            let mut catalog = self
                .host
                .catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store.list_cached(&mut catalog.sessions, limit)?
        };
        let sessions = discovered
            .into_iter()
            .map(|session| WireSessionSummary {
                id: session.summary.id.to_string(),
                name: session.summary.name,
                modified_at: session.summary.modified_at.to_rfc3339(),
                message_count: session.summary.message_count,
                first_user_message: session.summary.first_user_message,
                state: match self.runtimes.is_completed(session.summary.id) {
                    Some(true) => SessionState::Idle,
                    Some(false) => SessionState::Running,
                    None => SessionState::Sleeping,
                },
            })
            .collect();
        let event = ServerEvent::SessionList {
            workspace_id,
            sessions,
        };
        let response_bytes = serde_json::to_vec(&event)
            .map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        crate::diagnostics::record(
            "session.list",
            &[
                ("enumerate_ms", timing.enumerate_ms),
                ("scan_ms", timing.scan_ms),
                ("file_count", timing.file_count),
                ("session_count", timing.session_count),
                ("parsed_count", timing.parsed_count),
                ("reused_count", timing.reused_count),
                ("response_bytes", response_bytes),
            ],
        );
        Ok(event)
    }

    fn create_session(
        &mut self,
        workspace_id: Uuid,
        name: Option<String>,
    ) -> Result<Vec<PendingEvent>, DispatchError> {
        let workspace = self.authorized_workspace(workspace_id)?;
        self.invalidate_session_index(&workspace);
        let session_id = self.runtimes.create(workspace, name)?;
        self.attached_sessions.insert(session_id);
        if let Err(error) = self.subscribe_session(session_id) {
            self.attached_sessions.remove(&session_id);
            let _ = self.runtimes.release(session_id);
            return Err(error);
        }
        Ok(snapshot_after_ack(session_id))
    }

    fn attach_session(&mut self, value: &str) -> Result<ServerEvent, DispatchError> {
        let session_id = parse_session_id(value)?;
        let already_attached = self.attached_sessions.contains(&session_id);
        if !already_attached {
            if self.runtimes.is_active(session_id) {
                self.ensure_active_session_authorized(session_id)?;
                self.runtimes.attach(session_id)?;
            } else {
                let located = self.find_authorized_session(session_id)?;
                self.runtimes.open(
                    &located.workspace,
                    located.store.session_directory(),
                    &located.session,
                )?;
            }
            self.attached_sessions.insert(session_id);
        }
        if let Err(error) = self.subscribe_session(session_id) {
            if !already_attached {
                self.attached_sessions.remove(&session_id);
                let _ = self.runtimes.detach(session_id);
            }
            return Err(error);
        }
        match self.session_snapshot_event(session_id) {
            Ok(event) => Ok(event),
            Err(error) if !already_attached => {
                self.attached_sessions.remove(&session_id);
                self.event_receivers.remove(&session_id);
                let _ = self.runtimes.detach(session_id);
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn command_ack(
        &self,
        session_id: &str,
        command: &PiCommand,
    ) -> Result<Vec<PendingEvent>, DispatchError> {
        let session_id = self.require_attached(session_id)?;
        self.runtimes.request(session_id, command)?;
        Ok(vec![ready(ServerEvent::RequestAck)])
    }

    fn session_snapshot_event(&self, session_id: SessionId) -> Result<ServerEvent, DispatchError> {
        let snapshot = self.runtimes.snapshot(session_id)?;
        Ok(ServerEvent::SessionSnapshot {
            snapshot: pi_bridge::session_snapshot(session_id, snapshot)?,
        })
    }

    fn subscribe_session(&mut self, session_id: SessionId) -> Result<(), DispatchError> {
        if self.event_receivers.contains_key(&session_id) {
            return Ok(());
        }
        let receiver = self.runtimes.subscribe(session_id)?;
        self.event_receivers.insert(session_id, receiver);
        Ok(())
    }

    fn require_attached(&self, value: &str) -> Result<SessionId, DispatchError> {
        let session_id = parse_session_id(value)?;
        if !self.attached_sessions.contains(&session_id) {
            return Err(DispatchError::NotAttached(session_id));
        }
        Ok(session_id)
    }

    fn invalidate_session_index(&self, workspace: &std::path::Path) {
        if let Ok(store) = PiSessionStore::for_workspace(workspace) {
            self.host
                .catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sessions
                .invalidate_directory(store.session_directory());
        }
    }

    fn authorized_workspace(&self, workspace_id: Uuid) -> Result<PathBuf, DispatchError> {
        let mut config = self.host.snapshot();
        Ok(WorkspaceRegistry::new(&mut config).authorized_root(workspace_id)?)
    }

    fn ensure_active_session_authorized(&self, session_id: SessionId) -> Result<(), DispatchError> {
        let workspace = self
            .runtimes
            .workspace(session_id)
            .ok_or(DispatchError::SessionNotFound(session_id))?;
        let mut config = self.host.snapshot();
        let workspace_id = config
            .workspaces
            .iter()
            .find(|record| record.path == workspace)
            .map(|record| record.id)
            .ok_or(DispatchError::UnauthorizedSession(session_id))?;
        WorkspaceRegistry::new(&mut config).authorized_root(workspace_id)?;
        Ok(())
    }

    fn find_authorized_session(
        &self,
        session_id: SessionId,
    ) -> Result<LocatedSession, DispatchError> {
        let config = self.host.snapshot();
        for workspace in &config.workspaces {
            let mut candidate = config.clone();
            let Ok(root) = WorkspaceRegistry::new(&mut candidate).authorized_root(workspace.id)
            else {
                continue;
            };
            let store = PiSessionStore::for_workspace(&root)?;
            match store.find(session_id) {
                Ok(session) => {
                    return Ok(LocatedSession {
                        workspace: root,
                        store,
                        session,
                    });
                }
                Err(SessionError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(DispatchError::SessionNotFound(session_id))
    }
}

impl Drop for HostProtocolDispatcher {
    fn drop(&mut self) {
        self.disconnect();
    }
}

struct LocatedSession {
    workspace: PathBuf,
    store: PiSessionStore,
    session: DiscoveredSession,
}

pub(crate) struct PendingResponse {
    request_id: u64,
    event: PendingEvent,
}

#[allow(clippy::large_enum_variant)]
enum PendingEvent {
    Ready(ServerEvent),
    SessionSnapshot(SessionId),
}

fn ready(event: ServerEvent) -> PendingEvent {
    PendingEvent::Ready(event)
}

fn snapshot_after_ack(session_id: SessionId) -> Vec<PendingEvent> {
    vec![
        ready(ServerEvent::RequestAck),
        PendingEvent::SessionSnapshot(session_id),
    ]
}

fn response(request_id: u64, event: ServerEvent) -> ServerEnvelope {
    ServerEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id: Some(request_id),
        event,
    }
}

fn unsolicited(event: ServerEvent) -> ServerEnvelope {
    ServerEnvelope {
        protocol: PROTOCOL_MAJOR,
        request_id: None,
        event,
    }
}

fn parse_session_id(value: &str) -> Result<SessionId, DispatchError> {
    SessionId::from_str(value).map_err(|_| DispatchError::InvalidSessionId)
}

const fn thinking_level(level: pix_wire::ThinkingLevel) -> PiThinkingLevel {
    match level {
        pix_wire::ThinkingLevel::Off => PiThinkingLevel::Off,
        pix_wire::ThinkingLevel::Minimal => PiThinkingLevel::Minimal,
        pix_wire::ThinkingLevel::Low => PiThinkingLevel::Low,
        pix_wire::ThinkingLevel::Medium => PiThinkingLevel::Medium,
        pix_wire::ThinkingLevel::High => PiThinkingLevel::High,
        pix_wire::ThinkingLevel::Xhigh => PiThinkingLevel::Xhigh,
        pix_wire::ThinkingLevel::Max => PiThinkingLevel::Max,
    }
}

fn extension_answer(answer: pix_wire::ExtensionUiAnswer) -> PiExtensionUiAnswer {
    match answer {
        pix_wire::ExtensionUiAnswer::Value { value } => PiExtensionUiAnswer::Value { value },
        pix_wire::ExtensionUiAnswer::Confirmed { confirmed } => {
            PiExtensionUiAnswer::Confirmed { confirmed }
        }
        pix_wire::ExtensionUiAnswer::Cancelled => {
            PiExtensionUiAnswer::Cancelled { cancelled: true }
        }
    }
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("session ID is invalid")]
    InvalidSessionId,
    #[error("session was not found: {0}")]
    SessionNotFound(SessionId),
    #[error("connection is not attached to session: {0}")]
    NotAttached(SessionId),
    #[error("active session is no longer in an authorized workspace: {0}")]
    UnauthorizedSession(SessionId),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Runtime(#[from] RuntimeManagerError),
    #[error(transparent)]
    PiBridge(#[from] PiBridgeError),
}

impl DispatchError {
    fn public_event(&self) -> ServerEvent {
        let (code, message, retryable) = match self {
            Self::InvalidSessionId => (ErrorCode::InvalidRequest, "Invalid session ID", false),
            Self::SessionNotFound(_) | Self::Workspace(WorkspaceError::UnknownWorkspace(_)) => (
                ErrorCode::NotFound,
                "Requested resource was not found",
                false,
            ),
            Self::NotAttached(_) => (
                ErrorCode::Conflict,
                "Attach the session before sending this request",
                false,
            ),
            Self::UnauthorizedSession(_)
            | Self::Workspace(
                WorkspaceError::OutsideAuthorizedRoot { .. }
                | WorkspaceError::AuthorizedRootChanged { .. },
            ) => (
                ErrorCode::Unauthorized,
                "Workspace authorization is no longer valid",
                false,
            ),
            Self::Runtime(RuntimeManagerError::Capacity { .. }) => (
                ErrorCode::Capacity,
                "Active session capacity has been reached",
                true,
            ),
            Self::Runtime(
                RuntimeManagerError::NotActive(_)
                | RuntimeManagerError::NoAttachedClient(_)
                | RuntimeManagerError::Busy(_),
            ) => (
                ErrorCode::Conflict,
                "Session state changed; attach again for a fresh snapshot",
                true,
            ),
            Self::Workspace(_) | Self::Session(_) | Self::Runtime(_) | Self::PiBridge(_) => (
                ErrorCode::PiUnavailable,
                "Pi session is temporarily unavailable",
                true,
            ),
        };
        ServerEvent::Error {
            code,
            message: message.to_owned(),
            retryable,
        }
    }
}
