use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError, mpsc};
use std::time::{Duration, Instant};

use pix_wire::{HostModelDefaults, SessionQueue, SessionState};
use thiserror::Error;

use crate::host_environment::HostEnvironment;
use crate::pi_rpc::{PiCommand, PiEvent, PiResponse, RpcError};
use crate::runtime::{PiRuntime, PiRuntimeOptions, RuntimeError, SessionLaunch};
use crate::session::{DiscoveredSession, SessionError, SessionSnapshot};
use crate::session_lock::{RecoveredSessionOwner, SessionId, SessionRecoveryState};
use crate::tui_bridge::{TuiBridgeConnectionState, TuiBridgeError, TuiBridgeRegistry};

const RUNTIME_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const AUTHORIZATION_REVOCATION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRuntimeSummary {
    pub session_id: SessionId,
    pub workspace: PathBuf,
    pub client_count: usize,
    pub completed: bool,
    pub state: SessionState,
    pub backend: RuntimeBackend,
}

impl ActiveRuntimeSummary {
    #[must_use]
    pub const fn state_name(&self) -> &'static str {
        match self.state {
            SessionState::Sleeping => "sleeping",
            SessionState::Starting => "starting",
            SessionState::Idle => "idle",
            SessionState::Running => "running",
            SessionState::Compacting => "compacting",
            SessionState::Unavailable => "unavailable",
        }
    }
}

/// Identifies which local runtime owns an active session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBackend {
    Rpc,
    Tui,
}

impl RuntimeBackend {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Tui => "tui",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeManagerOptions {
    pub executable: PathBuf,
    pub lock_directory: PathBuf,
    /// Maximum number of resident Pi child processes.
    pub max_active_sessions: usize,
    /// Maximum number of sessions with an accepted turn in flight.
    pub max_concurrent_turns: usize,
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
    pub extra_arguments: Vec<String>,
    /// Environment every Pi child is spawned in.
    pub environment: HostEnvironment,
}

impl RuntimeManagerOptions {
    /// Validates runtime limits before the manager starts accepting sessions.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] when no runtime or turn slot is allowed.
    pub fn validate(&self) -> Result<(), RuntimeManagerError> {
        if self.max_active_sessions == 0 {
            return Err(RuntimeManagerError::InvalidLimit);
        }
        if self.max_concurrent_turns == 0 {
            return Err(RuntimeManagerError::InvalidTurnLimit);
        }
        Ok(())
    }
}

struct ManagedRuntime {
    runtime: Arc<PiRuntime>,
    operation: Arc<Mutex<()>>,
    workspace: PathBuf,
    client_count: usize,
    last_used: Instant,
    completed: bool,
    phase: RuntimePhase,
    /// Last `queue_update` contents. Ephemeral reconnect state: it dies with
    /// the runtime and never reaches durable storage.
    queue: Option<SessionQueue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePhase {
    Starting,
    Idle,
    Running,
    Compacting,
    Unavailable,
}

impl RuntimePhase {
    const fn session_state(self) -> SessionState {
        match self {
            Self::Starting => SessionState::Starting,
            Self::Idle => SessionState::Idle,
            Self::Running => SessionState::Running,
            Self::Compacting => SessionState::Compacting,
            Self::Unavailable => SessionState::Unavailable,
        }
    }
}

/// Owns all active Pi children and enforces host runtime limits.
pub struct RuntimeManager {
    options: RuntimeManagerOptions,
    runtimes: Mutex<HashMap<SessionId, ManagedRuntime>>,
    tui_bridge: Arc<TuiBridgeRegistry>,
    turns: Mutex<HashSet<SessionId>>,
    lifecycle: Mutex<()>,
}

impl RuntimeManager {
    /// Creates an empty runtime manager.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] for invalid configured limits.
    pub fn new(options: RuntimeManagerOptions) -> Result<Self, RuntimeManagerError> {
        options.validate()?;
        Ok(Self {
            tui_bridge: Arc::new(TuiBridgeRegistry::new(options.lock_directory.clone())),
            options,
            runtimes: Mutex::new(HashMap::new()),
            turns: Mutex::new(HashSet::new()),
            lifecycle: Mutex::new(()),
        })
    }

