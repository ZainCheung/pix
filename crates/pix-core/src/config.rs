use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tempfile::Builder;
use thiserror::Error;
use uuid::Uuid;

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
        if let Some(url) = &self.preferences.relay_url
            && !(url.starts_with("wss://") || url.starts_with("ws://"))
        {
            return Err(ConfigError::Invalid(
                "relay_url must be a ws:// or wss:// endpoint",
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
    pub max_active_sessions: usize,
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

impl Default for Preferences {
    fn default() -> Self {
        Self {
            relay_enabled: true,
            relay_url: None,
            idle_timeout_seconds: 300,
            max_active_sessions: 4,
            pi_executable: None,
            unknown: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the platform-appropriate Pix configuration file location.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::NoConfigDirectory`] when the operating system
    /// does not expose a per-user configuration directory.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        ProjectDirs::from("", "", "Pix")
            .map(|dirs| dirs.config_dir().join("config.json"))
            .ok_or(ConfigError::NoConfigDirectory)
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

    /// Persists configuration using a same-directory temporary file, file
    /// fsync, atomic rename, and directory fsync on Unix.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if validation or any persistence step fails.
    /// A newer schema already on disk is never overwritten.
    pub fn save(&self, config: &HostConfig) -> Result<(), ConfigError> {
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

    use tempfile::tempdir;

    use super::{ConfigError, ConfigStore, HostConfig};

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
}
