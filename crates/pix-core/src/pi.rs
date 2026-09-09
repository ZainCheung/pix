use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::host_environment::HostEnvironment;

/// The oldest Pi release supported by Pix's RPC adapter.
pub const MINIMUM_PI_VERSION: &str = "0.84.1";

/// CLI options required to launch the Pi RPC adapter.
pub const REQUIRED_PI_RPC_FLAGS: [&str; 4] = [
    "--mode <mode>",
    "--approve",
    "--session <path|id>",
    "--session-id <id>",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiInstallation {
    pub executable: PathBuf,
    pub version: Version,
    /// Whether the minimum version and required RPC CLI capabilities passed.
    pub supported: bool,
}

/// The safe, host-local outcome of the Pi compatibility preflight.
///
/// This report is a snapshot of the Pi that the Host actually checked while
/// starting its lifecycle.  It intentionally contains no command output or
/// other diagnostics.  The local CLI control surface may expose the optional
/// executable path, but this type is never part of the public phone protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiCompatibilityStatus {
    Compatible,
    UpdateRequired,
    MissingRequiredCapability,
    NotFound,
    CannotLaunch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiCompatibilityReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub compatibility: PiCompatibilityStatus,
}

impl PiCompatibilityReport {
    /// Creates a payload-safe snapshot from the one compatibility preflight
    /// performed by a Host lifecycle.
    #[must_use]
    pub fn from_preflight(preflight: &Result<PiInstallation, PiCompatibilityError>) -> Self {
        match preflight {
            Ok(installation) => Self {
                executable: Some(installation.executable.clone()),
                version: Some(installation.version.to_string()),
                compatibility: if installation.is_compatible() {
                    PiCompatibilityStatus::Compatible
                } else {
                    PiCompatibilityStatus::UpdateRequired
                },
            },
            Err(error) => Self {
                executable: None,
                version: match error {
                    PiCompatibilityError::TooOld { found } => Some(found.to_string()),
                    PiCompatibilityError::MissingRequiredCapability
                    | PiCompatibilityError::NotFound
                    | PiCompatibilityError::CannotLaunch => None,
                },
                compatibility: match error {
                    PiCompatibilityError::TooOld { .. } => PiCompatibilityStatus::UpdateRequired,
                    PiCompatibilityError::MissingRequiredCapability => {
                        PiCompatibilityStatus::MissingRequiredCapability
                    }
                    PiCompatibilityError::NotFound => PiCompatibilityStatus::NotFound,
                    PiCompatibilityError::CannotLaunch => PiCompatibilityStatus::CannotLaunch,
                },
            },
        }
    }

    /// Returns the legacy boolean field used by the CLI status envelope.
    #[must_use]
    pub const fn supported(&self) -> Option<bool> {
        match self.compatibility {
            PiCompatibilityStatus::Compatible => Some(true),
            PiCompatibilityStatus::UpdateRequired => Some(false),
            PiCompatibilityStatus::MissingRequiredCapability
            | PiCompatibilityStatus::NotFound
            | PiCompatibilityStatus::CannotLaunch => None,
        }
    }
}

impl PiInstallation {
    /// Returns whether this installation satisfies Pix's minimum-only
    /// compatibility policy. Newer Pi versions remain eligible when the RPC
    /// capability probe succeeds.
    #[must_use]
    pub const fn is_compatible(&self) -> bool {
        self.supported
    }
}

#[derive(Debug, Clone, Default)]
pub struct PiProbe {
    explicit_path: Option<PathBuf>,
    environment: HostEnvironment,
}

impl PiProbe {
    #[must_use]
    pub fn new(explicit_path: Option<PathBuf>) -> Self {
        Self {
            explicit_path,
            environment: HostEnvironment::from_process(),
        }
    }