    /// Configures the optional TUI bridge authorization view.  The normal
    /// host calls this before restoring ownership records and before accepting
    /// protocol requests; tests may configure a smaller isolated view.
    pub fn configure_tui_bridge(
        &self,
        authorized_workspaces: HashSet<PathBuf>,
        expected_peer_uid: Option<u32>,
    ) {
        self.tui_bridge
            .configure_authorization(authorized_workspaces, expected_peer_uid);
    }

    /// Returns the host-local TUI registry used by the bridge transport and
    /// deterministic harnesses.
    #[must_use]
    pub fn tui_bridge(&self) -> Arc<TuiBridgeRegistry> {
        Arc::clone(&self.tui_bridge)
    }

    /// Reinstates a live `PiTui` owner found during the startup recovery barrier.
    /// The registry holds the external lease so a concurrent RPC `open` cannot
    /// turn the recovered owner into Sleeping.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] when the durable owner cannot be
    /// revalidated or its lock cannot be held by this host.
    pub fn restore_tui_owner(
        &self,
        owner: &RecoveredSessionOwner,
        workspace: &Path,
    ) -> Result<(), RuntimeManagerError> {
        if owner.state != SessionRecoveryState::TuiUnreachable {
            return Ok(());
        }
        self.tui_bridge.restore(&owner.record, workspace)?;
        Ok(())
    }

    /// Reads Pi's persisted model preferences without starting a child
    /// process. Draft sessions use this to mirror Pi's last selected model.
    #[must_use]
    pub fn pi_model_defaults(&self) -> HostModelDefaults {
        crate::pi_defaults::discover(&self.options.environment)
    }

    /// Starts a new native Pi session with one attached client.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] when capacity cannot be freed, workspace
    /// validation fails, or Pi cannot start.
    pub fn create(
        &self,
        workspace: impl AsRef<Path>,
        name: Option<String>,
    ) -> Result<SessionId, RuntimeManagerError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = SessionId::new();
        self.start(workspace.as_ref(), SessionLaunch::Create { id, name })?;
        Ok(id)
    }

    /// Starts or reuses a discovered native Pi session with one attached client.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] if the discovered file is outside its
    /// session directory, capacity cannot be freed, or Pi cannot start.
    pub fn open(
        &self,
        workspace: impl AsRef<Path>,
        session_directory: impl AsRef<Path>,
        session: &DiscoveredSession,
    ) -> Result<SessionId, RuntimeManagerError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = session.summary.id;
        {
            let mut runtimes = self
                .runtimes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(managed) = runtimes.get_mut(&id) {
                managed.client_count = managed.client_count.saturating_add(1);
                managed.last_used = Instant::now();
                return Ok(id);
            }
        }
        self.reject_tui_owner(id)?;
        let directory = std::fs::canonicalize(session_directory.as_ref()).map_err(|source| {
            RuntimeManagerError::Canonicalize {
                path: session_directory.as_ref().to_path_buf(),
                source,
            }
        })?;
        let path = std::fs::canonicalize(&session.path).map_err(|source| {
            RuntimeManagerError::Canonicalize {
                path: session.path.clone(),
                source,
            }
        })?;
        if !path.starts_with(&directory) {
            return Err(RuntimeManagerError::SessionOutsideDirectory { path, directory });
        }
        self.start(
            workspace.as_ref(),
            SessionLaunch::Existing {
                id,
                reference: path.display().to_string(),
            },
        )?;
        Ok(id)
    }

    /// Attaches another client to an already active session.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] when the session is not active.
    pub fn attach(&self, session_id: SessionId) -> Result<(), RuntimeManagerError> {
        let mut runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(managed) = runtimes.get_mut(&session_id) else {
            drop(runtimes);
            return Err(self.tui_owner_error(session_id));
        };
        managed.client_count = managed.client_count.saturating_add(1);
        managed.last_used = Instant::now();
        Ok(())
    }

