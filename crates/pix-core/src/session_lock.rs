use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::Builder as TempFileBuilder;
use thiserror::Error;
use uuid::Uuid;

pub const SESSION_LOCK_RECORD_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl SessionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOwnerKind {
    #[default]
    PixRpc,
    PiTui,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub process_start_identity: String,
}

impl ProcessIdentity {
    /// Resolves the identity of the current process.
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] if the operating system cannot expose the
    /// process start identity or the current process cannot be found.
    pub fn current() -> Result<Self, SessionLockError> {
        let pid = std::process::id();
        let process_start_identity =
            process_start_identity(pid)?.ok_or(SessionLockError::CurrentProcessNotFound(pid))?;
        Ok(Self {
            pid,
            process_start_identity,
        })
    }

    /// Resolves a process identity without treating an absent PID as an error.
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] if process inspection fails.
    pub fn inspect(pid: u32) -> Result<Option<Self>, SessionLockError> {
        Ok(
            process_start_identity(pid)?.map(|process_start_identity| Self {
                pid,
                process_start_identity,
            }),
        )
    }
}

/// Durable ownership metadata. The advisory lock itself lives in the adjacent
/// `.lock` file; this record is stored in `<session-key>.owner.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLockRecord {
    pub version: u32,
    pub owner_kind: SessionOwnerKind,
    pub session_id: SessionId,
    pub workspace_fingerprint: Option<String>,
    pub owner_pid: u32,
    pub owner_process_start_identity: String,
    pub writer_pid: Option<u32>,
    pub writer_process_start_identity: Option<String>,
    pub lease_holder_pid: u32,
    pub lease_holder_process_start_identity: String,
    pub bridge_instance_id: Option<Uuid>,
    pub generation: u64,
    pub claim_nonce: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SessionLockRecord {
    fn new(
        owner_kind: SessionOwnerKind,
        session_id: SessionId,
        workspace_fingerprint: Option<String>,
        owner: &ProcessIdentity,
        writer: Option<&ProcessIdentity>,
        bridge_instance_id: Option<Uuid>,
        lease_holder: &ProcessIdentity,
    ) -> Self {
        let now = Utc::now();
        Self {
            version: SESSION_LOCK_RECORD_VERSION,
            owner_kind,
            session_id,
            workspace_fingerprint,
            owner_pid: owner.pid,
            owner_process_start_identity: owner.process_start_identity.clone(),
            writer_pid: writer.map(|identity| identity.pid),
            writer_process_start_identity: writer
                .map(|identity| identity.process_start_identity.clone()),
            lease_holder_pid: lease_holder.pid,
            lease_holder_process_start_identity: lease_holder.process_start_identity.clone(),
            bridge_instance_id,
            generation: 1,
            claim_nonce: Uuid::new_v4(),
            created_at: now,
            updated_at: now,
        }
    }

    fn from_legacy(legacy: LegacySessionLockRecord) -> Self {
        Self {
            version: legacy.version,
            owner_kind: SessionOwnerKind::PixRpc,
            session_id: legacy.session_id,
            workspace_fingerprint: None,
            owner_pid: legacy.pid,
            owner_process_start_identity: legacy.process_start_identity.clone(),
            writer_pid: None,
            writer_process_start_identity: None,
            lease_holder_pid: legacy.pid,
            lease_holder_process_start_identity: legacy.process_start_identity,
            bridge_instance_id: None,
            generation: 0,
            claim_nonce: Uuid::nil(),
            created_at: legacy.created_at,
            updated_at: legacy.created_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacySessionLockRecord {
    version: u32,
    pid: u32,
    process_start_identity: String,
    session_id: SessionId,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRecoveryState {
    TuiUnreachable,
    RpcOrphanSuspect,
    LegacyRpcOwned,
    Unauthorized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredSessionOwner {
    pub record: SessionLockRecord,
    pub state: SessionRecoveryState,
}

#[derive(Debug, Default)]
pub struct SessionLockRecovery {
    owners: Vec<RecoveredSessionOwner>,
    stale_cleared: usize,
    malformed: usize,
    unsupported: usize,
    blocked: usize,
}

impl SessionLockRecovery {
    #[must_use]
    pub fn owners(&self) -> &[RecoveredSessionOwner] {
        &self.owners
    }

    #[must_use]
    pub const fn stale_cleared(&self) -> usize {
        self.stale_cleared
    }

    #[must_use]
    pub const fn malformed(&self) -> usize {
        self.malformed
    }

    #[must_use]
    pub const fn unsupported(&self) -> usize {
        self.unsupported
    }

    #[must_use]
    pub const fn blocked(&self) -> usize {
        self.blocked
    }
}

/// Reads durable ownership records before Host accepts any session request.
/// The returned records are advisory state; every later claim revalidates the
/// process identity while holding the corresponding lock file.
pub struct SessionLockStore {
    lock_directory: PathBuf,
}

impl SessionLockStore {
    #[must_use]
    pub fn new(lock_directory: impl Into<PathBuf>) -> Self {
        Self {
            lock_directory: lock_directory.into(),
        }
    }

    #[must_use]
    pub fn lock_directory(&self) -> &Path {
        &self.lock_directory
    }

    /// Scans v2 owner records and legacy v1 records. Individual damaged or
    /// unsupported records are retained and counted so one bad session cannot
    /// make the rest of the Host unavailable.
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] for an unreadable lock directory or an
    /// unexpected filesystem/process-inspection failure.
    #[allow(clippy::too_many_lines)]
    pub fn recover(
        &self,
        authorized_workspace_fingerprints: &HashSet<String>,
    ) -> Result<SessionLockRecovery, SessionLockError> {
        if !self.lock_directory.exists() {
            return Ok(SessionLockRecovery::default());
        }
        let entries =
            fs::read_dir(&self.lock_directory).map_err(|source| SessionLockError::Io {
                path: self.lock_directory.clone(),
                source,
            })?;
        let mut recovery = SessionLockRecovery::default();
        let mut owner_paths = HashSet::new();
        let mut lock_paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| SessionLockError::Io {
                path: self.lock_directory.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("json")
                && path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| name.ends_with(".owner.json"))
            {
                owner_paths.insert(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some("lock") {
                lock_paths.push(path);
            }
        }

        for owner_path in owner_paths {
            let lock_path = lock_path_for_owner_path(&owner_path);
            match read_record_path(&owner_path) {
                Ok(record) => {
                    if !workspace_is_authorized(&record, authorized_workspace_fingerprints) {
                        recovery.owners.push(RecoveredSessionOwner {
                            record,
                            state: SessionRecoveryState::Unauthorized,
                        });
                        continue;
                    }
                    match recovery_state(&record)? {
                        RecoveryDisposition::Live(state) => {
                            recovery
                                .owners
                                .push(RecoveredSessionOwner { record, state });
                        }
                        RecoveryDisposition::Stale => {
                            if clear_stale_record(&lock_path, &owner_path, &record)? {
                                recovery.stale_cleared = recovery.stale_cleared.saturating_add(1);
                            } else {
                                recovery.blocked = recovery.blocked.saturating_add(1);
                            }
                        }
                        RecoveryDisposition::UnknownWriter => {
                            recovery.blocked = recovery.blocked.saturating_add(1);
                        }
                    }
                }
                Err(SessionLockError::UnsupportedVersion { .. }) => {
                    recovery.unsupported = recovery.unsupported.saturating_add(1);
                }
                Err(
                    SessionLockError::Malformed { .. } | SessionLockError::SessionIdMismatch { .. },
                ) => {
                    recovery.malformed = recovery.malformed.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }

        // A v1 record occupied the lock inode itself. Do not treat an empty
        // v2 sidecar lock as a legacy record.
        for lock_path in lock_paths {
            let owner_path = owner_path_for_lock_path(&lock_path);
            if owner_path.exists() {
                continue;
            }
            let mut file = open_lock_file(&lock_path)?;
            match file.try_lock_exclusive() {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    recovery.blocked = recovery.blocked.saturating_add(1);
                    continue;
                }
                Err(source) => {
                    return Err(SessionLockError::Io {
                        path: lock_path,
                        source,
                    });
                }
            }
            if file
                .metadata()
                .map_err(|source| SessionLockError::Io {
                    path: lock_path.clone(),
                    source,
                })?
                .len()
                == 0
            {
                continue;
            }
            match read_record_from_file(&mut file, &lock_path) {
                Ok(record) => {
                    let state = match recovery_state(&record)? {
                        RecoveryDisposition::Live(_) => SessionRecoveryState::LegacyRpcOwned,
                        RecoveryDisposition::Stale => {
                            clear_legacy_record(&mut file, &lock_path)?;
                            recovery.stale_cleared = recovery.stale_cleared.saturating_add(1);
                            continue;
                        }
                        RecoveryDisposition::UnknownWriter => {
                            recovery.blocked = recovery.blocked.saturating_add(1);
                            continue;
                        }
                    };
                    recovery
                        .owners
                        .push(RecoveredSessionOwner { record, state });
                }
                Err(SessionLockError::UnsupportedVersion { .. }) => {
                    recovery.unsupported = recovery.unsupported.saturating_add(1);
                }
                Err(
                    SessionLockError::Malformed { .. } | SessionLockError::SessionIdMismatch { .. },
                ) => {
                    recovery.malformed = recovery.malformed.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }

        Ok(recovery)
    }
}

/// Exclusive cross-process ownership of one Pi session.
pub struct SessionLease {
    lock_path: PathBuf,
    owner_path: PathBuf,
    record: SessionLockRecord,
    file: File,
    clear_on_drop: bool,
    released: bool,
}

impl SessionLease {
    /// Atomically claims a session for the current Pix RPC host process.
    ///
    /// This compatibility entry point does not attach a workspace fingerprint;
    /// production runtimes should use [`Self::acquire_for_workspace`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] when the process identity cannot be read,
    /// the session is already owned, or the lock state cannot be updated.
    pub fn acquire(lock_directory: &Path, session_id: SessionId) -> Result<Self, SessionLockError> {
        let owner = ProcessIdentity::current()?;
        let host = owner.clone();
        let record = SessionLockRecord::new(
            SessionOwnerKind::PixRpc,
            session_id,
            None,
            &owner,
            None,
            None,
            &host,
        );
        Self::acquire_record(lock_directory, &record, true)
    }

    /// Claims a session while binding the record to an authorized workspace.
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] when the workspace cannot be fingerprinted,
    /// the process identity cannot be read, the session is already owned, or
    /// the lock state cannot be updated.
    pub fn acquire_for_workspace(
        lock_directory: &Path,
        session_id: SessionId,
        workspace: &Path,
    ) -> Result<Self, SessionLockError> {
        let owner = ProcessIdentity::current()?;
        let host = owner.clone();
        let record = SessionLockRecord::new(
            SessionOwnerKind::PixRpc,
            session_id,
            Some(workspace_fingerprint(workspace)?),
            &owner,
            None,
            None,
            &host,
        );
        Self::acquire_record(lock_directory, &record, true)
    }

    /// Claims a session for a validated external Pi TUI owner. The caller is
    /// responsible for validating the owner identity against UDS peer creds.
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] when the session is already owned, the
    /// workspace cannot be fingerprinted, or the lock state is invalid.
    pub fn acquire_for_tui(
        lock_directory: &Path,
        session_id: SessionId,
        workspace: &Path,
        owner: &ProcessIdentity,
        bridge_instance_id: Uuid,
    ) -> Result<Self, SessionLockError> {
        let host = ProcessIdentity::current()?;
        let record = SessionLockRecord::new(
            SessionOwnerKind::PiTui,
            session_id,
            Some(workspace_fingerprint(workspace)?),
            owner,
            None,
            Some(bridge_instance_id),
            &host,
        );
        Self::acquire_record(lock_directory, &record, false)
    }

    fn acquire_record(
        lock_directory: &Path,
        record: &SessionLockRecord,
        clear_on_drop: bool,
    ) -> Result<Self, SessionLockError> {
        claim_in_process(record.session_id)?;
        match Self::acquire_claimed(lock_directory, record.clone(), clear_on_drop) {
            Ok(lease) => Ok(lease),
            Err(error) => {
                release_in_process(record.session_id);
                Err(error)
            }
        }
    }

    fn acquire_claimed(
        lock_directory: &Path,
        record: SessionLockRecord,
        clear_on_drop: bool,
    ) -> Result<Self, SessionLockError> {
        ensure_lock_directory(lock_directory)?;
        let lock_path = lock_path_for(lock_directory, record.session_id);
        let owner_path = owner_path_for(lock_directory, record.session_id);
        let mut file = open_lock_file(&lock_path)?;
        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                let existing = read_existing_record(&mut file, &lock_path, &owner_path)?;
                let Some(existing) = existing else {
                    return Err(SessionLockError::AlreadyOwned {
                        session_id: record.session_id,
                        pid: 0,
                        created_at: Utc::now(),
                    });
                };
                return Err(SessionLockError::AlreadyOwned {
                    session_id: record.session_id,
                    pid: existing.owner_pid,
                    created_at: existing.created_at,
                });
            }
            Err(source) => {
                return Err(SessionLockError::Io {
                    path: lock_path,
                    source,
                });
            }
        }

        if let Some(existing) = read_existing_record(&mut file, &lock_path, &owner_path)? {
            match owner_liveness(&existing)? {
                OwnerLiveness::Live => {
                    if existing.owner_kind == SessionOwnerKind::PiTui
                        && record.owner_kind == SessionOwnerKind::PiTui
                        && existing.owner_pid == record.owner_pid
                        && existing.owner_process_start_identity
                            == record.owner_process_start_identity
                        && existing.workspace_fingerprint == record.workspace_fingerprint
                    {
                        let mut replacement = record;
                        replacement.generation = existing.generation.saturating_add(1);
                        replacement.created_at = existing.created_at;
                        replacement.updated_at = Utc::now();
                        write_owner_record(&owner_path, &replacement)?;
                        clear_legacy_record(&mut file, &lock_path)?;
                        return Ok(Self {
                            lock_path,
                            owner_path,
                            record: replacement,
                            file,
                            clear_on_drop,
                            released: false,
                        });
                    }
                    return Err(SessionLockError::AlreadyOwned {
                        session_id: record.session_id,
                        pid: existing.owner_pid,
                        created_at: existing.created_at,
                    });
                }
                OwnerLiveness::UnknownWriter => {
                    return Err(SessionLockError::UnknownWriter {
                        session_id: record.session_id,
                        pid: existing.owner_pid,
                        path: owner_path,
                    });
                }
                OwnerLiveness::Dead => {
                    remove_owner_record(&mut file, &lock_path, &owner_path)?;
                }
            }
        }

        write_owner_record(&owner_path, &record)?;
        clear_legacy_record(&mut file, &lock_path)?;
        Ok(Self {
            lock_path,
            owner_path,
            record,
            file,
            clear_on_drop,
            released: false,
        })
    }

    /// Records the spawned Pi RPC writer identity before it can accept turns.
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] if this is not a Pix RPC lease or the
    /// atomic owner record update fails.
    pub fn set_writer_process(&mut self, writer: ProcessIdentity) -> Result<(), SessionLockError> {
        if self.record.owner_kind != SessionOwnerKind::PixRpc {
            return Err(SessionLockError::InvalidOperation(
                "only PixRpc leases can set a writer process",
            ));
        }
        self.record.writer_pid = Some(writer.pid);
        self.record.writer_process_start_identity = Some(writer.process_start_identity);
        self.record.updated_at = Utc::now();
        write_owner_record(&self.owner_path, &self.record)
    }

    /// Explicitly releases a TUI owner. A mismatched generation/nonce is
    /// rejected so an old bridge connection cannot clear a replacement owner.
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] when the lease is not a TUI lease, the
    /// generation/nonce does not match, or the owner record cannot be removed.
    pub fn release_external(
        &mut self,
        generation: u64,
        claim_nonce: Uuid,
    ) -> Result<(), SessionLockError> {
        if self.record.owner_kind != SessionOwnerKind::PiTui {
            return Err(SessionLockError::InvalidOperation(
                "only PiTui leases can use external release",
            ));
        }
        if self.record.generation != generation || self.record.claim_nonce != claim_nonce {
            return Err(SessionLockError::OwnershipTokenMismatch {
                session_id: self.record.session_id,
            });
        }
        if self.released {
            return Ok(());
        }
        if let Some(current) =
            read_existing_record(&mut self.file, &self.lock_path, &self.owner_path)?
            && !same_ownership_token(&current, &self.record)
        {
            return Err(SessionLockError::OwnershipTokenMismatch {
                session_id: self.record.session_id,
            });
        }
        remove_owner_record(&mut self.file, &self.lock_path, &self.owner_path)?;
        FileExt::unlock(&self.file).map_err(|source| SessionLockError::Io {
            path: self.lock_path.clone(),
            source,
        })?;
        release_in_process(self.record.session_id);
        self.clear_on_drop = false;
        self.released = true;
        Ok(())
    }

    #[must_use]
    pub const fn record(&self) -> &SessionLockRecord {
        &self.record
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.lock_path
    }

    #[must_use]
    pub fn owner_path(&self) -> &Path {
        &self.owner_path
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        if !self.released {
            if self.clear_on_drop {
                let _ = remove_owner_record(&mut self.file, &self.lock_path, &self.owner_path);
            }
            let _ = FileExt::unlock(&self.file);
            release_in_process(self.record.session_id);
        }
    }
}

/// Returns a stable, non-plaintext workspace key for lock records.
///
/// # Errors
///
/// Returns [`SessionLockError`] if the workspace cannot be canonicalized.
pub fn workspace_fingerprint(path: &Path) -> Result<String, SessionLockError> {
    let canonical = fs::canonicalize(path).map_err(|source| SessionLockError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    Ok(format!("sha256:{digest:x}"))
}

fn ensure_lock_directory(lock_directory: &Path) -> Result<(), SessionLockError> {
    fs::create_dir_all(lock_directory).map_err(|source| SessionLockError::Io {
        path: lock_directory.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(lock_directory, fs::Permissions::from_mode(0o700)).map_err(
            |source| SessionLockError::Io {
                path: lock_directory.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

fn lock_path_for(lock_directory: &Path, session_id: SessionId) -> PathBuf {
    lock_directory.join(format!("{session_id}.lock"))
}

fn owner_path_for(lock_directory: &Path, session_id: SessionId) -> PathBuf {
    lock_directory.join(format!("{session_id}.owner.json"))
}

fn owner_path_for_lock_path(lock_path: &Path) -> PathBuf {
    lock_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            "{}.owner.json",
            lock_path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".lock"))
                .unwrap_or_default()
        ))
}

fn lock_path_for_owner_path(owner_path: &Path) -> PathBuf {
    owner_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            "{}.lock",
            owner_path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".owner.json"))
                .unwrap_or_default()
        ))
}

