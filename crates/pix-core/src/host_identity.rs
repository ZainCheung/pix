//! Host static identity persistence.
//!
//! The JSON configuration intentionally excludes private key material. This
//! small store is the documented mode-0600 fallback used by Linux and by
//! macOS migration/development; the macOS CLI prefers Keychain without
//! changing the secure-channel API.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use tempfile::Builder;
use thiserror::Error;

/// A validated long-term host identity for Noise XX/IK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIdentityKey {
    pub private_key: Vec<u8>,
    pub public_key: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct HostIdentityStore {
    path: PathBuf,
    secret_service_host_id: Option<String>,
    #[cfg(test)]
    secret_service_tool: Option<PathBuf>,
    #[cfg(target_os = "macos")]
    keychain_host_id: Option<String>,
}

impl HostIdentityStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            secret_service_host_id: None,
            #[cfg(test)]
            secret_service_tool: None,
            #[cfg(target_os = "macos")]
            keychain_host_id: None,
        }
    }

    /// Uses Secret Service (through the `secret-tool` CLI) as the preferred
    /// backend for this host identity. The file path remains the documented
    /// mode-0600 fallback.
    #[must_use]
    pub fn with_secret_service_host_id(mut self, host_id: impl Into<String>) -> Self {
        self.secret_service_host_id = Some(host_id.into());
        self
    }

    #[cfg(test)]
    fn with_secret_service_command_for_tests(mut self, command: impl Into<PathBuf>) -> Self {
        self.secret_service_tool = Some(command.into());
        self
    }

    /// Uses the macOS Keychain as the preferred backend for this host
    /// identity. The file path remains a mode-0600 migration/fallback path so
    /// an unavailable Keychain cannot rotate an already paired host identity.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn with_keychain_host_id(mut self, host_id: impl Into<String>) -> Self {
        self.keychain_host_id = Some(host_id.into());
        self
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads a persisted identity or creates one atomically on first use.
    ///
    /// # Errors
    ///
    /// Returns [`HostIdentityError`] for malformed state, secure randomness,
    /// or filesystem failures.
    #[allow(clippy::too_many_lines)]
    pub fn load_or_create(&self) -> Result<HostIdentityKey, HostIdentityError> {
        #[cfg(target_os = "macos")]
        let keychain_error = if let Some(host_id) = &self.keychain_host_id {
            match load_keychain_identity(host_id) {
                Ok(Some(identity)) => return Ok(identity),
                Ok(None) => None,
                Err(error) => {
                    eprintln!(
                        "Pix: macOS Keychain is unavailable for host identity; \
                         using the existing mode-0600 key file if present ({error})."
                    );
                    Some(error)
                }
            }
        } else {
            None
        };

        let mut secret_service_command = None;
        let mut secret_service_unavailable = None;
        let mut secret_service_corrupt = None;
        if let Some(host_id) = &self.secret_service_host_id
            && let Some(command) = self.secret_service_command()
        {
            match load_secret_service_identity(host_id, &command) {
                Ok(Some(identity)) => {
                    // Keep a local recovery copy for a temporary keyring
                    // outage. It is never used as a second source of
                    // truth while Secret Service is healthy.
                    if let Err(error) = self.ensure_file_fallback(&identity) {
                        eprintln!(
                            "Pix: could not refresh the mode-0600 host identity fallback ({error})."
                        );
                    }
                    return Ok(identity);
                }
                Ok(None) => secret_service_command = Some(command),
                Err(error) => {
                    eprintln!(
                        "Pix: Secret Service is unavailable for host identity; \
                         using the mode-0600 key file if present ({error})."
                    );
                    if error.is_secret_service_corruption() {
                        secret_service_corrupt = Some(error);
                    } else {
                        secret_service_unavailable = Some(error);
                    }
                }
            }
        }

        match fs::read(&self.path) {
            Ok(bytes) => {
                let identity = decode(&bytes)?;
                #[cfg(target_os = "macos")]
                if let Some(host_id) = &self.keychain_host_id {
                    if keychain_error.is_none() {
                        match store_keychain_identity(host_id, &identity) {
                            Ok(()) => {
                                if let Err(error) = fs::remove_file(&self.path) {
                                    eprintln!(
                                        "Pix: could not remove the migrated host identity file \
                                         {} ({error}); Keychain remains authoritative.",
                                        self.path.display()
                                    );
                                }
                                return Ok(identity);
                            }
                            Err(error) => eprintln!(
                                "Pix: could not migrate host identity into macOS Keychain; \
                                 keeping the mode-0600 key file ({error})."
                            ),
                        }
                    }
                    return Ok(identity);
                }
                if let (Some(host_id), Some(command)) =
                    (&self.secret_service_host_id, &secret_service_command)
                {
                    // Migrate an existing file-backed identity into Secret
                    // Service once so later loads prefer the system keyring.
                    if let Err(error) = store_secret_service_identity(host_id, command, &identity) {
                        eprintln!(
                            "Pix: could not migrate host identity into Secret Service; \
                             keeping the mode-0600 key file ({error})."
                        );
                    }
                }
                Ok(identity)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                #[cfg(target_os = "macos")]
                if let Some(error) = keychain_error {
                    return Err(error);
                }
                if let Some(error) = secret_service_corrupt.or(secret_service_unavailable) {
                    // A keyring error with no recovery file must not rotate
                    // the host key. The paired phone would otherwise become
                    // permanently unknown after a transient outage.
                    return Err(error);
                }
                let generated = pix_wire::generate_static_keypair()?;
                let identity = HostIdentityKey {
                    private_key: generated.private_key,
                    public_key: generated.public_key,
                };
                if let (Some(host_id), Some(command)) =
                    (&self.secret_service_host_id, &secret_service_command)
                {
                    match store_secret_service_identity(host_id, command, &identity) {
                        Ok(()) => {}
                        Err(error) => {
                            eprintln!(
                                "Pix: could not store host identity in Secret Service; \
                                 falling back to the mode-0600 key file ({error})."
                            );
                        }
                    }
                }
                #[cfg(target_os = "macos")]
                if let Some(host_id) = &self.keychain_host_id {
                    match store_keychain_identity(host_id, &identity) {
                        Ok(()) => return Ok(identity),
                        Err(error) => eprintln!(
                            "Pix: could not store host identity in macOS Keychain; \
                             falling back to the mode-0600 key file ({error})."
                        ),
                    }
                }
                // Linux always retains a mode-0600 recovery copy, including
                // after a successful Secret Service store.
                self.save(&identity)?;
                Ok(identity)
            }
            Err(source) => Err(HostIdentityError::Read {
                path: self.path.clone(),
                source,
            }),
        }
    }

    fn ensure_file_fallback(&self, identity: &HostIdentityKey) -> Result<(), HostIdentityError> {
        let fallback_matches = fs::read(&self.path)
            .ok()
            .and_then(|bytes| decode(&bytes).ok())
            .is_some_and(|fallback| fallback == *identity);
        if fallback_matches {
            Ok(())
        } else {
            self.save(identity)
        }
    }

    #[allow(clippy::unused_self)]
    fn secret_service_command(&self) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(command) = &self.secret_service_tool {
            return Some(command.clone());
        }
        trusted_secret_tool()
    }

    fn save(&self, identity: &HostIdentityKey) -> Result<(), HostIdentityError> {
        validate(identity)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| HostIdentityError::InvalidPath {
                path: self.path.clone(),
            })?;
        fs::create_dir_all(parent).map_err(|source| HostIdentityError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
        let mut temporary = Builder::new()
            .prefix(".pix-host-identity-")
            .tempfile_in(parent)
            .map_err(|source| HostIdentityError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|source| HostIdentityError::Write {
                    path: temporary.path().to_path_buf(),
                    source,
                })?;
        }
        temporary
            .write_all(&encode(identity))
            .and_then(|()| temporary.as_file_mut().sync_all())
            .map_err(|source| HostIdentityError::Write {
                path: temporary.path().to_path_buf(),
                source,
            })?;
        temporary
            .persist(&self.path)
            .map_err(|error| HostIdentityError::Write {
                path: self.path.clone(),
                source: error.error,
            })?;
        #[cfg(unix)]
        {
            let directory = OpenOptions::new()
                .read(true)
                .open(parent)
                .map_err(|source| HostIdentityError::Write {
                    path: parent.to_path_buf(),
                    source,
                })?;
            directory
                .sync_all()
                .map_err(|source| HostIdentityError::Write {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
        Ok(())
    }
}

const SECRET_SERVICE_ATTRIBUTE: &str = "pix_host_id";
const SECRET_SERVICE_LABEL: &str = "Pix host identity";

#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "com.deepoke.pix.host-identity";

#[cfg(target_os = "macos")]
fn load_keychain_identity(host_id: &str) -> Result<Option<HostIdentityKey>, HostIdentityError> {
    use security_framework::passwords::generic_password;
    use security_framework_sys::base::errSecItemNotFound;

    match generic_password(
        security_framework::passwords::PasswordOptions::new_generic_password(
            KEYCHAIN_SERVICE,
            host_id,
        ),
    ) {
        Ok(bytes) => decode(&bytes)
            .map(Some)
            .map_err(|error| HostIdentityError::Keychain {
                host_id: host_id.to_owned(),
                message: format!("stored identity is invalid: {error}"),
            }),
        Err(error) if error.code() == errSecItemNotFound => Ok(None),
        Err(error) => Err(HostIdentityError::Keychain {
            host_id: host_id.to_owned(),
            message: error.to_string(),
        }),
    }
}

#[cfg(target_os = "macos")]
fn store_keychain_identity(
    host_id: &str,
    identity: &HostIdentityKey,
) -> Result<(), HostIdentityError> {
    use security_framework::passwords::set_generic_password;

    set_generic_password(KEYCHAIN_SERVICE, host_id, &encode(identity)).map_err(|error| {
        HostIdentityError::Keychain {
            host_id: host_id.to_owned(),
            message: error.to_string(),
        }
    })
}

const SECRET_SERVICE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SECRET_SERVICE_OUTPUT: usize = 4 * 1024;

/// Resolves only root-owned, non-writable absolute paths. In particular, a
/// user-controlled PATH entry can never replace the keyring helper.
fn trusted_secret_tool() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        for candidate in ["/usr/bin/secret-tool", "/bin/secret-tool"] {
            let path = Path::new(candidate);
            let Ok(metadata) = fs::symlink_metadata(path) else {
                continue;
            };
            let mode = metadata.permissions().mode();
            if metadata.file_type().is_file()
                && metadata.uid() == 0
                && mode & 0o022 == 0
                && mode & 0o111 != 0
            {
                return Some(path.to_path_buf());
            }
        }
    }
    None
}