    /// Sends a Pi operation to an active session.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] for unknown sessions or RPC failures.
    pub fn request(
        &self,
        session_id: SessionId,
        command: &PiCommand,
    ) -> Result<PiResponse, RuntimeManagerError> {
        self.request_with_timeout(session_id, command, self.options.request_timeout)
    }

    /// Sends a Pi operation with an operation-specific deadline. Optional
    /// session metadata uses a short timeout so a slow Pi extension can never
    /// hold up the base session snapshot or a phone connection.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] for unknown sessions or RPC failures.
    pub fn request_with_timeout(
        &self,
        session_id: SessionId,
        command: &PiCommand,
        timeout: Duration,
    ) -> Result<PiResponse, RuntimeManagerError> {
        let (runtime, operation) = self.runtime_and_operation(session_id)?;
        let _operation = try_lock_operation(&operation, session_id)?;
        let admitted = if is_turn_command(command) {
            self.try_admit_turn(session_id)?
        } else {
            false
        };
        let response = match runtime.rpc().request(command, timeout) {
            Ok(response) => response,
            Err(error) => {
                if admitted {
                    self.release_turn(session_id);
                }
                return Err(error.into());
            }
        };
        let mut runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turn_active = is_turn_command(command) && self.turn_is_active(session_id);
        let Some(managed) = runtimes.get_mut(&session_id) else {
            if admitted {
                self.release_turn(session_id);
            }
            return Err(RuntimeManagerError::NotActive(session_id));
        };
        managed.last_used = Instant::now();
        match command {
            PiCommand::Prompt { .. } | PiCommand::Steer { .. } | PiCommand::FollowUp { .. }
                if turn_active =>
            {
                managed.completed = false;
                managed.phase = RuntimePhase::Running;
            }
            PiCommand::Compact { .. } if turn_active => {
                managed.completed = false;
                managed.phase = RuntimePhase::Compacting;
            }
            _ => {}
        }
        Ok(response)
    }

    /// Subscribes to raw events from one active Pi runtime.
    ///
    /// Event interpretation remains in the Pi compatibility adapter.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] when the session is not active.
    pub fn subscribe(
        &self,
        session_id: SessionId,
    ) -> Result<mpsc::Receiver<PiEvent>, RuntimeManagerError> {
        Ok(self.active_runtime(session_id)?.rpc().subscribe())
    }

    /// Reads and records an authoritative snapshot from Pi.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] for unknown sessions, RPC failures, or
    /// incompatible Pi snapshot data.
    pub fn snapshot(&self, session_id: SessionId) -> Result<SessionSnapshot, RuntimeManagerError> {
        self.snapshot_with_timeout(session_id, self.options.request_timeout)
    }

    /// Reads the authoritative state and messages with a caller-selected
    /// deadline. History restoration is allowed more time than optional
    /// metadata, but it remains bounded so a dead Pi child cannot pin a
    /// connection forever.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] when the session is inactive, busy, or
    /// Pi does not return a valid snapshot before the deadline.
    pub fn snapshot_with_timeout(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> Result<SessionSnapshot, RuntimeManagerError> {
        let (runtime, operation) = self.runtime_and_operation(session_id)?;
        let _operation = try_lock_operation(&operation, session_id)?;
        let snapshot =
            SessionSnapshot::read(runtime.rpc(), timeout.min(self.options.request_timeout))?;
        let mut runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(managed) = runtimes.get_mut(&session_id) else {
            return Err(RuntimeManagerError::NotActive(session_id));
        };
        managed.phase = phase_for_snapshot(snapshot.is_streaming, snapshot.is_compacting);
        managed.completed = matches!(managed.phase, RuntimePhase::Idle);
        let completed = managed.completed;
        managed.last_used = Instant::now();
        drop(runtimes);
        if completed {
            self.release_turn(session_id);
        }
        Ok(snapshot)
    }

    /// Detaches one client without stopping an in-progress Pi task.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] when the session is unknown or has no
    /// attached client.
    pub fn detach(&self, session_id: SessionId) -> Result<(), RuntimeManagerError> {
        let mut runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(managed) = runtimes.get_mut(&session_id) else {
            drop(runtimes);
            return Err(self.tui_owner_error(session_id));
        };
        if managed.client_count == 0 {
            return Err(RuntimeManagerError::NoAttachedClient(session_id));
        }
        managed.client_count -= 1;
        managed.last_used = Instant::now();
        Ok(())
    }

    /// Stops an active session and releases its single-writer lease.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] if the session is unknown or Pi cannot
    /// be stopped cleanly.
    pub fn release(&self, session_id: SessionId) -> Result<(), RuntimeManagerError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.release_inner(session_id)
    }

