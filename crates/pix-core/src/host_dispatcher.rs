use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pix_wire::{
    ClientEnvelope, ClientRequest, ErrorCode, HOST_CAPABILITIES, HistoryPageItem,
    HistoryPresentation, HistoryProcessSummary, HostSnapshot, HostSummary,
    MAX_ATTACHMENTS_PER_REQUEST, MAX_IMAGE_CHUNK_BYTES, PROTOCOL_MAJOR, RelayAccess,
    ServerEnvelope, ServerEvent, SessionState, SessionSummary as WireSessionSummary,
    TurnPresentationState, WorkspaceAvailability, WorkspaceSummary,
};
use thiserror::Error;
use uuid::Uuid;

use crate::config::{ConfigError, ConfigStore, HostConfig};
use crate::image_assets::{ImageAsset, ImageAssetChunk, ImageAssetError, ImageAssetStore};
use crate::pi_bridge::{self, PiBridgeError};
use crate::pi_rpc::{
    ExtensionUiAnswer as PiExtensionUiAnswer, PiCommand, PiEvent, PiImage,
    ThinkingLevel as PiThinkingLevel,
};
use crate::runtime_manager::{RuntimeManager, RuntimeManagerError};
use crate::session::{DiscoveredSession, PiSessionStore, SessionError, SessionMetadataIndex};
use crate::session_history::{self, HistoryError};
use crate::session_lock::SessionId;
use crate::workspace::{WorkspaceError, WorkspaceRegistry};

const WORKSPACE_AVAILABILITY_TTL: Duration = Duration::from_secs(10);
const MAX_SESSION_LIST: u32 = 200;
/// Idle lifetime of a not-yet-consumed attachment upload.
const ATTACHMENT_IDLE_TTL: Duration = Duration::from_secs(600);
/// Attachment uploads buffered per connection. This must accommodate every
/// image that the client uploads before the prompt consumes the references.
const MAX_PENDING_ATTACHMENTS: usize = MAX_ATTACHMENTS_PER_REQUEST;
/// Aggregate decoded bytes reserved by unfinished uploads on one connection.
/// Keeping this below the theoretical 9 × 4 MiB ceiling prevents a client from
/// pinning a large amount of host memory while it delays `attachment.finish`.
const MAX_PENDING_ATTACHMENT_BYTES: usize = 16 * 1024 * 1024;
const LEGACY_MAX_ATTACHMENTS_PER_REQUEST: usize = 4;
/// Ceiling on total base64 image bytes in one Pi prompt command; keeps the
/// Pi RPC JSONL record comfortably below its 16 MiB limit.
const MAX_PROMPT_IMAGE_BASE64_BYTES: usize = 12 * 1024 * 1024;
const CAPABILITY_COMMANDS: &str = "commands.v1";
const CAPABILITY_QUEUE: &str = "queue.v1";
const CAPABILITY_ATTACHMENTS: &str = "attachments.v1";
const CAPABILITY_ATTACHMENTS_V2: &str = "attachments.v2";
const CAPABILITY_USAGE: &str = "usage.v1";
const CAPABILITY_THINKING_LEVELS: &str = "thinking_levels.v1";
const CAPABILITY_SESSION_METADATA: &str = "session_metadata.v1";
const CAPABILITY_IMAGE_REFS: &str = "image_refs.v1";
const CAPABILITY_SESSION_HISTORY: &str = "session_history.v1";
const CAPABILITY_HISTORY_ITEMS: &str = "history_items.v1";
const CAPABILITY_HISTORY_PRESENTATION: &str = "history_presentation.v1";
const SESSION_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(8);
const RUNTIME_METADATA_TIMEOUT: Duration = Duration::from_secs(2);