struct SecretToolOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_secret_tool(
    host_id: &str,
    operation: &str,
    command: &Path,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> Result<SecretToolOutput, HostIdentityError> {
    let mut child = Command::new(command)
        .args(arguments)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| HostIdentityError::SecretService {
            host_id: host_id.to_owned(),
            source,
        })?;

    if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| HostIdentityError::SecretService {
                host_id: host_id.to_owned(),
                source: io::Error::other("secret-tool stdin was not available"),
            })?;
        stdin
            .write_all(input)
            .map_err(|source| HostIdentityError::SecretService {
                host_id: host_id.to_owned(),
                source,
            })?;
    }

    let deadline = Instant::now() + SECRET_SERVICE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostIdentityError::SecretServiceTimeout {
                    host_id: host_id.to_owned(),
                    operation: operation.to_owned(),
                });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostIdentityError::SecretService {
                    host_id: host_id.to_owned(),
                    source,
                });
            }
        }
    };

    let mut stdout = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        pipe.take(u64::try_from(MAX_SECRET_SERVICE_OUTPUT + 1).unwrap_or(u64::MAX))
            .read_to_end(&mut stdout)
            .map_err(|source| HostIdentityError::SecretService {
                host_id: host_id.to_owned(),
                source,
            })?;
    }
    let mut stderr = Vec::new();
    if let Some(pipe) = child.stderr.take() {
        pipe.take(1024).read_to_end(&mut stderr).map_err(|source| {
            HostIdentityError::SecretService {
                host_id: host_id.to_owned(),
                source,
            }
        })?;
    }
    if stdout.len() > MAX_SECRET_SERVICE_OUTPUT {
        return Err(HostIdentityError::SecretService {
            host_id: host_id.to_owned(),
            source: io::Error::other("secret-tool output exceeded the safety limit"),
        });
    }
    Ok(SecretToolOutput {
        status,
        stdout,
        stderr,
    })
}