    /// Stops every runtime whose canonical workspace is no longer authorized.
    ///
    /// Configuration refresh first replaces the shared authorization view, so
    /// new requests are denied immediately. This bounded retry then waits for
    /// any short in-flight RPC operation and terminates the underlying Pi
    /// process, ensuring an attached client cannot keep using a removed root.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] if a selected runtime remains busy or
    /// cannot be stopped.
    pub fn release_outside_workspaces(
        &self,
        authorized: &HashSet<PathBuf>,
    ) -> Result<Vec<SessionId>, RuntimeManagerError> {
        self.tui_bridge
            .mark_unavailable_if_workspace_not_authorized(authorized);
        let session_ids = self
            .active_sessions()
            .into_iter()
            .filter(|session| {
                !authorized.contains(&session.workspace)
                    && !self.tui_bridge.contains(session.session_id)
            })
            .map(|session| session.session_id)
            .collect::<Vec<_>>();
        let mut released = Vec::with_capacity(session_ids.len());
        let deadline = Instant::now() + AUTHORIZATION_REVOCATION_TIMEOUT;
        for session_id in session_ids {
            loop {
                match self.release(session_id) {
                    Ok(()) | Err(RuntimeManagerError::NotActive(_)) => {
                        released.push(session_id);
                        break;
                    }
                    Err(RuntimeManagerError::Busy(_)) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(released)
    }

    fn release_inner(&self, session_id: SessionId) -> Result<(), RuntimeManagerError> {
        let operation = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| Arc::clone(&managed.operation));
        let Some(operation) = operation else {
            return Err(self.tui_owner_error(session_id));
        };
        let _operation = try_lock_operation(&operation, session_id)?;
        let managed = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id)
            .ok_or(RuntimeManagerError::NotActive(session_id))?;
        let runtime = Arc::try_unwrap(managed.runtime).map_err(|runtime| {
            self.runtimes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(session_id, ManagedRuntime { runtime, ..managed });
            RuntimeManagerError::Busy(session_id)
        })?;
        self.release_turn(session_id);
        runtime.stop()?;
        Ok(())
    }

    /// Stops completed, client-free runtimes older than the idle timeout.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] if a selected child cannot be stopped.
    pub fn sweep_idle(&self) -> Result<Vec<SessionId>, RuntimeManagerError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let idle_timeout = self.options.idle_timeout;
        let candidates = {
            let runtimes = self
                .runtimes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut candidates = runtimes
                .iter()
                .filter(|(_, managed)| {
                    managed.client_count == 0
                        && now.duration_since(managed.last_used) >= idle_timeout
                })
                .map(|(id, managed)| (*id, managed.last_used))
                .collect::<Vec<_>>();
            candidates.sort_by_key(|(_, last_used)| *last_used);
            candidates.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
        };
        let mut released = Vec::new();
        let mut first_error = None;
        for id in candidates {
            match self.refresh_completed_with_timeout(id, self.probe_timeout()) {
                Ok(true) => match self.release_inner(id) {
                    Ok(()) => released.push(id),
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                },
                Ok(false) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if released.is_empty()
            && let Some(error) = first_error
        {
            return Err(error);
        }
        Ok(released)
    }

    /// Removes runtimes whose Pi child has already exited.
    ///
    /// The RPC client broadcasts the terminal `Closed` event before it is
    /// dropped, so attached clients still receive an `Unavailable` state while
    /// the manager releases the process lease and registry entry.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] if the child cannot be inspected or a
    /// runtime cannot be released cleanly.
    pub fn reap_exited(&self) -> Result<Vec<SessionId>, RuntimeManagerError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let exited = {
            let runtimes = self
                .runtimes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut exited = Vec::new();
            for (id, managed) in runtimes.iter() {
                if managed.runtime.try_wait()?.is_some() {
                    exited.push(*id);
                }
            }
            exited
        };
        let mut reaped = Vec::new();
        for id in exited {
            match self.release_inner(id) {
                Ok(()) => reaped.push(id),
                Err(RuntimeManagerError::NotActive(_) | RuntimeManagerError::Busy(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(reaped)
    }

    #[must_use]
    pub fn active_sessions(&self) -> Vec<ActiveRuntimeSummary> {
        let mut sessions = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(id, managed)| ActiveRuntimeSummary {
                session_id: *id,
                workspace: managed.workspace.clone(),
                client_count: managed.client_count,
                completed: managed.completed,
                state: managed.phase.session_state(),
                backend: RuntimeBackend::Rpc,
            })
            .collect::<Vec<_>>();
        sessions.extend(
            self.tui_bridge
                .owners()
                .into_iter()
                .map(|owner| ActiveRuntimeSummary {
                    session_id: owner.token.session_id,
                    workspace: owner.workspace,
                    client_count: owner.client_count,
                    completed: matches!(owner.session_state, SessionState::Idle),
                    state: owner.session_state,
                    backend: RuntimeBackend::Tui,
                }),
        );
        sessions
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_active(&self, session_id: SessionId) -> bool {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&session_id)
            || self.tui_bridge.contains(session_id)
    }

    #[must_use]
    pub fn is_completed(&self, session_id: SessionId) -> Option<bool> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| managed.completed)
            .or_else(|| {
                self.tui_bridge
                    .owner(session_id)
                    .map(|owner| matches!(owner.session_state, SessionState::Idle))
            })
    }

    /// Returns the current wire-compatible state for an active runtime.
    #[must_use]
    pub fn session_state(&self, session_id: SessionId) -> Option<SessionState> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| managed.phase.session_state())
            .or_else(|| {
                self.tui_bridge
                    .owner(session_id)
                    .map(|owner| owner.session_state)
            })
    }