/// Shared, conversation-free host configuration visible to all connections.
pub struct HostState {
    config: RwLock<HostConfig>,
    catalog: Mutex<HostCatalog>,
    image_assets: Arc<ImageAssetStore>,
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
        let root = ConfigStore::default_path()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("attachments")))
            .unwrap_or_else(|| std::env::temp_dir().join("pix").join("attachments"));
        Self::with_asset_root(config, root)
    }

    /// Constructs host state with an explicit durable asset root. The service
    /// uses the directory next to its `ConfigStore`; tests and embedded callers
    /// can isolate image files in a temporary directory.
    #[must_use]
    pub fn with_asset_root(config: HostConfig, root: impl Into<PathBuf>) -> Self {
        Self {
            config: RwLock::new(config),
            catalog: Mutex::new(HostCatalog::default()),
            image_assets: Arc::new(ImageAssetStore::new(root)),
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

    #[must_use]
    pub fn image_assets(&self) -> Arc<ImageAssetStore> {
        Arc::clone(&self.image_assets)
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
    /// Events that arrived after a TUI snapshot cursor while the snapshot was
    /// being assembled. They must be delivered in order after the snapshot.
    buffered_events: HashMap<SessionId, VecDeque<PiEvent>>,
    /// Optional protocol extensions the connected client declared. Every
    /// gated field and event is omitted until the declaration arrives.
    client_capabilities: HashSet<String>,
    /// Attachment uploads staged on this connection. Durable asset files are
    /// retained for history/lazy loading; the staging entries are dropped on
    /// disconnect, expiry, or the prompt that consumes them.
    attachments: HashMap<String, PendingAttachment>,
    pending_attachment_bytes: usize,
    metadata_events: mpsc::Receiver<ServerEvent>,
    metadata_sender: mpsc::Sender<ServerEvent>,
    metadata_cancel: Arc<AtomicBool>,
}

/// One assembling attachment upload. Bytes are staged until `finish`, then
/// persisted as a host asset and consumed by the prompt that references it.
struct PendingAttachment {
    session_id: SessionId,
    mime_type: String,
    expected_size: usize,
    buffer: Vec<u8>,
    ready: bool,
    asset: Option<ImageAsset>,
    updated: Instant,
}

impl HostProtocolDispatcher {
    #[must_use]
    pub fn new(host: Arc<HostState>, runtimes: Arc<RuntimeManager>) -> Self {
        let (metadata_sender, metadata_events) = mpsc::channel();
        Self {
            host,
            runtimes,
            device_id: None,
            attached_sessions: HashSet::new(),
            event_receivers: HashMap::new(),
            buffered_events: HashMap::new(),
            client_capabilities: HashSet::new(),
            attachments: HashMap::new(),
            pending_attachment_bytes: 0,
            metadata_events,
            metadata_sender,
            metadata_cancel: Arc::new(AtomicBool::new(true)),
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
        self.ensure_device_authorized()?;
        let session_id = parse_session_id(session_id)?;
        if !self.attached_sessions.contains(&session_id) {
            return Err(DispatchError::NotAttached(session_id));
        }
        self.ensure_active_session_authorized(session_id)?;
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
        self.ensure_device_authorized()?;
        let session_id = parse_session_id(session_id)?;
        if !self.attached_sessions.contains(&session_id) {
            return Err(DispatchError::NotAttached(session_id));
        }
        self.ensure_active_session_authorized(session_id)?;
        let mapped = pi_bridge::event(session_id, event)?;
        if let Some(event) = &mapped {
            match event {
                ServerEvent::SessionState { state, .. } => {
                    self.runtimes.mark_state(session_id, *state);
                }
                ServerEvent::SessionQueue { queue, .. } => {
                    // Cache unconditionally so a reconnect that declares
                    // `queue.v1` still recovers the live queue text.
                    self.runtimes.record_queue(session_id, queue.clone());
                    if !self.client_capabilities.contains(CAPABILITY_QUEUE) {
                        return Ok(None);
                    }
                }
                ServerEvent::Compaction { .. } => self
                    .runtimes
                    .mark_state(session_id, SessionState::Compacting),
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
        if let Err(error) = self.ensure_device_authorized() {
            self.disconnect();
            return vec![unsolicited(error.public_event())];
        }
        let mut envelopes = Vec::new();
        while let Ok(event) = self.metadata_events.try_recv() {
            // A metadata query may finish after the user detached the session;
            // never deliver stale enrichment to a different attachment.
            let attached = match &event {
                ServerEvent::SessionMetadata { session_id, .. } => parse_session_id(session_id)
                    .map(|id| self.attached_sessions.contains(&id))
                    .unwrap_or(false),
                _ => true,
            };
            if attached {
                envelopes.push(unsolicited(event));
            }
        }

        let mut raw_events = Vec::new();
        for (&session_id, buffered) in &mut self.buffered_events {
            while let Some(event) = buffered.pop_front() {
                raw_events.push((session_id, event));
            }
        }
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
            self.buffered_events.remove(&session_id);
        }

        envelopes.extend(
            raw_events
                .into_iter()
                .filter_map(|(session_id, event)| {
                    match self.map_pi_event(&session_id.to_string(), event) {
                        Ok(mapped) => mapped,
                        Err(error) => Some(unsolicited(error.public_event())),
                    }
                })
                .collect::<Vec<_>>(),
        );
        envelopes
    }

    /// Detaches this connection from every session without stopping Pi work.
    pub fn disconnect(&mut self) {
        self.metadata_cancel.store(false, Ordering::Release);
        for session_id in self.attached_sessions.drain() {
            let _ = self.runtimes.detach(session_id);
        }
        self.event_receivers.clear();
        self.buffered_events.clear();
        self.attachments.clear();
        self.pending_attachment_bytes = 0;
    }

    pub(crate) fn prepare_dispatch(&mut self, envelope: ClientEnvelope) -> Vec<PendingResponse> {
        self.sweep_expired_attachments();
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

    pub(crate) fn resolve_response(&mut self, pending: PendingResponse) -> ServerEnvelope {
        let event = match pending.event {
            PendingEvent::Ready(event) => event,
            PendingEvent::SessionSnapshot {
                session_id,
                cleanup_on_error,
            } => match self.session_snapshot_event(session_id) {
                Ok(event) => event,
                Err(error) => {
                    if cleanup_on_error {
                        self.cleanup_failed_session_start(session_id);
                    }
                    error.public_event()
                }
            },
        };
        response(pending.request_id, event)
    }

    #[allow(clippy::too_many_lines)]
    fn handle(&mut self, request: ClientRequest) -> Result<Vec<PendingEvent>, DispatchError> {
        self.ensure_device_authorized()?;
        match request {
            ClientRequest::HostSnapshot { capabilities } => {
                self.client_capabilities = capabilities.into_iter().collect();
                Ok(vec![ready(ServerEvent::HostSnapshot {
                    snapshot: self.host_snapshot(),
                })])
            }
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
            ClientRequest::SessionHistoryRequest {
                session_id,
                before,
                limit,
            } => Ok(vec![ready(self.session_history_page(
                &session_id,
                &before,
                limit,
            )?)]),
            ClientRequest::SessionRename { session_id, name } => {
                let session_id = self.require_attached(&session_id)?;
                self.runtimes
                    .request_backend(session_id, &PiCommand::SetSessionName { name })?;
                Ok(snapshot_after_ack(session_id, false))
            }
            ClientRequest::SessionRelease { session_id } => {
                let session_id = self.require_attached(&session_id)?;
                self.runtimes.release(session_id)?;
                self.attached_sessions.remove(&session_id);
                self.event_receivers.remove(&session_id);
                self.buffered_events.remove(&session_id);
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
                attachments,
            } => {
                let (message, images) = self.prepare_prompt(&session_id, content, &attachments)?;
                self.command_ack(
                    &session_id,
                    &PiCommand::Prompt {
                        message,
                        streaming_behavior: None,
                        images,
                    },
                )
            }
            ClientRequest::SessionSteer {
                session_id,
                content,
                attachments,
            } => {
                let (message, images) = self.prepare_prompt(&session_id, content, &attachments)?;
                self.command_ack(&session_id, &PiCommand::Steer { message, images })
            }
            ClientRequest::SessionFollowUp {
                session_id,
                content,
                attachments,
            } => {
                let (message, images) = self.prepare_prompt(&session_id, content, &attachments)?;
                self.command_ack(&session_id, &PiCommand::FollowUp { message, images })
            }
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
                    .request_backend(session_id, &PiCommand::GetAvailableModels)?;
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
            ClientRequest::AttachmentBegin {
                session_id,
                attachment_id,
                mime_type,
                size,
            } => {
                let session_id = self.require_attached(&session_id)?;
                self.require_capability(CAPABILITY_ATTACHMENTS)?;
                if self.attachments.contains_key(&attachment_id) {
                    return Err(DispatchError::InvalidAttachment(
                        "Attachment ID is already in use",
                    ));
                }
                if self.attachments.len() >= self.max_attachments_per_request() {
                    return Err(DispatchError::InvalidAttachment(
                        "Too many pending attachments on this connection",
                    ));
                }
                let expected_size = usize::try_from(size).map_err(|_| {
                    DispatchError::InvalidAttachment("Attachment size is invalid for this host")
                })?;
                if self
                    .pending_attachment_bytes
                    .checked_add(expected_size)
                    .is_none_or(|bytes| bytes > MAX_PENDING_ATTACHMENT_BYTES)
                {
                    return Err(DispatchError::InvalidAttachment(
                        "Pending attachment bytes exceed the per-connection limit",
                    ));
                }
                self.pending_attachment_bytes += expected_size;
                self.attachments.insert(
                    attachment_id,
                    PendingAttachment {
                        session_id,
                        mime_type,
                        expected_size,
                        buffer: Vec::new(),
                        ready: false,
                        asset: None,
                        updated: Instant::now(),
                    },
                );
                Ok(vec![ready(ServerEvent::RequestAck)])
            }
            ClientRequest::AttachmentChunk {
                attachment_id,
                data,
            } => {
                self.require_capability(CAPABILITY_ATTACHMENTS)?;
                let is_ready = self
                    .attachments
                    .get(&attachment_id)
                    .ok_or(DispatchError::InvalidAttachment(
                        "Attachment upload was not found",
                    ))?
                    .ready;
                if is_ready {
                    return Err(DispatchError::InvalidAttachment(
                        "Attachment upload is already finished",
                    ));
                }
                let bytes = STANDARD.decode(&data).map_err(|_| {
                    DispatchError::InvalidAttachment("Attachment chunk is not canonical base64")
                })?;
                let exceeds_size = self
                    .attachments
                    .get(&attachment_id)
                    .is_some_and(|attachment| {
                        attachment.buffer.len() + bytes.len() > attachment.expected_size
                    });
                if exceeds_size {
                    self.remove_attachment(&attachment_id);
                    return Err(DispatchError::InvalidAttachment(
                        "Attachment chunks exceed the declared size",
                    ));
                }
                let attachment = self.attachments.get_mut(&attachment_id).ok_or(
                    DispatchError::InvalidAttachment("Attachment upload was not found"),
                )?;
                attachment.buffer.extend_from_slice(&bytes);
                attachment.updated = Instant::now();
                Ok(vec![ready(ServerEvent::RequestAck)])
            }
            ClientRequest::AttachmentFinish { attachment_id } => {
                self.require_capability(CAPABILITY_ATTACHMENTS)?;
                let expected_size = self
                    .attachments
                    .get(&attachment_id)
                    .ok_or(DispatchError::InvalidAttachment(
                        "Attachment upload was not found",
                    ))?
                    .expected_size;
                let size_matches = self
                    .attachments
                    .get(&attachment_id)
                    .is_some_and(|attachment| attachment.buffer.len() == expected_size);
                if !size_matches {
                    self.remove_attachment(&attachment_id);
                    return Err(DispatchError::InvalidAttachment(
                        "Attachment byte count does not match the declared size",
                    ));
                }
                let asset = {
                    let attachment = self.attachments.get(&attachment_id).ok_or(
                        DispatchError::InvalidAttachment("Attachment upload was not found"),
                    )?;
                    self.host.image_assets().persist_named(
                        attachment.session_id,
                        &attachment_id,
                        attachment.mime_type.clone(),
                        &attachment.buffer,
                    )?
                };
                if let Some(attachment) = self.attachments.get_mut(&attachment_id) {
                    attachment.asset = Some(asset);
                    // Once the durable source exists, do not retain a second
                    // full copy in the connection-scoped staging map.
                    attachment.buffer.clear();
                    attachment.ready = true;
                    attachment.updated = Instant::now();
                }
                self.pending_attachment_bytes =
                    self.pending_attachment_bytes.saturating_sub(expected_size);
                Ok(vec![ready(ServerEvent::RequestAck)])
            }
            ClientRequest::ImageGet {
                session_id,
                image_ref,
                offset,
                limit,
            } => {
                self.require_capability(CAPABILITY_IMAGE_REFS)?;
                let session_id = self.require_attached(&session_id)?;
                let max_image_chunk_bytes =
                    usize::try_from(MAX_IMAGE_CHUNK_BYTES).unwrap_or(usize::MAX);
                let limit = usize::try_from(limit)
                    .unwrap_or(max_image_chunk_bytes)
                    .min(max_image_chunk_bytes);
                let chunk = self
                    .host
                    .image_assets()
                    .read_chunk(session_id, &image_ref, offset, limit)?;
                Ok(vec![ready(image_chunk_event(chunk))])
            }
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
            capabilities: HOST_CAPABILITIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
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
            .map(|session| {
                let state = if self.runtimes.client_count(session.summary.id) == Some(0) {
                    self.runtimes
                        .refresh_state(session.summary.id)
                        .ok()
                        .or_else(|| self.runtimes.session_state(session.summary.id))
                        .unwrap_or(SessionState::Sleeping)
                } else {
                    self.runtimes
                        .session_state(session.summary.id)
                        .unwrap_or(SessionState::Sleeping)
                };
                WireSessionSummary {
                    id: session.summary.id.to_string(),
                    name: session.summary.name,
                    modified_at: session.summary.modified_at.to_rfc3339(),
                    message_count: session.summary.message_count,
                    first_user_message: session.summary.first_user_message,
                    state,
                }
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
            self.release_failed_runtime(session_id);
            return Err(error);
        }
        Ok(snapshot_after_ack(session_id, true))
    }

    fn attach_session(&mut self, value: &str) -> Result<ServerEvent, DispatchError> {
        let session_id = parse_session_id(value)?;
        let already_attached = self.attached_sessions.contains(&session_id);
        let mut opened_runtime = false;
        if already_attached {
            self.ensure_active_session_authorized(session_id)?;
        } else {
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
                opened_runtime = true;
            }
            self.attached_sessions.insert(session_id);
        }
        if let Err(error) = self.subscribe_session(session_id) {
            if !already_attached {
                self.attached_sessions.remove(&session_id);
                if opened_runtime {
                    self.release_failed_runtime(session_id);
                } else {
                    let _ = self.runtimes.detach(session_id);
                }
            }
            return Err(error);
        }
        match self.session_snapshot_event(session_id) {
            Ok(event) => Ok(event),
            Err(error) if !already_attached => {
                self.attached_sessions.remove(&session_id);
                self.event_receivers.remove(&session_id);
                self.buffered_events.remove(&session_id);
                if opened_runtime {
                    self.release_failed_runtime(session_id);
                } else {
                    let _ = self.runtimes.detach(session_id);
                }
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
        self.runtimes.request_backend(session_id, command)?;
        Ok(vec![ready(ServerEvent::RequestAck)])
    }

    fn session_snapshot_event(
        &mut self,
        session_id: SessionId,
    ) -> Result<ServerEvent, DispatchError> {
        self.ensure_device_authorized()?;
        self.ensure_active_session_authorized(session_id)?;
        let history_capability = self
            .client_capabilities
            .contains(CAPABILITY_SESSION_HISTORY);
        let history_items_capability = self.client_capabilities.contains(CAPABILITY_HISTORY_ITEMS);
        let history_presentation_capability = self
            .client_capabilities
            .contains(CAPABILITY_HISTORY_PRESENTATION);
        let (runtime_snapshot, through_sequence) = if history_capability {
            self.runtimes
                .snapshot_state_with_timeout_and_cursor(session_id, SESSION_SNAPSHOT_TIMEOUT)?
        } else {
            self.runtimes
                .snapshot_with_timeout_and_cursor(session_id, SESSION_SNAPSHOT_TIMEOUT)?
        };
        if let Some(through_sequence) = through_sequence {
            self.discard_tui_events_through(session_id, through_sequence);
        }
        let mut snapshot = pi_bridge::session_snapshot(session_id, runtime_snapshot)?;
        if history_capability {
            let mut page = if let Some(page) = self.read_history_page(session_id, None, 50)? {
                page
            } else {
                // A brand-new Pi session may not have flushed its JSONL
                // header yet. Fall back once to Pi's in-memory view so the
                // create/first-prompt path remains immediately usable.
                let (full, through_sequence) = self
                    .runtimes
                    .snapshot_with_timeout_and_cursor(session_id, SESSION_SNAPSHOT_TIMEOUT)?;
                if let Some(through_sequence) = through_sequence {
                    self.discard_tui_events_through(session_id, through_sequence);
                }
                let mut messages = full.messages;
                if self.client_capabilities.contains(CAPABILITY_IMAGE_REFS) {
                    self.host
                        .image_assets()
                        .externalize_messages(session_id, &mut messages)?;
                }
                session_history::initial_page(&session_id.to_string(), &messages)?
            };
            if self.client_capabilities.contains(CAPABILITY_IMAGE_REFS) {
                self.host
                    .image_assets()
                    .externalize_messages(session_id, &mut page.messages)?;
            }
            if history_items_capability {
                promote_page_to_history_items(&mut page);
            }
            let mut history = session_history::state(&page);
            if history_presentation_capability {
                let mut presentation = self
                    .read_history_presentation(session_id, snapshot.state)
                    .unwrap_or_else(|_| empty_history_presentation(snapshot.state));
                suppress_presented_previews(&mut presentation, &page);
                history.presentation = Some(presentation);
            }
            snapshot.messages = page.messages;
            snapshot.history_items = page.history_items;
            snapshot.history = Some(history);
        }
        if !history_capability && self.client_capabilities.contains(CAPABILITY_IMAGE_REFS) {
            self.host
                .image_assets()
                .externalize_messages(session_id, &mut snapshot.messages)?;
        }
        if self.client_capabilities.contains(CAPABILITY_QUEUE) {
            snapshot.queue = self.runtimes.queue(session_id);
        }
        if self
            .client_capabilities
            .contains(CAPABILITY_SESSION_METADATA)
        {
            self.schedule_session_metadata(session_id);
        } else {
            // Legacy clients still receive the old fields, but every optional
            // probe is strictly bounded and best-effort. New clients declare
            // `session_metadata.v1` and receive the same data asynchronously
            // after this base snapshot has already been delivered.
            self.enrich_legacy_snapshot(&mut snapshot, session_id);
        }
        Ok(ServerEvent::SessionSnapshot { snapshot })
    }

    /// Reads a bounded history page from the native Pi JSONL session file.
    /// Returns `None` only for a new runtime whose file has not been flushed;
    /// callers may then use Pi's in-memory compatibility path once.
    fn read_history_page(
        &self,
        session_id: SessionId,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<Option<pix_wire::SessionHistoryPage>, DispatchError> {
        let started = Instant::now();
        let workspace = self
            .runtimes
            .workspace(session_id)
            .ok_or(DispatchError::SessionNotFound(session_id))?;
        let store = PiSessionStore::for_workspace(&workspace)?;
        let mut catalog = self
            .host
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let include_items = self.client_capabilities.contains(CAPABILITY_HISTORY_ITEMS);
        let result = if self.client_capabilities.contains(CAPABILITY_IMAGE_REFS) {
            let assets = self.host.image_assets();
            store.history_page_with_transform_cached_options(
                &mut catalog.sessions,
                session_id,
                cursor,
                limit,
                include_items,
                |mut message| {
                    assets.externalize_messages(session_id, std::slice::from_mut(&mut message))?;
                    Ok(message)
                },
            )
        } else {
            store.history_page_with_transform_cached_options(
                &mut catalog.sessions,
                session_id,
                cursor,
                limit,
                include_items,
                Ok,
            )
        };
        match result {
            Ok(page) => {
                let page_bytes = serde_json::to_vec(&page).map_or(0, |bytes| bytes.len());
                crate::diagnostics::record(
                    "session.history.page",
                    &[
                        ("read_ms", crate::diagnostics::elapsed_ms(started)),
                        ("page_bytes", u64::try_from(page_bytes).unwrap_or(u64::MAX)),
                        (
                            "item_count",
                            u64::try_from(if page.history_items.is_empty() {
                                page.messages.len()
                            } else {
                                page.history_items.len()
                            })
                            .unwrap_or(u64::MAX),
                        ),
                        ("has_more", u64::from(page.has_more)),
                        ("cursor_present", u64::from(cursor.is_some())),
                    ],
                );
                Ok(Some(page))
            }
            Err(SessionError::NotFound(_)) => Ok(None),
            Err(error) => {
                crate::diagnostics::record(
                    "session.history.page",
                    &[
                        ("read_ms", crate::diagnostics::elapsed_ms(started)),
                        ("failed", 1),
                    ],
                );
                Err(error.into())
            }
        }
    }

    fn read_history_presentation(
        &self,
        session_id: SessionId,
        state: SessionState,
    ) -> Result<HistoryPresentation, DispatchError> {
        let workspace = self
            .runtimes
            .workspace(session_id)
            .ok_or(DispatchError::SessionNotFound(session_id))?;
        let store = PiSessionStore::for_workspace(&workspace)?;
        let mut catalog = self
            .host
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(store.history_presentation_cached(&mut catalog.sessions, session_id, state)?)
    }

    fn session_history_page(
        &mut self,
        session_id: &str,
        before: &str,
        limit: u32,
    ) -> Result<ServerEvent, DispatchError> {
        self.require_capability(CAPABILITY_SESSION_HISTORY)?;
        let session_id = self.require_attached(session_id)?;
        let mut page = if let Some(page) =
            self.read_history_page(session_id, Some(before), limit)?
        {
            page
        } else {
            let (snapshot, through_sequence) = self
                .runtimes
                .snapshot_with_timeout_and_cursor(session_id, SESSION_SNAPSHOT_TIMEOUT)?;
            if let Some(through_sequence) = through_sequence {
                self.discard_tui_events_through(session_id, through_sequence);
            }
            let mut messages = snapshot.messages;
            if self.client_capabilities.contains(CAPABILITY_IMAGE_REFS) {
                self.host
                    .image_assets()
                    .externalize_messages(session_id, &mut messages)?;
            }
            session_history::page_from_cursor(&session_id.to_string(), &messages, before, limit)?
        };
        if self.client_capabilities.contains(CAPABILITY_IMAGE_REFS) {
            self.host
                .image_assets()
                .externalize_messages(session_id, &mut page.messages)?;
        }
        if self.client_capabilities.contains(CAPABILITY_HISTORY_ITEMS) {
            promote_page_to_history_items(&mut page);
        }
        Ok(ServerEvent::SessionHistoryPage { page })
    }

    fn enrich_legacy_snapshot(
        &self,
        snapshot: &mut pix_wire::SessionSnapshot,
        session_id: SessionId,
    ) {
        if self.client_capabilities.contains(CAPABILITY_COMMANDS) {
            match self.runtimes.request_backend_with_timeout(
                session_id,
                &PiCommand::GetCommands,
                RUNTIME_METADATA_TIMEOUT,
            ) {
                Ok(response) => match pi_bridge::commands(&response) {
                    Ok(commands) => snapshot.commands = commands,
                    Err(_) => crate::diagnostics::record("session.commands", &[("failed", 1)]),
                },
                Err(_) => crate::diagnostics::record("session.commands", &[("failed", 1)]),
            }
        }
        if self.client_capabilities.contains(CAPABILITY_USAGE)
            && let Ok(response) = self.runtimes.request_with_timeout(
                session_id,
                &PiCommand::GetSessionStats,
                RUNTIME_METADATA_TIMEOUT,
            )
            && let Ok(usage) = pi_bridge::usage(&response)
        {
            snapshot.usage = Some(usage);
        }
        if self
            .client_capabilities
            .contains(CAPABILITY_THINKING_LEVELS)
            && let Some(model) = snapshot.model.as_mut()
            && let Ok(response) = self.runtimes.request_with_timeout(
                session_id,
                &PiCommand::GetAvailableThinkingLevels,
                RUNTIME_METADATA_TIMEOUT,
            )
            && let Ok(levels) = pi_bridge::thinking_levels(&response)
        {
            model.thinking_levels = levels;
        }
    }

    fn schedule_session_metadata(&self, session_id: SessionId) {
        let runtimes = Arc::clone(&self.runtimes);
        let sender = self.metadata_sender.clone();
        let cancelled = Arc::clone(&self.metadata_cancel);
        let include_commands = self.client_capabilities.contains(CAPABILITY_COMMANDS);
        let include_usage = self.client_capabilities.contains(CAPABILITY_USAGE);
        let include_thinking = self
            .client_capabilities
            .contains(CAPABILITY_THINKING_LEVELS);
        let _ = std::thread::Builder::new()
            .name("pix-session-metadata".to_owned())
            .spawn(move || {
                let mut commands = None;
                let mut usage = None;
                let mut thinking_levels = None;
                if !cancelled.load(Ordering::Acquire) {
                    return;
                }
                if include_commands
                    && let Ok(response) = runtimes.request_backend_with_timeout(
                        session_id,
                        &PiCommand::GetCommands,
                        RUNTIME_METADATA_TIMEOUT,
                    )
                {
                    commands = pi_bridge::commands(&response).ok();
                }
                if !cancelled.load(Ordering::Acquire) {
                    return;
                }
                if include_usage
                    && let Ok(response) = runtimes.request_with_timeout(
                        session_id,
                        &PiCommand::GetSessionStats,
                        RUNTIME_METADATA_TIMEOUT,
                    )
                {
                    usage = pi_bridge::usage(&response).ok();
                }
                if !cancelled.load(Ordering::Acquire) {
                    return;
                }
                if include_thinking
                    && let Ok(response) = runtimes.request_with_timeout(
                        session_id,
                        &PiCommand::GetAvailableThinkingLevels,
                        RUNTIME_METADATA_TIMEOUT,
                    )
                {
                    thinking_levels = pi_bridge::thinking_levels(&response).ok();
                }
                if !cancelled.load(Ordering::Acquire) {
                    return;
                }
                let _ = sender.send(ServerEvent::SessionMetadata {
                    session_id: session_id.to_string(),
                    commands,
                    usage,
                    thinking_levels,
                });
            });
    }

    /// Resolves finished attachment uploads into Pi images and consumes them.
    /// An empty reference list needs no capability and changes nothing.
    fn prepare_prompt(
        &mut self,
        session_id: &str,
        content: String,
        attachments: &[String],
    ) -> Result<(String, Vec<PiImage>), DispatchError> {
        if !attachments.is_empty() {
            let parsed_session_id = self.require_attached(session_id)?;
            if self.runtimes.is_tui_attached(parsed_session_id) {
                return Err(RuntimeManagerError::TuiUnsupportedCommand(parsed_session_id).into());
            }
        }
        let (images, paths) = self.take_attachment_images(session_id, attachments)?;
        if paths.is_empty() {
            return Ok((content, images));
        }
        let mut message = content;
        message.push_str("\n\nAttached image paths (host-local):\n");
        for path in paths {
            message.push_str(&path.display().to_string());
            message.push('\n');
        }
        Ok((message, images))
    }

    fn take_attachment_images(
        &mut self,
        session_id: &str,
        attachments: &[String],
    ) -> Result<(Vec<PiImage>, Vec<PathBuf>), DispatchError> {
        if attachments.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        self.require_capability(CAPABILITY_ATTACHMENTS)?;
        if attachments.len() > self.max_attachments_per_request() {
            return Err(DispatchError::InvalidAttachment(
                "Too many attachments for this client capability",
            ));
        }
        let session_id = self.require_attached(session_id)?;
        let mut images = Vec::with_capacity(attachments.len());
        let mut paths = Vec::with_capacity(attachments.len());
        let mut total_base64 = 0_usize;
        for attachment_id in attachments {
            let attachment =
                self.remove_attachment(attachment_id)
                    .ok_or(DispatchError::InvalidAttachment(
                        "Attachment upload was not found",
                    ))?;
            if !attachment.ready {
                return Err(DispatchError::InvalidAttachment(
                    "Attachment upload is not finished",
                ));
            }
            if attachment.session_id != session_id {
                return Err(DispatchError::InvalidAttachment(
                    "Attachment belongs to a different session",
                ));
            }
            let asset = attachment.asset.ok_or(DispatchError::InvalidAttachment(
                "Attachment was not persisted",
            ))?;
            let bytes = std::fs::read(&asset.vision_path).map_err(|_| {
                DispatchError::InvalidAttachment("Attachment vision asset is unavailable")
            })?;
            let data = STANDARD.encode(bytes);
            total_base64 += data.len() + attachment.mime_type.len();
            images.push(PiImage::new(attachment.mime_type, data));
            paths.push(asset.agent_path);
        }
        if total_base64 > MAX_PROMPT_IMAGE_BASE64_BYTES {
            return Err(DispatchError::InvalidAttachment(
                "Attachment payload is too large for one Pi prompt",
            ));
        }
        Ok((images, paths))
    }

    fn require_capability(&self, capability: &'static str) -> Result<(), DispatchError> {
        if self.client_capabilities.contains(capability) {
            Ok(())
        } else {
            Err(DispatchError::MissingCapability(capability))
        }
    }

    fn max_attachments_per_request(&self) -> usize {
        if self.client_capabilities.contains(CAPABILITY_ATTACHMENTS_V2) {
            MAX_PENDING_ATTACHMENTS
        } else {
            LEGACY_MAX_ATTACHMENTS_PER_REQUEST
        }
    }

    fn remove_attachment(&mut self, attachment_id: &str) -> Option<PendingAttachment> {
        let attachment = self.attachments.remove(attachment_id)?;
        if !attachment.ready {
            self.pending_attachment_bytes = self
                .pending_attachment_bytes
                .saturating_sub(attachment.expected_size);
        }
        Some(attachment)
    }

    fn sweep_expired_attachments(&mut self) {
        let expired = self
            .attachments
            .iter()
            .filter(|(_, attachment)| attachment.updated.elapsed() >= ATTACHMENT_IDLE_TTL)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for attachment_id in expired {
            self.remove_attachment(&attachment_id);
        }
    }

    fn subscribe_session(&mut self, session_id: SessionId) -> Result<(), DispatchError> {
        if self.event_receivers.contains_key(&session_id) {
            return Ok(());
        }
        let receiver = self.runtimes.subscribe(session_id)?;
        self.event_receivers.insert(session_id, receiver);
        Ok(())
    }

    fn discard_tui_events_through(&mut self, session_id: SessionId, through_sequence: u64) {
        let mut keep = VecDeque::new();
        if let Some(receiver) = self.event_receivers.get(&session_id) {
            loop {
                match receiver.try_recv() {
                    Ok(PiEvent::Event {
                        sequence: Some(sequence),
                        ..
                    }) if sequence <= through_sequence => {}
                    Ok(event) => keep.push_back(event),
                    Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
        if !keep.is_empty() {
            self.buffered_events
                .entry(session_id)
                .or_default()
                .extend(keep);
        }
    }

    fn cleanup_failed_session_start(&mut self, session_id: SessionId) {
        self.attached_sessions.remove(&session_id);
        self.event_receivers.remove(&session_id);
        self.buffered_events.remove(&session_id);
        self.release_failed_runtime(session_id);
    }

    fn release_failed_runtime(&self, session_id: SessionId) {
        if self.runtimes.release(session_id).is_err() {
            // A concurrent operation may still own the operation gate. Drop
            // this connection's reference so the periodic idle sweep can
            // retry cleanup once that operation settles.
            let _ = self.runtimes.detach(session_id);
        }
    }

    fn require_attached(&self, value: &str) -> Result<SessionId, DispatchError> {
        let session_id = parse_session_id(value)?;
        if !self.attached_sessions.contains(&session_id) {
            return Err(DispatchError::NotAttached(session_id));
        }
        self.ensure_active_session_authorized(session_id)?;
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

    fn ensure_device_authorized(&self) -> Result<(), DispatchError> {
        let Some(device_id) = self.device_id.as_deref() else {
            // Standalone dispatcher tests and internal tooling do not attach a
            // device identity. Every network connection sets one before use.
            return Ok(());
        };
        if self
            .host
            .snapshot()
            .devices
            .iter()
            .any(|device| device.id == device_id)
        {
            Ok(())
        } else {
            Err(DispatchError::DeviceRevoked)
        }
    }

    fn find_authorized_session(
        &self,
        session_id: SessionId,
    ) -> Result<LocatedSession, DispatchError> {
        let config = self.host.snapshot();
        let mut catalog = self
            .host
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for workspace in &config.workspaces {
            let mut candidate = config.clone();
            let Ok(root) = WorkspaceRegistry::new(&mut candidate).authorized_root(workspace.id)
            else {
                continue;
            };
            let store = PiSessionStore::for_workspace(&root)?;
            match store.find_cached(&mut catalog.sessions, session_id) {
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

fn promote_page_to_history_items(page: &mut pix_wire::SessionHistoryPage) {
    if !page.history_items.is_empty() {
        page.messages.clear();
        return;
    }
    page.history_items = page
        .messages
        .iter()
        .enumerate()
        .map(|(offset, message)| HistoryPageItem::Message {
            index: page.start_index.saturating_add(offset),
            message: message.clone(),
        })
        .collect();
    page.messages.clear();
}

fn suppress_presented_previews(
    presentation: &mut HistoryPresentation,
    page: &pix_wire::SessionHistoryPage,
) {
    let contains = |index: usize| {
        if page.history_items.is_empty() {
            index >= page.start_index
                && index < page.start_index.saturating_add(page.messages.len())
        } else {
            page.history_items.iter().any(|item| match item {
                HistoryPageItem::Message {
                    index: item_index, ..
                }
                | HistoryPageItem::Placeholder {
                    index: item_index, ..
                } => *item_index == index,
            })
        }
    };
    if let Some(anchor) = presentation.user_anchor.as_mut()
        && contains(anchor.source_index)
    {
        anchor.preview = None;
    }
    if let Some(anchor) = presentation.terminal_anchor.as_mut()
        && contains(anchor.source_index)
    {
        anchor.preview = None;
    }
}

fn empty_history_presentation(state: SessionState) -> HistoryPresentation {
    let turn_state = match state {
        SessionState::Compacting => TurnPresentationState::Compacted,
        SessionState::Running | SessionState::Starting => TurnPresentationState::Active,
        SessionState::Unavailable => TurnPresentationState::Failed,
        SessionState::Sleeping | SessionState::Idle => TurnPresentationState::Completed,
    };
    HistoryPresentation {
        turn_state,
        user_anchor: None,
        terminal_anchor: None,
        process: HistoryProcessSummary {
            thought_count: 0,
            tool_count: 0,
            error_count: 0,
            omitted: false,
        },
        error_summary: None,
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
    SessionSnapshot {
        session_id: SessionId,
        cleanup_on_error: bool,
    },
}

fn ready(event: ServerEvent) -> PendingEvent {
    PendingEvent::Ready(event)
}

fn snapshot_after_ack(session_id: SessionId, cleanup_on_error: bool) -> Vec<PendingEvent> {
    vec![
        ready(ServerEvent::RequestAck),
        PendingEvent::SessionSnapshot {
            session_id,
            cleanup_on_error,
        },
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

fn image_chunk_event(chunk: ImageAssetChunk) -> ServerEvent {
    ServerEvent::ImageChunk {
        image_ref: chunk.id,
        mime_type: chunk.mime_type,
        offset: chunk.offset,
        total_size: chunk.total_size,
        eof: chunk.eof,
        data: STANDARD.encode(chunk.data),
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
    #[error("authenticated device is no longer authorized")]
    DeviceRevoked,
    #[error("attachment transfer is invalid: {0}")]
    InvalidAttachment(&'static str),
    #[error(transparent)]
    ImageAsset(#[from] ImageAssetError),
    #[error("connection did not declare capability {0}")]
    MissingCapability(&'static str),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Runtime(#[from] RuntimeManagerError),
    #[error(transparent)]
    PiBridge(#[from] PiBridgeError),
    #[error(transparent)]
    History(#[from] HistoryError),
}

impl DispatchError {
    #[allow(clippy::too_many_lines)]
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
            Self::DeviceRevoked => (
                ErrorCode::Unauthorized,
                "Device authorization is no longer valid",
                false,
            ),
            Self::InvalidAttachment(message) => (ErrorCode::InvalidRequest, *message, false),
            Self::ImageAsset(_) => (
                ErrorCode::PiUnavailable,
                "Image asset is temporarily unavailable",
                true,
            ),
            Self::MissingCapability(_) => (
                ErrorCode::InvalidRequest,
                "This client did not declare the capability this request requires",
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
            Self::Runtime(RuntimeManagerError::TurnCapacity { .. }) => (
                ErrorCode::Capacity,
                "Concurrent turn capacity has been reached",
                true,
            ),
            Self::Runtime(RuntimeManagerError::TuiOwned(_)) => (
                ErrorCode::Conflict,
                "Session is owned by a local Pi TUI",
                true,
            ),
            Self::Runtime(RuntimeManagerError::TuiUnsupportedCommand(_)) => (
                ErrorCode::InvalidRequest,
                "This operation is not supported for a local Pi TUI session",
                false,
            ),
            Self::Runtime(RuntimeManagerError::TuiCommandRejected(_)) => (
                ErrorCode::Conflict,
                "The local Pi TUI rejected this operation in its current state",
                true,
            ),
            Self::Runtime(RuntimeManagerError::TuiUnavailable(_)) => (
                ErrorCode::PiUnavailable,
                "The local Pi TUI bridge is temporarily unreachable",
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
            Self::History(HistoryError::InvalidCursor | HistoryError::InvalidLimit) => (
                ErrorCode::InvalidRequest,
                "Session history cursor or page size is invalid",
                false,
            ),
            Self::History(HistoryError::MessageTooLarge(_)) => (
                ErrorCode::PiUnavailable,
                "A session history message is too large to display",
                false,
            ),
            Self::History(_) => (
                ErrorCode::PiUnavailable,
                "Session history is temporarily unavailable",
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

#[cfg(test)]
mod tui_snapshot_tests {
    use std::collections::HashSet;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use tempfile::tempdir;

    use super::HostProtocolDispatcher;
    use crate::host_dispatcher::HostState;
    use crate::host_environment::HostEnvironment;
    use crate::runtime_manager::{RuntimeManager, RuntimeManagerOptions};
    use crate::session_lock::{ProcessIdentity, SessionId};
    use crate::tui_bridge::{
        TuiBridgeEventFrame, TuiBridgeHarness, TuiBridgeRequestFrame, TuiBridgeResponseFrame,
        TuiBridgeSnapshot, owner_uid,
    };
    use crate::{HostConfig, WorkspaceRegistry};
    use pix_wire::{ServerEvent, SessionState};

    #[allow(clippy::too_many_lines)]
    #[test]
    fn tui_snapshot_discards_events_through_cursor_and_keeps_later_events() {
        let workspace = tempdir().expect("workspace");
        let locks = tempdir().expect("locks");
        let mut config = HostConfig::new("TUI snapshot test");
        WorkspaceRegistry::new(&mut config)
            .add(workspace.path(), Some("Project".to_owned()))
            .expect("authorize workspace");
        let manager = Arc::new(
            RuntimeManager::new(RuntimeManagerOptions {
                executable: workspace.path().join("unused-pi"),
                lock_directory: locks.path().to_path_buf(),
                max_active_sessions: 2,
                max_concurrent_turns: 2,
                idle_timeout: Duration::from_secs(30),
                request_timeout: Duration::from_secs(2),
                extra_arguments: Vec::new(),
                environment: HostEnvironment::from_process(),
            })
            .expect("runtime manager"),
        );
        let mut authorized = HashSet::new();
        authorized.insert(workspace.path().to_path_buf());
        manager.configure_tui_bridge(authorized, owner_uid(workspace.path()));
        let session_id = SessionId::new();
        let peer = crate::tui_bridge::TuiBridgePeer::new(
            owner_uid(workspace.path()).expect("workspace owner"),
            ProcessIdentity::current().expect("current process"),
        );
        let harness = TuiBridgeHarness::new(manager.tui_bridge());
        let register = serde_json::to_vec(&crate::tui_bridge::TuiBridgeRegister::new(
            session_id,
            workspace.path(),
            uuid::Uuid::new_v4(),
        ))
        .expect("register frame");
        let registration = harness
            .register_frame(&register, &peer)
            .expect("register TUI");
        let (outbound, requests) = mpsc::sync_channel(1);
        let _broker = manager
            .tui_bridge()
            .bind_transport(&registration.token, outbound)
            .expect("bind bridge transport");
        let mut dispatcher =
            HostProtocolDispatcher::new(Arc::new(HostState::new(config)), Arc::clone(&manager));
        dispatcher.attached_sessions.insert(session_id);
        dispatcher.event_receivers.insert(
            session_id,
            manager.subscribe(session_id).expect("subscribe TUI"),
        );
        let first = TuiBridgeEventFrame::new(
            session_id,
            registration.token.bridge_instance_id,
            1,
            "agent_start",
            serde_json::json!({}),
        );
        manager
            .tui_bridge()
            .publish_event(&registration.token, &first)
            .expect("publish first event");
        let snapshot_registry = Arc::clone(&manager.tui_bridge());
        let snapshot_token = registration.token.clone();
        let response_thread = std::thread::spawn(move || {
            let request = requests
                .recv_timeout(Duration::from_secs(1))
                .expect("snapshot request");
            let request =
                serde_json::from_slice::<TuiBridgeRequestFrame>(&request[..request.len() - 1])
                    .expect("decode snapshot request");
            snapshot_registry
                .resolve_snapshot_response(
                    &snapshot_token,
                    TuiBridgeResponseFrame {
                        version: 1,
                        message_type: "response".to_owned(),
                        request_id: request.request_id,
                        session_id: session_id.to_string(),
                        command: "snapshot".to_owned(),
                        success: true,
                        snapshot: Some(TuiBridgeSnapshot {
                            session_id: session_id.to_string(),
                            session_name: Some("TUI".to_owned()),
                            model: None,
                            thinking_level: "high".to_owned(),
                            is_streaming: true,
                            is_compacting: false,
                            pending_message_count: 0,
                            messages: vec![serde_json::json!({
                                "role": "user",
                                "content": "hi"
                            })],
                            inflight_assistant: None,
                            active_tools: Vec::new(),
                            through_sequence: 1,
                        }),
                        result: None,
                        error: None,
                    },
                )
                .expect("resolve snapshot");
        });
        let snapshot = dispatcher
            .session_snapshot_event(session_id)
            .expect("TUI snapshot");
        response_thread.join().expect("snapshot response thread");
        let ServerEvent::SessionSnapshot { snapshot } = snapshot else {
            panic!("expected session snapshot");
        };
        assert_eq!(snapshot.state, SessionState::Running);
        assert_eq!(snapshot.through_sequence, Some(1));
        let second = TuiBridgeEventFrame::new(
            session_id,
            registration.token.bridge_instance_id,
            2,
            "agent_settled",
            serde_json::json!({}),
        );
        manager
            .tui_bridge()
            .publish_event(&registration.token, &second)
            .expect("publish second event");
        let events = dispatcher.drain_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].event,
            ServerEvent::SessionState {
                state: SessionState::Idle,
                ..
            }
        ));
        assert!(!events.iter().any(|event| matches!(
            &event.event,
            ServerEvent::SessionState {
                state: SessionState::Running,
                ..
            }
        )));
    }
}