fn open_lock_file(path: &Path) -> Result<File, SessionLockError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    let file = options.open(path).map_err(|source| SessionLockError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

fn open_owner_file(path: &Path) -> Result<File, SessionLockError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|source| SessionLockError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_owner_record(path: &Path, record: &SessionLockRecord) -> Result<(), SessionLockError> {
    let parent = path.parent().ok_or_else(|| SessionLockError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "owner record has no parent"),
    })?;
    let mut temporary = TempFileBuilder::new()
        .prefix(".owner-")
        .tempfile_in(parent)
        .map_err(|source| SessionLockError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    serde_json::to_writer(&mut temporary, record).map_err(|source| {
        SessionLockError::Malformed {
            path: path.to_path_buf(),
            source,
        }
    })?;
    temporary
        .write_all(b"\n")
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| SessionLockError::Io {
                path: path.to_path_buf(),
                source,
            })?;
    }
    temporary
        .persist(path)
        .map_err(|error| SessionLockError::Io {
            path: path.to_path_buf(),
            source: error.error,
        })?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), SessionLockError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| SessionLockError::Io {
                path: path.to_path_buf(),
                source,
            })?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn read_existing_record(
    lock_file: &mut File,
    lock_path: &Path,
    owner_path: &Path,
) -> Result<Option<SessionLockRecord>, SessionLockError> {
    if owner_path.exists() {
        return read_record_path(owner_path).map(Some);
    }
    if lock_file
        .metadata()
        .map_err(|source| SessionLockError::Io {
            path: lock_path.to_path_buf(),
            source,
        })?
        .len()
        > 0
    {
        return read_record_from_file(lock_file, lock_path).map(Some);
    }
    Ok(None)
}