    /// Refreshes an active runtime from Pi before a detached session is shown
    /// in a catalog. A failed refresh is returned to the caller so it can keep
    /// the previous in-memory state without blocking the whole session list.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeManagerError`] if the session is inactive or Pi does
    /// not return a usable state response within the probe timeout.
    pub fn refresh_state(
        &self,
        session_id: SessionId,
    ) -> Result<SessionState, RuntimeManagerError> {
        if let Some(owner) = self.tui_bridge.owner(session_id) {
            return Ok(owner.session_state);
        }
        self.refresh_completed_with_timeout(session_id, self.probe_timeout())?;
        self.session_state(session_id)
            .ok_or(RuntimeManagerError::NotActive(session_id))
    }

    /// Records an authoritative state received from the Pi compatibility
    /// bridge. This is intentionally in-memory; Pi JSONL remains durable truth.
    pub fn mark_state(&self, session_id: SessionId, state: SessionState) {
        {
            if let Some(managed) = self
                .runtimes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(&session_id)
            {
                managed.phase = match state {
                    SessionState::Sleeping | SessionState::Idle => RuntimePhase::Idle,
                    SessionState::Starting => RuntimePhase::Starting,
                    SessionState::Running => RuntimePhase::Running,
                    SessionState::Compacting => RuntimePhase::Compacting,
                    SessionState::Unavailable => RuntimePhase::Unavailable,
                };
                managed.completed = matches!(managed.phase, RuntimePhase::Idle);
                managed.last_used = Instant::now();
            }
        }
        if !self.is_active(session_id) {
            return;
        }
        if self.tui_bridge.contains(session_id) {
            let _ = self.tui_bridge.mark_state(session_id, state);
        }
        if matches!(
            state,
            SessionState::Idle | SessionState::Sleeping | SessionState::Unavailable
        ) {
            self.release_turn(session_id);
        }
    }

    /// Records completion state derived by the Pi compatibility bridge.
    pub fn mark_completed(&self, session_id: SessionId, completed: bool) {
        self.mark_state(
            session_id,
            if completed {
                SessionState::Idle
            } else {
                SessionState::Running
            },
        );
    }