    /// Discovers and probes Pi inside `environment` instead of the process
    /// environment. GUI-launched hosts pass the resolved login shell
    /// environment so version-manager installations (mise, nvm, asdf, volta,
    /// bun) behave exactly as they do in the user's terminal.
    #[must_use]
    pub fn with_environment(mut self, environment: HostEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Resolves the executable without starting Pi.
    ///
    /// This is the cheap discovery path used by metadata-only commands such
    /// as `pix pi show`.  It deliberately performs no `--version` or `--help`
    /// invocation; callers that need compatibility must use [`Self::inspect`]
    /// or [`Self::inspect_compatibility`] explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`PiError`] when the configured path cannot be resolved or Pi
    /// cannot be found on the selected environment's `PATH`.
    pub fn resolve_executable(&self) -> Result<PathBuf, PiError> {
        if let Some(path) = &self.explicit_path {
            return resolve_executable(path);
        }
        let path = self
            .environment
            .find_executable("pi")
            .ok_or(PiError::NotFound)?;
        resolve_executable(&path)
    }

    /// Locates Pi and verifies the version and required RPC command-line flags.
    ///
    /// # Errors
    ///
    /// Returns [`PiError`] when Pi cannot be found, launched, parsed, or does
    /// not advertise the capabilities required by the adapter.
    pub fn inspect(&self) -> Result<PiInstallation, PiError> {
        let executable = self.resolve_executable()?;
        let version_output = self
            .environment
            .command(&executable)
            .arg("--version")
            .output()
            .map_err(|source| PiError::Launch {
                path: executable.clone(),
                source,
            })?;
        if !version_output.status.success() {
            return Err(PiError::CommandFailed {
                path: executable,
                command: "--version",
                status: version_output.status.code(),
            });
        }
        let raw_version = String::from_utf8_lossy(&version_output.stdout);
        let version = Version::parse(raw_version.trim()).map_err(|source| PiError::Version {
            value: raw_version.trim().to_owned(),
            source,
        })?;

        verify_rpc_flags(&executable, &self.environment)?;
        let minimum = Version::parse(MINIMUM_PI_VERSION).map_err(PiError::MinimumVersion)?;
        Ok(PiInstallation {
            executable,
            supported: version >= minimum,
            version,
        })
    }

    /// Runs the full compatibility preflight used before a host starts a Pi
    /// session. The returned error is deliberately path-free so it can be
    /// mapped to a safe phone-facing message.
    ///
    /// # Errors
    ///
    /// Returns [`PiCompatibilityError`] when Pi cannot be found, launched, or
    /// does not satisfy the minimum version/capability contract.
    pub fn inspect_compatibility(&self) -> Result<PiInstallation, PiCompatibilityError> {
        let installation = self.inspect().map_err(PiCompatibilityError::from)?;
        if installation.is_compatible() {
            Ok(installation)
        } else {
            Err(PiCompatibilityError::TooOld {
                found: installation.version,
            })
        }
    }
}

fn verify_rpc_flags(executable: &Path, environment: &HostEnvironment) -> Result<(), PiError> {
    let output = environment
        .command(executable)
        .arg("--help")
        .output()
        .map_err(|source| PiError::Launch {
            path: executable.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(PiError::CommandFailed {
            path: executable.to_path_buf(),
            command: "--help",
            status: output.status.code(),
        });
    }
    let help = String::from_utf8_lossy(&output.stdout);
    for required_flag in REQUIRED_PI_RPC_FLAGS {
        if !help.contains(required_flag) {
            return Err(PiError::MissingCapability(required_flag));
        }
    }
    Ok(())
}

fn resolve_executable(path: &Path) -> Result<PathBuf, PiError> {
    // Keep an explicitly configured path exactly as given when it already
    // points at a file. Version-manager shims are symlinks whose target
    // dispatches on `argv[0]`; canonicalizing them would probe the wrong
    // program.
    if path.is_file() {
        if !is_executable_file(path) {
            return Err(PiError::NotExecutable(path.to_path_buf()));
        }
        return Ok(path.to_path_buf());
    }
    let canonical = fs::canonicalize(path).map_err(|source| PiError::Resolve {
        path: path.to_path_buf(),
        source,
    })?;
    if !is_executable_file(&canonical) {
        return Err(PiError::NotExecutable(canonical));
    }
    Ok(canonical)
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Error)]
pub enum PiError {
    #[error("Pi executable was not found on PATH")]
    NotFound,
    #[error("failed to resolve Pi executable {path}: {source}")]
    Resolve { path: PathBuf, source: io::Error },
    #[error("Pi path is not a file: {0}")]
    NotExecutable(PathBuf),
    #[error("failed to launch Pi executable {path}: {source}")]
    Launch { path: PathBuf, source: io::Error },
    #[error("Pi {command} failed for {path} with status {status:?}")]
    CommandFailed {
        path: PathBuf,
        command: &'static str,
        status: Option<i32>,
    },
    #[error("could not parse Pi version {value:?}: {source}")]
    Version {
        value: String,
        source: semver::Error,
    },
    #[error("Pix was built with an invalid minimum Pi version: {0}")]
    MinimumVersion(semver::Error),
    #[error("Pi does not advertise required RPC capability {0}")]
    MissingCapability(&'static str),
}

/// Safe categories for a host-side Pi compatibility preflight. These values
/// intentionally omit executable paths and command output before they reach a
/// remote client or the persistent host diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PiCompatibilityError {
    #[error("Pi {found} is too old")]
    TooOld { found: Version },
    #[error("installed Pi is missing a required RPC capability")]
    MissingRequiredCapability,
    #[error("Pi executable was not found")]
    NotFound,
    #[error("Pi could not be launched")]
    CannotLaunch,
}

impl From<PiError> for PiCompatibilityError {
    fn from(error: PiError) -> Self {
        match error {
            PiError::NotFound | PiError::Resolve { .. } | PiError::NotExecutable(_) => {
                Self::NotFound
            }
            PiError::MissingCapability(_) => Self::MissingRequiredCapability,
            PiError::Launch { .. }
            | PiError::CommandFailed { .. }
            | PiError::Version { .. }
            | PiError::MinimumVersion(_) => Self::CannotLaunch,
        }
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::{MINIMUM_PI_VERSION, REQUIRED_PI_RPC_FLAGS};

    #[cfg(unix)]
    fn write_fake_pi_version(
        directory: &std::path::Path,
        version: &str,
        help: &str,
    ) -> std::path::PathBuf {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join("pi");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf '{version}\\n'\nelif [ \"$1\" = \"--help\" ]; then\n  printf -- '{help}\\n'\nfi\nexit 0\n"
        );
        fs::write(&path, script).expect("write fake Pi");
        let mut permissions = fs::metadata(&path).expect("fake Pi metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("make fake Pi executable");
        path
    }

    #[cfg(unix)]
    fn write_fake_pi(directory: &std::path::Path) -> std::path::PathBuf {
        write_fake_pi_version(directory, "0.84.1", &REQUIRED_PI_RPC_FLAGS.join(" "))
    }

    #[cfg(unix)]
    #[test]
    fn resolving_executable_does_not_invoke_pi_capability_commands() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary Pi directory");
        let counter = directory.path().join("invocations");
        let path = directory.path().join("pi");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version|--help) printf '%s\\n' \"$1\" >> '{}' ;;\nesac\n",
            counter.display()
        );
        fs::write(&path, script).expect("write fake Pi");
        let mut permissions = fs::metadata(&path).expect("fake Pi metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("make fake Pi executable");

        let resolved = super::PiProbe::new(Some(path.clone()))
            .resolve_executable()
            .expect("resolve fake Pi");

        assert_eq!(resolved, path);
        assert!(
            !counter.exists(),
            "resolution invoked Pi capability commands"
        );
    }