fn read_record_path(path: &Path) -> Result<SessionLockRecord, SessionLockError> {
    let mut file = open_owner_file(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    decode_record(&bytes, path)
}

fn read_record_from_file(
    file: &mut File,
    path: &Path,
) -> Result<SessionLockRecord, SessionLockError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    decode_record(&bytes, path)
}

fn decode_record(bytes: &[u8], path: &Path) -> Result<SessionLockRecord, SessionLockError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|source| SessionLockError::Malformed {
            path: path.to_path_buf(),
            source,
        })?;
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| SessionLockError::Malformed {
            path: path.to_path_buf(),
            source: serde_json::Error::io(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing lock record version",
            )),
        })?;
    let record = match version {
        1 => serde_json::from_value::<LegacySessionLockRecord>(value)
            .map(SessionLockRecord::from_legacy)
            .map_err(|source| SessionLockError::Malformed {
                path: path.to_path_buf(),
                source,
            }),
        SESSION_LOCK_RECORD_VERSION => {
            serde_json::from_value(value).map_err(|source| SessionLockError::Malformed {
                path: path.to_path_buf(),
                source,
            })
        }
        version => Err(SessionLockError::UnsupportedVersion {
            path: path.to_path_buf(),
            version,
        }),
    }?;
    if let Some(expected_session_id) = session_id_from_path(path)
        && expected_session_id != record.session_id
    {
        return Err(SessionLockError::SessionIdMismatch {
            path: path.to_path_buf(),
            expected: expected_session_id,
            found: record.session_id,
        });
    }
    Ok(record)
}

