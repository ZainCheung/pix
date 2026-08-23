use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use tempfile::Builder;
use thiserror::Error;
use uuid::Uuid;

const CONFIG_LOCK_TIMEOUT: Duration = Duration::from_secs(3);

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostConfig {
    pub version: u32,
    pub host: HostIdentity,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceRecord>,
    #[serde(default)]
    pub devices: Vec<DeviceRecord>,
    #[serde(default)]
    pub preferences: Preferences,
    /// Fields written by a newer build within the same schema version. An
    /// older binary must round-trip them unchanged instead of silently
    /// dropping configuration on its next save.
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

impl HostConfig {
    #[must_use]
    pub fn new(display_name: impl Into<String>) -> Self {
        Self {
            version: CONFIG_VERSION,
            host: HostIdentity {
                id: Uuid::new_v4(),
                display_name: display_name.into(),
            },
            workspaces: Vec::new(),
            devices: Vec::new(),
            preferences: Preferences::default(),
            unknown: serde_json::Map::new(),
        }
    }

    /// Checks schema compatibility and persistent-state invariants.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the schema is unsupported or a durable
    /// record would violate a configuration invariant.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: self.version,
                supported: CONFIG_VERSION,
            });
        }
        if self.host.display_name.trim().is_empty() {
            return Err(ConfigError::Invalid("host display name cannot be empty"));
        }
        if self.preferences.max_active_sessions == 0 {
            return Err(ConfigError::Invalid(
                "max_active_sessions must be greater than zero",
            ));
        }
        if self.preferences.max_concurrent_turns == 0 {
            return Err(ConfigError::Invalid(
                "max_concurrent_turns must be greater than zero",
            ));
        }
        if let Some(url) = &self.preferences.relay_url
            && crate::relay_client::validate_relay_url(url).is_err()
        {
            return Err(ConfigError::Invalid(
                "relay_url must be a valid ws:// or wss:// endpoint without credentials or a fragment",
            ));
        }

        for (index, workspace) in self.workspaces.iter().enumerate() {
            if workspace.name.trim().is_empty() {
                return Err(ConfigError::Invalid("workspace name cannot be empty"));
            }
            if !workspace.path.is_absolute() {
                return Err(ConfigError::Invalid("workspace path must be absolute"));
            }
            if self.workspaces[..index]
                .iter()
                .any(|other| other.id == workspace.id || other.path == workspace.path)
            {
                return Err(ConfigError::Invalid(
                    "workspace IDs and canonical paths must be unique",
                ));
            }
        }

        for (index, device) in self.devices.iter().enumerate() {
            if device.name.trim().is_empty() {
                return Err(ConfigError::Invalid("device name cannot be empty"));
            }
            if device.id.trim().is_empty()
                || device.public_key.trim().is_empty()
                || device.relay_channel.trim().is_empty()
            {
                return Err(ConfigError::Invalid(
                    "device identity and relay channel cannot be empty",
                ));
            }
            if self.devices[..index].iter().any(|other| {
                other.id == device.id
                    || other.public_key == device.public_key
                    || other.relay_channel == device.relay_channel
            }) {
                return Err(ConfigError::Invalid(
                    "device IDs, public keys, and relay channels must be unique",
                ));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostIdentity {
    pub id: Uuid,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub public_key: String,
    pub relay_channel: String,
    pub paired_at: DateTime<Utc>,
    /// Round-trips per-device fields introduced by newer builds.
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    pub relay_enabled: bool,
    /// Base WebSocket endpoint of the deployed Pix relay, for example
    /// `wss://relay.example.com`. Relay transport stays off until both this
    /// URL is present and `relay_enabled` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    pub idle_timeout_seconds: u64,
    /// Maximum number of Pi child processes kept resident at once.
    pub max_active_sessions: usize,
    /// Maximum number of sessions allowed to execute a turn concurrently.
    #[serde(default = "default_max_concurrent_turns")]
    pub max_concurrent_turns: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi_executable: Option<PathBuf>,
    /// Round-trips preference fields introduced by newer builds.
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

impl Preferences {
    /// Returns the relay endpoint only when relay transport is active.
    #[must_use]
    pub fn active_relay_url(&self) -> Option<&str> {
        if !self.relay_enabled {
            return None;
        }
        self.relay_url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
    }
}

fn default_max_concurrent_turns() -> usize {
    4
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            relay_enabled: true,
            relay_url: None,
            idle_timeout_seconds: 300,
            max_active_sessions: 4,
            max_concurrent_turns: 4,
            pi_executable: None,
            unknown: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

/// An exclusive, cross-process configuration transaction.
///
/// Keep the transaction short: read the latest snapshot, apply one logical
/// mutation, and save it. The adjacent lock file is intentionally persistent;
/// the operating system releases the advisory lock when this value is dropped.
pub struct ConfigTransaction<'a> {
    store: &'a ConfigStore,
    _lock: File,
}

impl ConfigTransaction<'_> {
    /// Loads the latest validated configuration while the exclusive lock is held.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the file is unreadable or fails
    /// validation.
    pub fn load(&self) -> Result<HostConfig, ConfigError> {
        self.store.load_unlocked()
    }

    /// Loads configuration or creates the first host identity under the lock.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when reading fails for a reason other than a
    /// missing file, or when the initial save fails.
    pub fn load_or_create(
        &self,
        display_name: impl Into<String>,
    ) -> Result<HostConfig, ConfigError> {
        match self.load() {
            Ok(config) => Ok(config),
            Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                let config = HostConfig::new(display_name);
                self.save(&config)?;
                Ok(config)
            }
            Err(error) => Err(error),
        }
    }

    /// Atomically saves configuration before releasing the transaction lock.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when serialization or the atomic write fails.
    pub fn save(&self, config: &HostConfig) -> Result<(), ConfigError> {
        self.store.save_unlocked(config)
    }
}

impl ConfigStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns Pix's unified per-user configuration file location.
    ///
    /// Pix deliberately uses the same `.config/pix` layout on macOS and
    /// Linux so host setup, service units, and diagnostics refer to one
    /// predictable location. Windows uses `USERPROFILE` as a fallback when
    /// the POSIX-style `HOME` variable is not present.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::NoConfigDirectory`] when the operating system
    /// does not expose a per-user configuration directory.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or(ConfigError::NoConfigDirectory)?;
        Ok(home.join(".config").join("pix").join("config.json"))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads and validates the current configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for filesystem, JSON, schema, or invariant
    /// failures. An absent file is reported and is not created implicitly.
    pub fn load(&self) -> Result<HostConfig, ConfigError> {
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> Result<HostConfig, ConfigError> {
        let bytes = fs::read(&self.path).map_err(|source| ConfigError::Read {
            path: self.path.clone(),
            source,
        })?;
        let config: HostConfig =
            serde_json::from_slice(&bytes).map_err(|source| ConfigError::Decode {
                path: self.path.clone(),
                source,
            })?;
        config.validate()?;
        Ok(config)
    }

    /// Loads configuration, creating a new host identity only when absent.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when existing state is unreadable or invalid,
    /// or when the initial atomic write fails.
    pub fn load_or_create(
        &self,
        display_name: impl Into<String>,
    ) -> Result<HostConfig, ConfigError> {
        self.transaction()?.load_or_create(display_name)
    }

    /// Persists configuration using a same-directory temporary file, file
    /// fsync, atomic rename, and directory fsync on Unix.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if validation or any persistence step fails.
    /// A newer schema already on disk is never overwritten.
    pub fn save(&self, config: &HostConfig) -> Result<(), ConfigError> {
        self.transaction()?.save(config)
    }

    /// Acquires the per-config cross-process mutation lock.
    ///
    /// Callers performing read-modify-write changes must load and save through
    /// the returned transaction so a stale snapshot cannot resurrect revoked
    /// devices or workspace authorization.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the parent directory cannot be created or
    /// the lock file cannot be opened.
    pub fn transaction(&self) -> Result<ConfigTransaction<'_>, ConfigError> {
        let parent = self.path.parent().ok_or_else(|| ConfigError::InvalidPath {
            path: self.path.clone(),
        })?;
        fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
        let lock_path = self.path.with_extension("lock");
        validate_lock_directory(parent, &lock_path)?;
        let lock = open_lock_file(&lock_path).map_err(|source| ConfigError::Lock {
            path: lock_path.clone(),
            source,
        })?;
        validate_lock_file(&lock, &lock_path)?;
        let deadline = Instant::now() + CONFIG_LOCK_TIMEOUT;
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(ConfigError::Busy { path: lock_path });
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(source) => {
                    return Err(ConfigError::Lock {
                        path: lock_path,
                        source,
                    });
                }
            }
        }
        Ok(ConfigTransaction {
            store: self,
            _lock: lock,
        })
    }

    fn save_unlocked(&self, config: &HostConfig) -> Result<(), ConfigError> {
        config.validate()?;
        self.refuse_newer_on_disk()?;

        let parent = self.path.parent().ok_or_else(|| ConfigError::InvalidPath {
            path: self.path.clone(),
        })?;
        fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: parent.to_path_buf(),
            source,
        })?;

        let mut temporary = Builder::new()
            .prefix(".pix-config-")
            .tempfile_in(parent)
            .map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        let temporary_path = temporary.path().to_path_buf();

        {
            let mut writer = BufWriter::new(temporary.as_file_mut());
            serde_json::to_writer_pretty(&mut writer, config).map_err(ConfigError::Encode)?;
            writer
                .write_all(b"\n")
                .map_err(|source| ConfigError::Write {
                    path: temporary_path.clone(),
                    source,
                })?;
            writer.flush().map_err(|source| ConfigError::Write {
                path: temporary_path.clone(),
                source,
            })?;
        }
        temporary
            .as_file()
            .sync_all()
            .map_err(|source| ConfigError::Write {
                path: temporary_path,
                source,
            })?;

        temporary
            .persist(&self.path)
            .map_err(|error| ConfigError::Write {
                path: self.path.clone(),
                source: error.error,
            })?;
        sync_directory(parent)?;
        Ok(())
    }

    fn refuse_newer_on_disk(&self) -> Result<(), ConfigError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|source| ConfigError::Decode {
                path: self.path.clone(),
                source,
            })?;
        let Some(version) = value.get("version").and_then(serde_json::Value::as_u64) else {
            return Err(ConfigError::Invalid("config version is missing"));
        };
        if version > u64::from(CONFIG_VERSION) {
            return Err(ConfigError::UnsupportedVersion {
                found: u32::try_from(version).unwrap_or(u32::MAX),
                supported: CONFIG_VERSION,
            });
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(unix)]
fn validate_lock_directory(parent: &Path, lock_path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::metadata(parent).map_err(|source| ConfigError::Lock {
        path: lock_path.to_path_buf(),
        source,
    })?;
    if metadata.mode() & 0o022 != 0 {
        return Err(ConfigError::UnsafeLockDirectory {
            path: parent.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_lock_directory(_parent: &Path, _lock_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(unix)]
fn validate_lock_file(lock: &File, path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let metadata = lock.metadata().map_err(|source| ConfigError::Lock {
        path: path.to_path_buf(),
        source,
    })?;
    let parent_owner = path
        .parent()
        .and_then(|parent| fs::metadata(parent).ok())
        .map(|parent| parent.uid());
    if !metadata.file_type().is_file() || parent_owner != Some(metadata.uid()) {
        return Err(ConfigError::UnsafeLockFile {
            path: path.to_path_buf(),
        });
    }
    if metadata.mode() & 0o077 != 0 {
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| ConfigError::Lock {
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_lock_file(lock: &File, path: &Path) -> Result<(), ConfigError> {
    if !lock
        .metadata()
        .map_err(|source| ConfigError::Lock {
            path: path.to_path_buf(),
            source,
        })?
        .is_file()
    {
        return Err(ConfigError::UnsafeLockFile {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ConfigError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("no platform configuration directory is available")]
    NoConfigDirectory,
    #[error("invalid configuration path: {path}")]
    InvalidPath { path: PathBuf },
    #[error("failed to read configuration at {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("failed to write configuration at {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("failed to lock configuration at {path}: {source}")]
    Lock { path: PathBuf, source: io::Error },
    #[error("configuration is busy; timed out waiting for {path}")]
    Busy { path: PathBuf },
    #[error("configuration lock directory is not private and user-owned: {path}")]
    UnsafeLockDirectory { path: PathBuf },
    #[error("configuration lock is not a private, user-owned regular file: {path}")]
    UnsafeLockFile { path: PathBuf },
    #[error("failed to decode configuration at {path}: {source}")]
    Decode {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to encode configuration: {0}")]
    Encode(serde_json::Error),
    #[error("unsupported configuration version {found}; this build supports version {supported}")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("invalid configuration: {0}")]
    Invalid(&'static str),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::thread;

    use tempfile::tempdir;

    use super::{ConfigError, ConfigStore, HostConfig};

    #[test]
    fn default_path_uses_the_unified_dot_config_location() {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("test environment home directory");

        assert_eq!(
            ConfigStore::default_path().expect("default config path"),
            home.join(".config").join("pix").join("config.json")
        );
    }

    #[test]
    fn round_trips_config() {
        let directory = tempdir().expect("temporary directory");
        let store = ConfigStore::new(directory.path().join("nested/config.json"));
        let config = HostConfig::new("Test Mac");

        store.save(&config).expect("save config");

        assert_eq!(store.load().expect("load config"), config);
    }

    #[test]
    fn load_or_create_does_not_replace_invalid_config() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.json");
        fs::write(&path, b"not json").expect("write fixture");
        let store = ConfigStore::new(&path);

        assert!(matches!(
            store.load_or_create("Test Mac"),
            Err(ConfigError::Decode { .. })
        ));
        assert_eq!(fs::read(path).expect("read fixture"), b"not json");
    }

    #[test]
    fn accepts_preferences_without_an_explicit_pi_path() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.json");
        fs::write(
            &path,
            br#"{
              "version": 1,
              "host": {"id": "4cc891bc-30b9-4b5f-9298-38471d9b27ea", "display_name": "Test Mac"},
              "workspaces": [],
              "devices": [],
              "preferences": {
                "relay_enabled": true,
                "idle_timeout_seconds": 300,
                "max_active_sessions": 4
              }
            }"#,
        )
        .expect("write fixture");

        let config = ConfigStore::new(&path).load().expect("load config");
        assert_eq!(config.preferences.pi_executable, None);
        assert_eq!(config.preferences.max_concurrent_turns, 4);
    }

    #[test]
    fn rejects_zero_concurrent_turn_limit() {
        let mut config = HostConfig::new("Test Mac");
        config.preferences.max_concurrent_turns = 0;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::Invalid(
                "max_concurrent_turns must be greater than zero"
            ))
        ));
    }

    #[test]
    fn rejects_unsafe_relay_urls_at_the_persistence_boundary() {
        for url in [
            "wss://user:secret@relay.example.com",
            "wss://relay.example.com/#fragment",
            "wss://relay.example.com\nspoof",
            "https://relay.example.com",
        ] {
            let mut config = HostConfig::new("Test Mac");
            config.preferences.relay_url = Some(url.to_owned());
            assert!(config.validate().is_err(), "accepted {url}");
        }
    }

    #[test]
    fn round_trips_fields_from_newer_builds_of_the_same_schema() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.json");
        fs::write(
            &path,
            br#"{
              "version": 1,
              "host": {"id": "4cc891bc-30b9-4b5f-9298-38471d9b27ea", "display_name": "Test Mac"},
              "future_top_level": {"kept": true},
              "workspaces": [],
              "devices": [{
                "id": "device-1",
                "name": "iPhone",
                "public_key": "AAAA",
                "relay_channel": "BBBB",
                "paired_at": "2026-08-12T21:23:46Z",
                "future_device_field": 7
              }],
              "preferences": {
                "relay_enabled": true,
                "relay_url": "wss://relay.example.invalid",
                "future_preference": "kept",
                "idle_timeout_seconds": 300,
                "max_active_sessions": 4
              }
            }"#,
        )
        .expect("write fixture");

        let store = ConfigStore::new(&path);
        let config = store.load().expect("load config");
        store.save(&config).expect("save config back");

        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("reread")).expect("json");
        assert_eq!(raw["future_top_level"]["kept"], true);
        assert_eq!(raw["devices"][0]["future_device_field"], 7);
        assert_eq!(raw["preferences"]["future_preference"], "kept");
        assert_eq!(
            raw["preferences"]["relay_url"],
            "wss://relay.example.invalid"
        );
    }

    #[test]
    fn never_overwrites_a_newer_schema() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.json");
        fs::write(&path, br#"{"version":999}"#).expect("write fixture");
        let store = ConfigStore::new(&path);

        assert!(matches!(
            store.save(&HostConfig::new("Test Mac")),
            Err(ConfigError::UnsupportedVersion { found: 999, .. })
        ));
    }

    #[test]
    fn transactions_serialize_cross_thread_read_modify_write() {
        let directory = tempdir().expect("temporary directory");
        let store = ConfigStore::new(directory.path().join("config.json"));
        store
            .save(&HostConfig::new("Test Mac"))
            .expect("save initial config");

        let workers = (0..12)
            .map(|_| {
                let store = store.clone();
                thread::spawn(move || {
                    let transaction = store.transaction().expect("lock config");
                    let mut config = transaction.load().expect("load config");
                    let count = config
                        .unknown
                        .get("test_counter")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    config
                        .unknown
                        .insert("test_counter".to_owned(), serde_json::json!(count + 1));
                    transaction.save(&config).expect("save config");
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("join config worker");
        }

        assert_eq!(
            store.load().expect("load final config").unknown["test_counter"],
            12
        );
    }
}