    #[cfg(unix)]
    #[test]
    fn probe_discovers_pi_through_the_resolved_environment() {
        use std::ffi::OsString;

        use crate::host_environment::HostEnvironment;

        let directory = tempfile::tempdir().expect("temporary PATH directory");
        let fake_pi = write_fake_pi(directory.path());
        let environment = HostEnvironment::captured_for_tests(
            std::path::PathBuf::from("/bin/zsh"),
            vec![(
                OsString::from("PATH"),
                directory.path().as_os_str().to_owned(),
            )],
        );

        let installation = super::PiProbe::new(None)
            .with_environment(environment)
            .inspect()
            .expect("probe fake Pi");

        assert_eq!(installation.executable, fake_pi);
        assert_eq!(installation.version, Version::new(0, 84, 1));
        assert!(installation.supported);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_shim_path_is_probed_without_canonicalization() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary shim directory");
        let target = write_fake_pi(directory.path());
        let shim = directory.path().join("pi-shim");
        symlink(&target, &shim).expect("create shim");

        let installation = super::PiProbe::new(Some(shim.clone()))
            .inspect()
            .expect("probe shim");

        assert_eq!(installation.executable, shim);
        assert_ne!(installation.executable, target);
    }

    #[test]
    fn compatibility_policy_is_minimum_only() {
        let minimum = Version::parse(MINIMUM_PI_VERSION).expect("valid minimum");
        for version in ["0.84.1", "0.84.4", "0.85.1", "0.99.0", "1.0.0"] {
            assert!(Version::parse(version).expect("valid version") >= minimum);
        }
        assert!(Version::parse("0.84.0").expect("valid version") < minimum);
    }