fn session_id_from_path(path: &Path) -> Option<SessionId> {
    let name = path.file_name()?.to_str()?;
    let raw = name
        .strip_suffix(".owner.json")
        .or_else(|| name.strip_suffix(".lock"))?;
    raw.parse().ok()
}

fn remove_owner_record(
    lock_file: &mut File,
    lock_path: &Path,
    owner_path: &Path,
) -> Result<(), SessionLockError> {
    match fs::remove_file(owner_path) {
        Ok(()) => sync_directory(owner_path.parent().unwrap_or_else(|| Path::new(".")))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(SessionLockError::Io {
                path: owner_path.to_path_buf(),
                source,
            });
        }
    }
    clear_legacy_record(lock_file, lock_path)
}

fn clear_legacy_record(file: &mut File, path: &Path) -> Result<(), SessionLockError> {
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.sync_all())
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn clear_stale_record(
    lock_path: &Path,
    owner_path: &Path,
    expected: &SessionLockRecord,
) -> Result<bool, SessionLockError> {
    let mut file = open_lock_file(lock_path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
        Err(source) => {
            return Err(SessionLockError::Io {
                path: lock_path.to_path_buf(),
                source,
            });
        }
    }
    let Some(current) = read_existing_record(&mut file, lock_path, owner_path)? else {
        return Ok(true);
    };
    if current != *expected {
        return Ok(false);
    }
    if !matches!(owner_liveness(&current)?, OwnerLiveness::Dead) {
        return Ok(false);
    }
    remove_owner_record(&mut file, lock_path, owner_path)?;
    FileExt::unlock(&file).map_err(|source| SessionLockError::Io {
        path: lock_path.to_path_buf(),
        source,
    })?;
    Ok(true)
}