    /// Records the latest steering and follow-up queue reported by Pi so a
    /// reconnecting client can recover queue text without a Pi round trip.
    pub fn record_queue(&self, session_id: SessionId, queue: SessionQueue) {
        if let Some(managed) = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&session_id)
        {
            managed.queue = Some(queue);
        }
    }

    /// Returns the last recorded queue for an active runtime.
    #[must_use]
    pub fn queue(&self, session_id: SessionId) -> Option<SessionQueue> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .and_then(|managed| managed.queue.clone())
    }

    #[must_use]
    pub fn client_count(&self, session_id: SessionId) -> Option<usize> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| managed.client_count)
            .or_else(|| {
                self.tui_bridge
                    .owner(session_id)
                    .map(|owner| owner.client_count)
            })
    }

    #[must_use]
    pub fn workspace(&self, session_id: SessionId) -> Option<PathBuf> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| managed.workspace.clone())
            .or_else(|| {
                self.tui_bridge
                    .owner(session_id)
                    .map(|owner| owner.workspace)
            })
    }

    fn start(&self, workspace: &Path, launch: SessionLaunch) -> Result<(), RuntimeManagerError> {
        self.reject_tui_owner(launch.id())?;
        self.make_capacity()?;
        let workspace = std::fs::canonicalize(workspace).map_err(|source| {
            RuntimeManagerError::Canonicalize {
                path: workspace.to_path_buf(),
                source,
            }
        })?;
        let id = launch.id();
        let runtime = Arc::new(PiRuntime::start(&PiRuntimeOptions {
            executable: self.options.executable.clone(),
            workspace: workspace.clone(),
            lock_directory: self.options.lock_directory.clone(),
            launch,
            extra_arguments: self.options.extra_arguments.clone(),
            environment: self.options.environment.clone(),
        })?);
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id,
                ManagedRuntime {
                    runtime,
                    operation: Arc::new(Mutex::new(())),
                    workspace,
                    client_count: 1,
                    last_used: Instant::now(),
                    completed: false,
                    phase: RuntimePhase::Starting,
                    queue: None,
                },
            );
        Ok(())
    }

    fn make_capacity(&self) -> Result<(), RuntimeManagerError> {
        let candidates = {
            let runtimes = self
                .runtimes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if runtimes.len() < self.options.max_active_sessions {
                return Ok(());
            }
            let mut candidates = runtimes
                .iter()
                .filter(|(_, managed)| managed.client_count == 0)
                .map(|(id, managed)| (*id, managed.last_used))
                .collect::<Vec<_>>();
            candidates.sort_by_key(|(_, last_used)| *last_used);
            candidates.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
        };
        for candidate in candidates {
            if self
                .refresh_completed_with_timeout(candidate, self.probe_timeout())
                .unwrap_or(false)
                && self.release_inner(candidate).is_ok()
            {
                return Ok(());
            }
        }
        Err(RuntimeManagerError::Capacity {
            limit: self.options.max_active_sessions,
        })
    }

    fn active_runtime(&self, session_id: SessionId) -> Result<Arc<PiRuntime>, RuntimeManagerError> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| Arc::clone(&managed.runtime))
            .ok_or_else(|| self.tui_owner_error(session_id))
    }

    fn try_admit_turn(&self, session_id: SessionId) -> Result<bool, RuntimeManagerError> {
        let mut turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if turns.contains(&session_id) {
            return Ok(false);
        }
        if turns.len() >= self.options.max_concurrent_turns {
            return Err(RuntimeManagerError::TurnCapacity {
                limit: self.options.max_concurrent_turns,
            });
        }
        turns.insert(session_id);
        Ok(true)
    }

    fn release_turn(&self, session_id: SessionId) {
        self.turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    fn turn_is_active(&self, session_id: SessionId) -> bool {
        self.turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&session_id)
    }

    fn runtime_and_operation(
        &self,
        session_id: SessionId,
    ) -> Result<(Arc<PiRuntime>, Arc<Mutex<()>>), RuntimeManagerError> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| (Arc::clone(&managed.runtime), Arc::clone(&managed.operation)))
            .ok_or_else(|| self.tui_owner_error(session_id))
    }

    fn reject_tui_owner(&self, session_id: SessionId) -> Result<(), RuntimeManagerError> {
        if self.tui_bridge.contains(session_id) {
            Err(self.tui_owner_error(session_id))
        } else {
            Ok(())
        }
    }

    fn tui_owner_error(&self, session_id: SessionId) -> RuntimeManagerError {
        match self.tui_bridge.owner(session_id).map(|owner| owner.state) {
            Some(TuiBridgeConnectionState::Unreachable) => {
                RuntimeManagerError::TuiUnavailable(session_id)
            }
            Some(TuiBridgeConnectionState::Attached) => RuntimeManagerError::TuiOwned(session_id),
            None => RuntimeManagerError::NotActive(session_id),
        }
    }

    fn refresh_completed_with_timeout(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> Result<bool, RuntimeManagerError> {
        let (runtime, operation) = self.runtime_and_operation(session_id)?;
        let _operation = try_lock_operation(&operation, session_id)?;
        let response = runtime.rpc().request(&PiCommand::GetState, timeout)?;
        let state = response
            .data
            .ok_or(RpcError::MissingResponseData("get_state"))?;
        let is_streaming = state
            .get("isStreaming")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        let is_compacting = state
            .get("isCompacting")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        let phase = phase_for_snapshot(is_streaming, is_compacting);
        let completed = matches!(phase, RuntimePhase::Idle);
        if let Some(managed) = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&session_id)
        {
            managed.phase = phase;
            managed.completed = completed;
            managed.last_used = Instant::now();
        }
        if completed {
            self.release_turn(session_id);
        }
        Ok(completed)
    }

    fn probe_timeout(&self) -> Duration {
        self.options.request_timeout.min(RUNTIME_PROBE_TIMEOUT)
    }
}

