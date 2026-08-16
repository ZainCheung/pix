use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};
use thiserror::Error;

use crate::host_environment::HostEnvironment;

/// The Pi minor release line verified against Pix's RPC adapter.
pub const SUPPORTED_PI_VERSION: &str = ">=0.84.1, <0.85.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiInstallation {
    pub executable: PathBuf,
    pub version: Version,
    pub supported: bool,
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

    /// Locates Pi and verifies the version and required RPC command-line flags.
    ///
    /// # Errors
    ///
    /// Returns [`PiError`] when Pi cannot be found, launched, parsed, or does
    /// not advertise the capabilities required by the adapter.
    pub fn inspect(&self) -> Result<PiInstallation, PiError> {
        let executable = match &self.explicit_path {
            Some(path) => resolve_executable(path)?,
            None => self
                .environment
                .find_executable("pi")
                .ok_or(PiError::NotFound)?,
        };
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
        let requirement = VersionReq::parse(SUPPORTED_PI_VERSION).map_err(PiError::SupportRange)?;
        Ok(PiInstallation {
            executable,
            supported: requirement.matches(&version),
            version,
        })
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
    for required_flag in [
        "--mode <mode>",
        "--approve",
        "--session <path|id>",
        "--session-id <id>",
    ] {
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
        return Ok(path.to_path_buf());
    }
    let canonical = fs::canonicalize(path).map_err(|source| PiError::Resolve {
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_file() {
        return Err(PiError::NotExecutable(canonical));
    }
    Ok(canonical)
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
    #[error("Pix was built with an invalid Pi support range: {0}")]
    SupportRange(semver::Error),
    #[error("Pi does not advertise required RPC capability {0}")]
    MissingCapability(&'static str),
}

#[cfg(test)]
mod tests {
    use semver::{Version, VersionReq};

    use super::SUPPORTED_PI_VERSION;

    #[cfg(unix)]
    fn write_fake_pi(directory: &std::path::Path) -> std::path::PathBuf {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join("pi");
        fs::write(
            &path,
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = \"--version\" ]; then\n",
                "  printf '0.84.1\\n'\n",
                "elif [ \"$1\" = \"--help\" ]; then\n",
                "  printf -- '--mode <mode> --approve --session <path|id> --session-id <id>\\n'\n",
                "fi\n",
                "exit 0\n",
            ),
        )
        .expect("write fake Pi");
        let mut permissions = fs::metadata(&path).expect("fake Pi metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("make fake Pi executable");
        path
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
    fn compatibility_range_is_explicit_and_narrow() {
        let requirement = VersionReq::parse(SUPPORTED_PI_VERSION).expect("valid range");
        assert!(requirement.matches(&Version::parse("0.84.1").expect("valid version")));
        assert!(requirement.matches(&Version::parse("0.84.2").expect("valid version")));
        assert!(!requirement.matches(&Version::parse("0.84.0").expect("valid version")));
        assert!(!requirement.matches(&Version::parse("0.85.0").expect("valid version")));
    }
}