fn workspace_is_authorized(
    record: &SessionLockRecord,
    authorized_workspace_fingerprints: &HashSet<String>,
) -> bool {
    // v1 never carried a workspace binding. Keep its conservative legacy
    // liveness/migration path available; all v2 records must bind explicitly.
    if record.version == 1 {
        return true;
    }
    record
        .workspace_fingerprint
        .as_ref()
        .is_some_and(|fingerprint| authorized_workspace_fingerprints.contains(fingerprint))
}

fn same_ownership_token(left: &SessionLockRecord, right: &SessionLockRecord) -> bool {
    left.session_id == right.session_id
        && left.owner_kind == right.owner_kind
        && left.owner_pid == right.owner_pid
        && left.owner_process_start_identity == right.owner_process_start_identity
        && left.generation == right.generation
        && left.claim_nonce == right.claim_nonce
}

enum RecoveryDisposition {
    Live(SessionRecoveryState),
    Stale,
    UnknownWriter,
}

fn recovery_state(record: &SessionLockRecord) -> Result<RecoveryDisposition, SessionLockError> {
    match owner_liveness(record)? {
        OwnerLiveness::Live => {
            let state = match record.owner_kind {
                SessionOwnerKind::PiTui => SessionRecoveryState::TuiUnreachable,
                SessionOwnerKind::PixRpc if record.version == 1 => {
                    SessionRecoveryState::LegacyRpcOwned
                }
                SessionOwnerKind::PixRpc => SessionRecoveryState::RpcOrphanSuspect,
            };
            Ok(RecoveryDisposition::Live(state))
        }
        OwnerLiveness::Dead => Ok(RecoveryDisposition::Stale),
        OwnerLiveness::UnknownWriter => Ok(RecoveryDisposition::UnknownWriter),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerLiveness {
    Live,
    Dead,
    UnknownWriter,
}

fn owner_liveness(record: &SessionLockRecord) -> Result<OwnerLiveness, SessionLockError> {
    match record.owner_kind {
        SessionOwnerKind::PiTui => Ok(
            if process_identity_matches(record.owner_pid, &record.owner_process_start_identity)? {
                OwnerLiveness::Live
            } else {
                OwnerLiveness::Dead
            },
        ),
        SessionOwnerKind::PixRpc => {
            if record.version == 1 {
                return Ok(
                    if process_identity_matches(
                        record.owner_pid,
                        &record.owner_process_start_identity,
                    )? {
                        OwnerLiveness::Live
                    } else {
                        OwnerLiveness::Dead
                    },
                );
            }
            let (Some(writer_pid), Some(writer_start)) = (
                record.writer_pid,
                record.writer_process_start_identity.as_deref(),
            ) else {
                return Ok(OwnerLiveness::UnknownWriter);
            };
            Ok(if process_identity_matches(writer_pid, writer_start)? {
                OwnerLiveness::Live
            } else {
                OwnerLiveness::Dead
            })
        }
    }
}

fn process_identity_matches(pid: u32, expected: &str) -> Result<bool, SessionLockError> {
    Ok(process_start_identity(pid)?.as_deref() == Some(expected))
}

fn in_process_sessions() -> &'static Mutex<HashSet<SessionId>> {
    static SESSIONS: OnceLock<Mutex<HashSet<SessionId>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn claim_in_process(session_id: SessionId) -> Result<(), SessionLockError> {
    let inserted = in_process_sessions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id);
    if inserted {
        Ok(())
    } else {
        Err(SessionLockError::AlreadyOwnedInProcess(session_id))
    }
}

fn release_in_process(session_id: SessionId) {
    in_process_sessions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&session_id);
}

