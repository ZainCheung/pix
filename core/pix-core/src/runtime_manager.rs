use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use pix_wire::HostModelDefaults;
use thiserror::Error;

use crate::host_environment::HostEnvironment;
use crate::pi_rpc::{PiCommand, PiEvent, PiResponse, RpcError};
use crate::runtime::{PiRuntime, PiRuntimeOptions, RuntimeError, SessionLaunch};
use crate::session::{DiscoveredSession, SessionError, SessionSnapshot};
use crate::session_lock::SessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRuntimeSummary {
    pub session_id: SessionId,
    pub workspace: PathBuf,
    pub client_count: usize,
    pub completed: bool,
}

#[derive(Debug, Clone)]
pub struct RuntimeManagerOptions {
    pub executable: PathBuf,
    pub lock_directory: PathBuf,
    pub max_active_sessions: usize,
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
    /// Returns [`RuntimeManagerError`] when no active session slot is allowed.
    pub fn validate(&self) -> Result<(), RuntimeManagerError> {
        if self.max_active_sessions == 0 {
            return Err(RuntimeManagerError::InvalidLimit);
        }
        Ok(())
    }
}

struct ManagedRuntime {
    runtime: Arc<PiRuntime>,
    workspace: PathBuf,
    client_count: usize,
    last_used: Instant,
    completed: bool,
}

/// Owns all active Pi children and enforces host runtime limits.
pub struct RuntimeManager {
    options: RuntimeManagerOptions,
    runtimes: Mutex<HashMap<SessionId, ManagedRuntime>>,
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
            options,
            runtimes: Mutex::new(HashMap::new()),
            lifecycle: Mutex::new(()),
        })
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
        self.start(
            workspace.as_ref(),
            SessionLaunch::Create { id, name },
        )?;
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
        let managed = runtimes
            .get_mut(&session_id)
            .ok_or(RuntimeManagerError::NotActive(session_id))?;
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
        let runtime = self.active_runtime(session_id)?;
        let response = runtime
            .rpc()
            .request(command, self.options.request_timeout)?;
        let mut runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(managed) = runtimes.get_mut(&session_id) else {
            return Err(RuntimeManagerError::NotActive(session_id));
        };
        managed.last_used = Instant::now();
        if matches!(
            command,
            PiCommand::Prompt { .. }
                | PiCommand::Steer { .. }
                | PiCommand::FollowUp { .. }
                | PiCommand::Compact { .. }
        ) {
            managed.completed = false;
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
        let runtime = self.active_runtime(session_id)?;
        let snapshot = SessionSnapshot::read(runtime.rpc(), self.options.request_timeout)?;
        let mut runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(managed) = runtimes.get_mut(&session_id) else {
            return Err(RuntimeManagerError::NotActive(session_id));
        };
        managed.completed = !snapshot.is_streaming && !snapshot.is_compacting;
        managed.last_used = Instant::now();
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
        let managed = runtimes
            .get_mut(&session_id)
            .ok_or(RuntimeManagerError::NotActive(session_id))?;
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

    fn release_inner(&self, session_id: SessionId) -> Result<(), RuntimeManagerError> {
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
            runtimes
                .iter()
                .filter(|(_, managed)| {
                    managed.client_count == 0
                        && now.duration_since(managed.last_used) >= idle_timeout
                })
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
        };
        let mut released = Vec::new();
        for id in candidates {
            if self.refresh_completed(id)? {
                self.release_inner(id)?;
                released.push(id);
            }
        }
        Ok(released)
    }

    #[must_use]
    pub fn active_sessions(&self) -> Vec<ActiveRuntimeSummary> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(id, managed)| ActiveRuntimeSummary {
                session_id: *id,
                workspace: managed.workspace.clone(),
                client_count: managed.client_count,
                completed: managed.completed,
            })
            .collect()
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
    }

    #[must_use]
    pub fn is_completed(&self, session_id: SessionId) -> Option<bool> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| managed.completed)
    }

    /// Records completion state derived by the Pi compatibility bridge.
    pub fn mark_completed(&self, session_id: SessionId, completed: bool) {
        if let Some(managed) = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&session_id)
        {
            managed.completed = completed;
            managed.last_used = Instant::now();
        }
    }

    #[must_use]
    pub fn client_count(&self, session_id: SessionId) -> Option<usize> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| managed.client_count)
    }

    #[must_use]
    pub fn workspace(&self, session_id: SessionId) -> Option<PathBuf> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| managed.workspace.clone())
    }

    fn start(&self, workspace: &Path, launch: SessionLaunch) -> Result<(), RuntimeManagerError> {
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
                    workspace,
                    client_count: 1,
                    last_used: Instant::now(),
                    completed: true,
                },
            );
        Ok(())
    }

    fn make_capacity(&self) -> Result<(), RuntimeManagerError> {
        let candidate = {
            let runtimes = self
                .runtimes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if runtimes.len() < self.options.max_active_sessions {
                return Ok(());
            }
            runtimes
                .iter()
                .filter(|(_, managed)| managed.client_count == 0 && managed.completed)
                .min_by_key(|(_, managed)| managed.last_used)
                .map(|(id, _)| *id)
        };
        let Some(candidate) = candidate else {
            return Err(RuntimeManagerError::Capacity {
                limit: self.options.max_active_sessions,
            });
        };
        if !self.refresh_completed(candidate)? {
            return Err(RuntimeManagerError::Capacity {
                limit: self.options.max_active_sessions,
            });
        }
        self.release_inner(candidate)
    }

    fn active_runtime(&self, session_id: SessionId) -> Result<Arc<PiRuntime>, RuntimeManagerError> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(|managed| Arc::clone(&managed.runtime))
            .ok_or(RuntimeManagerError::NotActive(session_id))
    }

    fn refresh_completed(&self, session_id: SessionId) -> Result<bool, RuntimeManagerError> {
        let runtime = self.active_runtime(session_id)?;
        let response = runtime
            .rpc()
            .request(&PiCommand::GetState, self.options.request_timeout)?;
        let state = response
            .data
            .ok_or(RpcError::MissingResponseData("get_state"))?;
        let completed = state
            .get("isStreaming")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
            && state
                .get("isCompacting")
                .and_then(serde_json::Value::as_bool)
                == Some(false);
        if let Some(managed) = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&session_id)
        {
            managed.completed = completed;
        }
        Ok(completed)
    }
}

#[derive(Debug, Error)]
pub enum RuntimeManagerError {
    #[error("max_active_sessions must be greater than zero")]
    InvalidLimit,
    #[error("active Pi session limit {limit} reached and no completed idle runtime can be stopped")]
    Capacity { limit: usize },
    #[error("Pi session is not active: {0}")]
    NotActive(SessionId),
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
}