fn load_secret_service_identity(
    host_id: &str,
    command: &Path,
) -> Result<Option<HostIdentityKey>, HostIdentityError> {
    let output = run_secret_tool(
        host_id,
        "lookup",
        command,
        &["lookup", SECRET_SERVICE_ATTRIBUTE, host_id],
        None,
    )?;
    if !output.status.success() {
        // `secret-tool lookup` uses an empty stderr/non-zero status for a
        // missing item. A diagnostic on stderr means the keyring itself is
        // unavailable and must not trigger identity rotation.
        if output.stderr.is_empty() {
            return Ok(None);
        }
        return Err(HostIdentityError::SecretService {
            host_id: host_id.to_owned(),
            source: io::Error::other("secret-tool lookup failed"),
        });
    }
    let encoded = String::from_utf8(output.stdout)
        .map_err(|source| HostIdentityError::SecretServiceCorrupt {
            host_id: host_id.to_owned(),
            message: format!("invalid UTF-8: {source}"),
        })?
        .trim()
        .to_owned();
    if encoded.is_empty() {
        return Err(HostIdentityError::SecretServiceCorrupt {
            host_id: host_id.to_owned(),
            message: "empty stored identity".to_owned(),
        });
    }
    let private_key = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|source| HostIdentityError::SecretServiceCorrupt {
            host_id: host_id.to_owned(),
            message: format!("invalid base64: {source}"),
        })?;
    let public_key = pix_wire::static_public_key(&private_key).map_err(|error| {
        HostIdentityError::SecretServiceCorrupt {
            host_id: host_id.to_owned(),
            message: format!("invalid private key: {error}"),
        }
    })?;
    let identity = HostIdentityKey {
        private_key,
        public_key,
    };
    validate(&identity).map_err(|error| HostIdentityError::SecretServiceCorrupt {
        host_id: host_id.to_owned(),
        message: error.to_string(),
    })?;
    Ok(Some(identity))
}