#[cfg(unix)]
fn process_start_identity(pid: u32) -> Result<Option<String>, SessionLockError> {
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .map_err(SessionLockError::ProcessInspection)?;
    if !output.status.success() {
        return Ok(None);
    }
    let identity = String::from_utf8(output.stdout)
        .map_err(SessionLockError::ProcessIdentityEncoding)?
        .trim()
        .to_owned();
    if identity.is_empty() {
        Ok(None)
    } else {
        Ok(Some(identity))
    }
}

#[cfg(not(unix))]
fn process_start_identity(_pid: u32) -> Result<Option<String>, SessionLockError> {
    Err(SessionLockError::UnsupportedPlatform)
}

#[derive(Debug, Error)]
pub enum SessionLockError {
    #[error("session {0} is already owned by this Pix process")]
    AlreadyOwnedInProcess(SessionId),
    #[error("session {session_id} is already owned by process {pid} since {created_at}")]
    AlreadyOwned {
        session_id: SessionId,
        pid: u32,
        created_at: DateTime<Utc>,
    },
    #[error("session {session_id} has an unknown RPC writer owned by process {pid}")]
    UnknownWriter {
        session_id: SessionId,
        pid: u32,
        path: PathBuf,
    },
    #[error("session {session_id} ownership token does not match the current owner")]
    OwnershipTokenMismatch { session_id: SessionId },
    #[error("invalid session lock operation: {0}")]
    InvalidOperation(&'static str),
    #[error("session lock at {path} is malformed: {source}")]
    Malformed {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("session lock at {path} uses unsupported version {version}")]
    UnsupportedVersion { path: PathBuf, version: u32 },
    #[error("session lock at {path} names session {expected}, but record contains {found}")]
    SessionIdMismatch {
        path: PathBuf,
        expected: SessionId,
        found: SessionId,
    },
    #[error("current Pix process {0} could not be inspected")]
    CurrentProcessNotFound(u32),
    #[error("Pi process {0} could not be inspected after spawn")]
    ProcessNotFound(u32),
    #[error("failed to inspect process identity: {0}")]
    ProcessInspection(io::Error),
    #[error("process identity was not valid UTF-8: {0}")]
    ProcessIdentityEncoding(std::string::FromUtf8Error),
    #[error("session locking is not supported on this platform")]
    UnsupportedPlatform,
    #[error("session lock I/O failed at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::path::{Path, PathBuf};

    use chrono::Utc;
    use fs2::FileExt;
    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        ProcessIdentity, SESSION_LOCK_RECORD_VERSION, SessionId, SessionLease, SessionLockError,
        SessionLockStore, SessionOwnerKind, SessionRecoveryState, lock_path_for_owner_path,
        owner_path_for_lock_path,
    };

    #[test]
    fn derives_matching_lock_and_owner_sidecar_paths() {
        assert_eq!(
            owner_path_for_lock_path(Path::new("/tmp/session.lock")),
            PathBuf::from("/tmp/session.owner.json")
        );
        assert_eq!(
            lock_path_for_owner_path(Path::new("/tmp/session.owner.json")),
            PathBuf::from("/tmp/session.lock")
        );
    }

    #[test]
    fn prevents_a_second_live_owner_and_releases_on_drop() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let first = SessionLease::acquire(directory.path(), session_id).expect("first lease");