    #[test]
    fn compatibility_report_preserves_safe_preflight_categories() {
        let old = super::PiCompatibilityReport::from_preflight(&Err(
            super::PiCompatibilityError::TooOld {
                found: Version::parse("0.83.0").expect("valid version"),
            },
        ));
        assert_eq!(
            old.compatibility,
            super::PiCompatibilityStatus::UpdateRequired
        );
        assert_eq!(old.version.as_deref(), Some("0.83.0"));
        assert_eq!(old.supported(), Some(false));

        let missing = super::PiCompatibilityReport::from_preflight(&Err(
            super::PiCompatibilityError::NotFound,
        ));
        assert_eq!(
            missing.compatibility,
            super::PiCompatibilityStatus::NotFound
        );
        assert!(missing.version.is_none());
        assert!(missing.supported().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn probe_accepts_future_versions_with_required_capabilities() {
        for version in ["0.84.4", "0.85.1", "0.99.0", "1.0.0"] {
            let directory = tempfile::tempdir().expect("temporary Pi directory");
            let path =
                write_fake_pi_version(directory.path(), version, &REQUIRED_PI_RPC_FLAGS.join(" "));
            let installation = super::PiProbe::new(Some(path))
                .inspect()
                .expect("probe future Pi version");
            assert!(installation.is_compatible(), "Pi {version}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn future_version_without_required_capabilities_is_incompatible() {
        let directory = tempfile::tempdir().expect("temporary Pi directory");
        let path = write_fake_pi_version(directory.path(), "0.99.0", "--mode <mode>");

        let error = super::PiProbe::new(Some(path))
            .inspect_compatibility()
            .expect_err("missing RPC capability");
        assert_eq!(
            error,
            super::PiCompatibilityError::MissingRequiredCapability
        );
    }

    #[cfg(unix)]
    #[test]
    fn versions_below_minimum_are_rejected_by_compatibility_preflight() {
        let directory = tempfile::tempdir().expect("temporary Pi directory");
        let path =
            write_fake_pi_version(directory.path(), "0.84.0", &REQUIRED_PI_RPC_FLAGS.join(" "));

        let error = super::PiProbe::new(Some(path))
            .inspect_compatibility()
            .expect_err("old Pi version");
        assert_eq!(
            error,
            super::PiCompatibilityError::TooOld {
                found: Version::parse("0.84.0").expect("valid version"),
            }
        );
    }
}