fn store_secret_service_identity(
    host_id: &str,
    command: &Path,
    identity: &HostIdentityKey,
) -> Result<(), HostIdentityError> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(&identity.private_key);
    let output = run_secret_tool(
        host_id,
        "store",
        command,
        &[
            "store",
            "--label",
            SECRET_SERVICE_LABEL,
            SECRET_SERVICE_ATTRIBUTE,
            host_id,
        ],
        Some(encoded.as_bytes()),
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(HostIdentityError::SecretService {
            host_id: host_id.to_owned(),
            source: io::Error::other("secret-tool store failed"),
        })
    }
}

fn encode(identity: &HostIdentityKey) -> Vec<u8> {
    [
        identity.private_key.as_slice(),
        identity.public_key.as_slice(),
    ]
    .concat()
}

fn decode(bytes: &[u8]) -> Result<HostIdentityKey, HostIdentityError> {
    if bytes.len() != 64 {
        return Err(HostIdentityError::InvalidLength(bytes.len()));
    }
    let identity = HostIdentityKey {
        private_key: bytes[..32].to_vec(),
        public_key: bytes[32..].to_vec(),
    };
    validate(&identity)?;
    Ok(identity)
}

fn validate(identity: &HostIdentityKey) -> Result<(), HostIdentityError> {
    if identity.private_key.len() != 32 || identity.public_key.len() != 32 {
        return Err(HostIdentityError::InvalidLength(
            identity.private_key.len() + identity.public_key.len(),
        ));
    }
    if pix_wire::static_public_key(&identity.private_key)? != identity.public_key {
        return Err(HostIdentityError::PublicKeyMismatch);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum HostIdentityError {
    #[error("failed to read host identity {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("failed to write host identity {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("host identity path is invalid: {path}")]
    InvalidPath { path: PathBuf },
    #[error("host identity has {0} bytes; expected 64")]
    InvalidLength(usize),
    #[error("host identity public key does not match its private key")]
    PublicKeyMismatch,
    #[error("Secret Service operation failed for host {host_id}: {source}")]
    SecretService { host_id: String, source: io::Error },
    #[error("Secret Service {operation} timed out for host {host_id}")]
    SecretServiceTimeout { host_id: String, operation: String },
    #[error("Secret Service stored identity is invalid for host {host_id}: {message}")]
    SecretServiceCorrupt { host_id: String, message: String },
    #[error("Secret Service returned invalid UTF-8 for host {host_id}: {source}")]
    SecretServiceEncoding {
        host_id: String,
        source: std::string::FromUtf8Error,
    },
    #[error("Secret Service returned invalid base64 for host {host_id}: {source}")]
    SecretServiceDecode { host_id: String, source: io::Error },
    #[cfg(target_os = "macos")]
    #[error("macOS Keychain operation failed for host {host_id}: {message}")]
    Keychain { host_id: String, message: String },
    #[error(transparent)]
    Wire(#[from] pix_wire::WireError),
}

impl HostIdentityError {
    fn is_secret_service_corruption(&self) -> bool {
        matches!(
            self,
            Self::SecretServiceCorrupt { .. }
                | Self::SecretServiceEncoding { .. }
                | Self::SecretServiceDecode { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{HostIdentityKey, HostIdentityStore};

    #[test]
    fn identity_is_stable_and_private_material_is_not_json() {
        let directory = tempdir().expect("identity directory");
        let store = HostIdentityStore::new(directory.path().join("host.key"));
        let first = store.load_or_create().expect("create identity");
        let second = store.load_or_create().expect("reload identity");
        assert_eq!(first, second);
        assert_eq!(fs::read(store.path()).expect("read identity").len(), 64);
    }

    #[test]
    fn malformed_identity_fails_closed() {
        let directory = tempdir().expect("identity directory");
        let path = directory.path().join("host.key");
        fs::write(&path, [1_u8; 3]).expect("write malformed identity");
        assert!(HostIdentityStore::new(path).load_or_create().is_err());
    }

    #[test]
    fn keyring_identity_replaces_a_valid_stale_fallback() {
        let directory = tempdir().expect("identity directory");
        let path = directory.path().join("host.key");
        let fallback = HostIdentityStore::new(&path)
            .load_or_create()
            .expect("create fallback identity");
        let generated = pix_wire::generate_static_keypair().expect("generate keyring identity");
        let keyring_identity = HostIdentityKey {
            private_key: generated.private_key,
            public_key: generated.public_key,
        };

        HostIdentityStore::new(&path)
            .ensure_file_fallback(&keyring_identity)
            .expect("reconcile fallback");
        assert_ne!(fallback, keyring_identity);
        assert_eq!(
            HostIdentityStore::new(path)
                .load_or_create()
                .expect("reload reconciled fallback"),
            keyring_identity
        );
    }

    #[cfg(unix)]
    fn executable_script(
        directory: &std::path::Path,
        name: &str,
        body: &str,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join(name);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n"))
            .expect("write fake Secret Service");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("make fake Secret Service executable");
        path
    }

    #[cfg(unix)]
    #[test]
    fn malformed_keyring_record_uses_the_valid_fallback_without_rotation() {
        let directory = tempdir().expect("identity directory");
        let path = directory.path().join("host.key");
        let expected = HostIdentityStore::new(&path)
            .load_or_create()
            .expect("create fallback identity");
        let fake = executable_script(
            directory.path(),
            "secret-tool-corrupt",
            "if [ \"$1\" = lookup ]; then printf '%s\\n' not-base64; exit 0; fi\nexit 0",
        );

        let actual = HostIdentityStore::new(path)
            .with_secret_service_host_id("test-host")
            .with_secret_service_command_for_tests(fake)
            .load_or_create()
            .expect("fallback identity remains usable");
        assert_eq!(actual, expected);
    }

    #[cfg(unix)]
    #[test]
    fn keyring_outage_is_bounded_and_does_not_rotate_identity() {
        use std::time::{Duration, Instant};

        let directory = tempdir().expect("identity directory");
        let path = directory.path().join("host.key");
        let expected = HostIdentityStore::new(&path)
            .load_or_create()
            .expect("create fallback identity");
        let fake = executable_script(directory.path(), "secret-tool-hangs", "while :; do :; done");

        let started = Instant::now();
        let actual = HostIdentityStore::new(path)
            .with_secret_service_host_id("test-host")
            .with_secret_service_command_for_tests(fake)
            .load_or_create()
            .expect("fallback identity remains usable");
        assert_eq!(actual, expected);
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[cfg(unix)]
    #[test]
    fn successful_keyring_store_keeps_a_recovery_copy() {
        let directory = tempdir().expect("identity directory");
        let path = directory.path().join("host.key");
        let fake = executable_script(
            directory.path(),
            "secret-tool-store",
            "if [ \"$1\" = lookup ]; then exit 1; fi\ncat >/dev/null\nexit 0",
        );

        let expected = HostIdentityStore::new(&path)
            .with_secret_service_host_id("test-host")
            .with_secret_service_command_for_tests(fake)
            .load_or_create()
            .expect("create identity through fake keyring");
        assert_eq!(fs::read(&path).expect("recovery copy").len(), 64);
        assert_eq!(
            HostIdentityStore::new(path)
                .load_or_create()
                .expect("reload"),
            expected
        );
    }
}