        assert!(matches!(
            SessionLease::acquire(directory.path(), session_id),
            Err(SessionLockError::AlreadyOwnedInProcess(_))
        ));
        drop(first);
        SessionLease::acquire(directory.path(), session_id).expect("reacquire lease");
    }

    #[test]
    fn writes_v2_record_to_an_atomic_sidecar() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let lease = SessionLease::acquire(directory.path(), session_id).expect("lease");
        assert_eq!(lease.record().version, SESSION_LOCK_RECORD_VERSION);
        assert_eq!(lease.record().owner_kind, SessionOwnerKind::PixRpc);
        assert!(lease.owner_path().is_file());
        assert_eq!(fs::metadata(lease.path()).expect("lock metadata").len(), 0);
        let record = fs::read_to_string(lease.owner_path()).expect("owner record");
        assert!(record.contains("\"owner_kind\":\"pix_rpc\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(directory.path())
                    .expect("lock directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(lease.path())
                    .expect("lock metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(lease.owner_path())
                    .expect("owner metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn external_tui_record_separates_owner_and_host_identity() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let tui_owner = ProcessIdentity {
            pid: 42,
            process_start_identity: "tui-start".to_owned(),
        };
        let lease = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &tui_owner,
            uuid::Uuid::new_v4(),
        )
        .expect("TUI lease");
        assert_eq!(lease.record().owner_kind, SessionOwnerKind::PiTui);
        assert_eq!(lease.record().owner_pid, tui_owner.pid);
        assert_eq!(
            lease.record().owner_process_start_identity,
            tui_owner.process_start_identity
        );
        assert_eq!(lease.record().lease_holder_pid, std::process::id());
        assert_eq!(
            lease.record().lease_holder_process_start_identity,
            ProcessIdentity::current()
                .expect("current identity")
                .process_start_identity
        );
        assert!(lease.record().writer_pid.is_none());
        assert!(lease.record().bridge_instance_id.is_some());
    }

    #[test]
    fn external_tui_record_survives_drop_until_explicit_release() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let owner = ProcessIdentity::current().expect("current identity");
        let bridge_id = uuid::Uuid::new_v4();
        let lease = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &owner,
            bridge_id,
        )
        .expect("TUI lease");
        let owner_path = lease.owner_path().to_path_buf();
        drop(lease);
        assert!(owner_path.is_file());

        let result = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &ProcessIdentity {
                pid: owner.pid,
                process_start_identity: "different-start".to_owned(),
            },
            bridge_id,
        );
        assert!(matches!(result, Err(SessionLockError::AlreadyOwned { .. })));
    }

    #[test]
    fn reconnecting_the_same_tui_replaces_the_bridge_claim() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let owner = ProcessIdentity::current().expect("current identity");
        let first = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &owner,
            uuid::Uuid::new_v4(),
        )
        .expect("first TUI lease");
        let first_generation = first.record().generation;
        let first_nonce = first.record().claim_nonce;
        drop(first);

        let mut second = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &owner,
            uuid::Uuid::new_v4(),
        )
        .expect("reconnected TUI lease");
        assert_eq!(second.record().generation, first_generation + 1);
        assert_ne!(second.record().claim_nonce, first_nonce);
        assert!(matches!(
            second.release_external(first_generation, first_nonce),
            Err(SessionLockError::OwnershipTokenMismatch { .. })
        ));
        let generation = second.record().generation;
        let nonce = second.record().claim_nonce;
        let owner_path = second.owner_path().to_path_buf();
        second
            .release_external(generation, nonce)
            .expect("release current TUI owner");
        assert!(!owner_path.exists());

        let replacement = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &ProcessIdentity::current().expect("current identity"),
            uuid::Uuid::new_v4(),
        )
        .expect("claim after explicit release");
        second
            .release_external(generation, nonce)
            .expect("repeated release is idempotent");
        assert!(replacement.owner_path().is_file());
    }

    #[test]
    fn recovers_live_tui_as_unreachable() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let lease = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &ProcessIdentity::current().expect("current identity"),
            uuid::Uuid::new_v4(),
        )
        .expect("TUI lease");
        let fingerprint = super::workspace_fingerprint(directory.path()).expect("fingerprint");
        drop(lease);
        let mut authorized = std::collections::HashSet::new();
        authorized.insert(fingerprint);
        let recovery = SessionLockStore::new(directory.path())
            .recover(&authorized)
            .expect("recover TUI");
        assert_eq!(recovery.owners().len(), 1);
        assert_eq!(
            recovery.owners()[0].state,
            SessionRecoveryState::TuiUnreachable
        );
    }

    #[test]
    fn recovery_clears_a_dead_tui_owner_but_keeps_authorized_live_state() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let owner = ProcessIdentity {
            pid: u32::MAX,
            process_start_identity: "not a process".to_owned(),
        };
        let lease = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &owner,
            uuid::Uuid::new_v4(),
        )
        .expect("dead-owner fixture");
        let owner_path = lease.owner_path().to_path_buf();
        let fingerprint = super::workspace_fingerprint(directory.path()).expect("fingerprint");
        drop(lease);

        let mut authorized = std::collections::HashSet::new();
        authorized.insert(fingerprint);
        let recovery = SessionLockStore::new(directory.path())
            .recover(&authorized)
            .expect("recover dead TUI");
        assert_eq!(recovery.stale_cleared(), 1);
        assert!(recovery.owners().is_empty());
        assert!(!owner_path.exists());
    }

    #[test]
    fn recovery_does_not_clear_a_stale_sidecar_while_lock_is_busy() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let lease = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &ProcessIdentity {
                pid: u32::MAX,
                process_start_identity: "not a process".to_owned(),
            },
            uuid::Uuid::new_v4(),
        )
        .expect("dead-owner fixture");
        let owner_path = lease.owner_path().to_path_buf();
        let lock_path = lease.path().to_path_buf();
        let fingerprint = super::workspace_fingerprint(directory.path()).expect("fingerprint");
        drop(lease);

        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("open lock");
        lock_file.lock_exclusive().expect("hold lock");
        let mut authorized = std::collections::HashSet::new();
        authorized.insert(fingerprint);
        let recovery = SessionLockStore::new(directory.path())
            .recover(&authorized)
            .expect("recover busy stale owner");
        assert_eq!(recovery.blocked(), 1);
        assert!(owner_path.exists());
        lock_file.unlock().expect("unlock lock");
    }

    #[test]
    fn recovery_counts_malformed_and_unknown_records_without_overwriting_them() {
        let directory = tempdir().expect("temporary directory");
        let malformed_path = directory.path().join("malformed.owner.json");
        let unsupported_path = directory.path().join("unsupported.owner.json");
        fs::write(&malformed_path, b"not json").expect("write malformed owner");
        fs::write(
            &unsupported_path,
            br#"{"version":99,"session_id":"unsupported"}"#,
        )
        .expect("write unsupported owner");

        let recovery = SessionLockStore::new(directory.path())
            .recover(&std::collections::HashSet::new())
            .expect("recover damaged records");
        assert_eq!(recovery.malformed(), 1);
        assert_eq!(recovery.unsupported(), 1);
        assert_eq!(
            fs::read(&malformed_path).expect("read malformed owner"),
            b"not json"
        );
        assert_eq!(
            fs::read(&unsupported_path).expect("read unsupported owner"),
            br#"{"version":99,"session_id":"unsupported"}"#
        );
    }

    #[test]
    fn recovery_rejects_a_record_whose_filename_names_another_session() {
        let directory = tempdir().expect("temporary directory");
        let source_id = SessionId::new();
        let target_id = SessionId::new();
        let lease = SessionLease::acquire_for_tui(
            directory.path(),
            source_id,
            directory.path(),
            &ProcessIdentity::current().expect("current identity"),
            uuid::Uuid::new_v4(),
        )
        .expect("TUI lease");
        let source_path = lease.owner_path().to_path_buf();
        let target_path = directory.path().join(format!("{target_id}.owner.json"));
        let fingerprint = super::workspace_fingerprint(directory.path()).expect("fingerprint");
        drop(lease);
        fs::rename(source_path, &target_path).expect("rename owner fixture");

        let mut authorized = std::collections::HashSet::new();
        authorized.insert(fingerprint);
        let recovery = SessionLockStore::new(directory.path())
            .recover(&authorized)
            .expect("recover mismatched owner");
        assert_eq!(recovery.malformed(), 1);
        assert!(target_path.exists());
    }

    #[test]
    fn recovery_fails_closed_for_missing_or_unknown_workspace() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let lease = SessionLease::acquire_for_tui(
            directory.path(),
            session_id,
            directory.path(),
            &ProcessIdentity::current().expect("current identity"),
            uuid::Uuid::new_v4(),
        )
        .expect("TUI lease");
        let owner_path = lease.owner_path().to_path_buf();
        drop(lease);

        let recovery = SessionLockStore::new(directory.path())
            .recover(&std::collections::HashSet::new())
            .expect("recover unauthorized owner");
        assert_eq!(recovery.owners().len(), 1);
        assert_eq!(
            recovery.owners()[0].state,
            SessionRecoveryState::Unauthorized
        );
        assert!(owner_path.exists());
    }

    #[test]
    fn recovery_blocks_an_rpc_record_before_writer_identity_is_persisted() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let lease =
            SessionLease::acquire_for_workspace(directory.path(), session_id, directory.path())
                .expect("RPC lease");
        let fingerprint = super::workspace_fingerprint(directory.path()).expect("fingerprint");
        let mut authorized = std::collections::HashSet::new();
        authorized.insert(fingerprint);
        let recovery = SessionLockStore::new(directory.path())
            .recover(&authorized)
            .expect("recover pre-writer RPC owner");
        assert_eq!(recovery.blocked(), 1);
        assert!(recovery.owners().is_empty());
        drop(lease);
    }

    #[test]
    fn replaces_a_stale_legacy_lock() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let path = directory.path().join(format!("{session_id}.lock"));
        let stale = json!({
            "version": 1,
            "pid": u32::MAX,
            "process_start_identity": "not a process",
            "session_id": session_id,
            "created_at": Utc::now(),
        });
        fs::write(
            &path,
            serde_json::to_vec(&stale).expect("encode stale lock"),
        )
        .expect("write stale lock");

        let lease =
            SessionLease::acquire(directory.path(), session_id).expect("replace stale lock");
        assert_eq!(lease.record().owner_pid, std::process::id());
    }
}
