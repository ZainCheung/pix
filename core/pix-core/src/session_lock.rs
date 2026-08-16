use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLockRecord {
    pub version: u32,
    pub pid: u32,
    pub process_start_identity: String,
    pub session_id: SessionId,
    pub created_at: DateTime<Utc>,
}

/// Exclusive cross-process ownership of one Pi session.
pub struct SessionLease {
    path: PathBuf,
    record: SessionLockRecord,
    file: File,
}

impl SessionLease {
    /// Atomically claims a session for the current Pix host process.
    ///
    /// The persistent lock record is considered stale only after the recorded
    /// PID and process start identity no longer identify the same live process.
    /// An advisory file lock closes the inspection/replacement race.
    ///
    /// # Errors
    ///
    /// Returns [`SessionLockError`] for live ownership conflicts, malformed
    /// lock state, process inspection failures, or filesystem errors.
    pub fn acquire(lock_directory: &Path, session_id: SessionId) -> Result<Self, SessionLockError> {
        claim_in_process(session_id)?;
        match Self::acquire_claimed(lock_directory, session_id) {
            Ok(lease) => Ok(lease),
            Err(error) => {
                release_in_process(session_id);
                Err(error)
            }
        }
    }

    fn acquire_claimed(
        lock_directory: &Path,
        session_id: SessionId,
    ) -> Result<Self, SessionLockError> {
        fs::create_dir_all(lock_directory).map_err(|source| SessionLockError::Io {
            path: lock_directory.to_path_buf(),
            source,
        })?;
        let path = lock_directory.join(format!("{session_id}.lock"));
        let mut file = open_lock_file(&path)?;
        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                let existing = read_record(&mut file, &path)?;
                return Err(SessionLockError::AlreadyOwned {
                    session_id,
                    pid: existing.pid,
                    created_at: existing.created_at,
                });
            }
            Err(source) => {
                return Err(SessionLockError::Io {
                    path: path.clone(),
                    source,
                });
            }
        }

        if file
            .metadata()
            .map_err(|source| SessionLockError::Io {
                path: path.clone(),
                source,
            })?
            .len()
            > 0
        {
            let existing = read_record(&mut file, &path)?;
            if owner_is_live(&existing)? {
                return Err(SessionLockError::AlreadyOwned {
                    session_id,
                    pid: existing.pid,
                    created_at: existing.created_at,
                });
            }
        }

        let pid = std::process::id();
        let process_start_identity =
            process_start_identity(pid)?.ok_or(SessionLockError::CurrentProcessNotFound(pid))?;
        let record = SessionLockRecord {
            version: 1,
            pid,
            process_start_identity,
            session_id,
            created_at: Utc::now(),
        };
        write_record(&mut file, &path, &record)?;
        Ok(Self { path, record, file })
    }

    #[must_use]
    pub const fn record(&self) -> &SessionLockRecord {
        &self.record
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        let _ = clear_record(&mut self.file, &self.path);
        let _ = FileExt::unlock(&self.file);
        release_in_process(self.record.session_id);
    }
}

fn open_lock_file(path: &Path) -> Result<File, SessionLockError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|source| SessionLockError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_record(
    file: &mut File,
    path: &Path,
    record: &SessionLockRecord,
) -> Result<(), SessionLockError> {
    file.set_len(0).map_err(|source| SessionLockError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, record).map_err(|source| SessionLockError::Malformed {
        path: path.to_path_buf(),
        source,
    })?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .and_then(|()| writer.get_ref().sync_all())
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn clear_record(file: &mut File, path: &Path) -> Result<(), SessionLockError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.set_len(0)
        .and_then(|()| file.sync_all())
        .map_err(|source| SessionLockError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn read_record(file: &mut File, path: &Path) -> Result<SessionLockRecord, SessionLockError> {
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
    let record: SessionLockRecord =
        serde_json::from_slice(&bytes).map_err(|source| SessionLockError::Malformed {
            path: path.to_path_buf(),
            source,
        })?;
    if record.version != 1 {
        return Err(SessionLockError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: record.version,
        });
    }
    Ok(record)
}

fn owner_is_live(record: &SessionLockRecord) -> Result<bool, SessionLockError> {
    Ok(process_start_identity(record.pid)?.as_deref()
        == Some(record.process_start_identity.as_str()))
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
    #[error("session {session_id} is already owned by Pix process {pid} since {created_at}")]
    AlreadyOwned {
        session_id: SessionId,
        pid: u32,
        created_at: DateTime<Utc>,
    },
    #[error("session lock at {path} is malformed: {source}")]
    Malformed {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("session lock at {path} uses unsupported version {version}")]
    UnsupportedVersion { path: PathBuf, version: u32 },
    #[error("current Pix process {0} could not be inspected")]
    CurrentProcessNotFound(u32),
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
    use std::fs;

    use chrono::Utc;
    use tempfile::tempdir;

    use super::{SessionId, SessionLease, SessionLockError, SessionLockRecord};

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
    fn replaces_a_stale_lock() {
        let directory = tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let path = directory.path().join(format!("{session_id}.lock"));
        let stale = SessionLockRecord {
            version: 1,
            pid: u32::MAX,
            process_start_identity: "not a process".to_owned(),
            session_id,
            created_at: Utc::now(),
        };
        fs::write(
            &path,
            serde_json::to_vec(&stale).expect("encode stale lock"),
        )
        .expect("write stale lock");

        let lease =
            SessionLease::acquire(directory.path(), session_id).expect("replace stale lock");
        assert_eq!(lease.record().pid, std::process::id());
    }
}