fn phase_for_snapshot(is_streaming: bool, is_compacting: bool) -> RuntimePhase {
    if is_compacting {
        RuntimePhase::Compacting
    } else if is_streaming {
        RuntimePhase::Running
    } else {
        RuntimePhase::Idle
    }
}

fn is_turn_command(command: &PiCommand) -> bool {
    matches!(
        command,
        PiCommand::Prompt { .. }
            | PiCommand::Steer { .. }
            | PiCommand::FollowUp { .. }
            | PiCommand::Compact { .. }
    )
}

fn try_lock_operation(
    operation: &Mutex<()>,
    session_id: SessionId,
) -> Result<MutexGuard<'_, ()>, RuntimeManagerError> {
    match operation.try_lock() {
        Ok(guard) => Ok(guard),
        Err(TryLockError::Poisoned(error)) => Ok(error.into_inner()),
        Err(TryLockError::WouldBlock) => Err(RuntimeManagerError::Busy(session_id)),
    }
}

#[derive(Debug, Error)]
pub enum RuntimeManagerError {
    #[error("max_active_sessions must be greater than zero")]
    InvalidLimit,
    #[error("max_concurrent_turns must be greater than zero")]
    InvalidTurnLimit,
    #[error("active Pi session limit {limit} reached and no idle runtime can be stopped")]
    Capacity { limit: usize },
    #[error("concurrent Pi turn limit {limit} reached")]
    TurnCapacity { limit: usize },
    #[error("Pi session is not active: {0}")]
    NotActive(SessionId),
    #[error("session is owned by a local Pi TUI: {0}")]
    TuiOwned(SessionId),
    #[error("local Pi TUI bridge is unreachable: {0}")]
    TuiUnavailable(SessionId),
    #[error("Pi session has no attached client: {0}")]
    NoAttachedClient(SessionId),
    #[error("Pi session still has an operation in flight: {0}")]
    Busy(SessionId),
    #[error("failed to canonicalize {path}: {source}")]
    Canonicalize { path: PathBuf, source: io::Error },
    #[error("session file {path} is outside session directory {directory}")]
    SessionOutsideDirectory { path: PathBuf, directory: PathBuf },
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    TuiBridge(#[from] TuiBridgeError),
}
