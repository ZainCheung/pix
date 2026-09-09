//! Lightweight identity checks for the executable backing the persistent host.
//!
//! A service manager keeps a process alive when its path is replaced on disk.
//! The monitor lets the process notice that replacement and exit normally so
//! the service manager can launch the new image at the same path.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableFileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    length: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutableCheck {
    Unchanged,
    Missing,
    Replaced,
}

#[derive(Debug)]
pub(crate) struct ExecutableIdentityMonitor {
    path: PathBuf,
    initial: ExecutableFileIdentity,
}

impl ExecutableIdentityMonitor {
    /// Captures the current process image's path and filesystem identity.
    pub(crate) fn current() -> Option<Self> {
        std::env::current_exe()
            .ok()
            .and_then(|path| Self::new(&path))
    }

    /// Captures an executable identity for tests and callers with a known path.
    pub(crate) fn new(path: &Path) -> Option<Self> {
        let initial = capture_identity(path)?;
        Some(Self {
            path: path.to_path_buf(),
            initial,
        })
    }

    /// Checks whether the original path now points to a different valid
    /// executable. A transiently absent or non-executable path is deliberately
    /// treated as unknown so an interrupted atomic replacement cannot stop the
    /// old host before the new image is ready.
    pub(crate) fn check(&self) -> ExecutableCheck {
        let Some(current) = capture_identity(&self.path) else {
            return ExecutableCheck::Missing;
        };
        if current == self.initial {
            ExecutableCheck::Unchanged
        } else {
            ExecutableCheck::Replaced
        }
    }
}

fn capture_identity(path: &Path) -> Option<ExecutableFileIdentity> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return None;
    }
    Some(ExecutableFileIdentity {
        #[cfg(unix)]
        device: metadata_device(&metadata),
        #[cfg(unix)]
        inode: metadata_inode(&metadata),
        length: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn metadata_device(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.dev()
}

#[cfg(unix)]
fn metadata_inode(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.ino()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{ExecutableCheck, ExecutableIdentityMonitor};

    #[cfg(unix)]
    fn make_executable(path: &Path, contents: &[u8]) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, contents).expect("write executable fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("mark executable fixture");
    }

    #[cfg(not(unix))]
    fn make_executable(path: &Path, contents: &[u8]) {
        fs::write(path, contents).expect("write executable fixture");
    }

    fn replace_atomically(path: &Path, contents: &[u8]) {
        let replacement = path.with_extension("replacement");
        make_executable(&replacement, contents);
        fs::rename(replacement, path).expect("atomically replace executable fixture");
    }

    #[test]
    fn same_executable_does_not_request_restart() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("pix");
        make_executable(&path, b"old image");
        let monitor = ExecutableIdentityMonitor::new(&path).expect("capture initial identity");

        assert_eq!(monitor.check(), ExecutableCheck::Unchanged);
    }

    #[test]
    fn atomic_replacement_requests_restart() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("pix");
        make_executable(&path, b"old image");
        let monitor = ExecutableIdentityMonitor::new(&path).expect("capture initial identity");

        replace_atomically(&path, b"new image");

        assert_eq!(monitor.check(), ExecutableCheck::Replaced);
    }

    #[test]
    fn temporary_missing_executable_does_not_request_restart() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("pix");
        make_executable(&path, b"old image");
        let monitor = ExecutableIdentityMonitor::new(&path).expect("capture initial identity");

        fs::remove_file(&path).expect("remove executable fixture");

        assert_eq!(monitor.check(), ExecutableCheck::Missing);
    }

    #[test]
    fn replacement_restored_with_new_identity_requests_restart() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("pix");
        let parked = directory.path().join("pix.parked");
        make_executable(&path, b"old image");
        let monitor = ExecutableIdentityMonitor::new(&path).expect("capture initial identity");

        fs::rename(&path, &parked).expect("park old executable");
        assert_eq!(monitor.check(), ExecutableCheck::Missing);
        replace_atomically(&path, b"new image");

        assert_eq!(monitor.check(), ExecutableCheck::Replaced);
    }

    #[test]
    fn new_process_identity_does_not_request_restart() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("pix");
        make_executable(&path, b"old image");
        let old_monitor = ExecutableIdentityMonitor::new(&path).expect("capture old identity");
        replace_atomically(&path, b"new image");

        assert_eq!(old_monitor.check(), ExecutableCheck::Replaced);
        let new_monitor = ExecutableIdentityMonitor::new(&path).expect("capture new identity");
        assert_eq!(new_monitor.check(), ExecutableCheck::Unchanged);
    }

    #[test]
    fn non_executable_replacement_is_waited_out() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("pix");
        make_executable(&path, b"old image");
        let monitor = ExecutableIdentityMonitor::new(&path).expect("capture initial identity");

        let replacement = path.with_extension("replacement");
        fs::write(&replacement, b"not ready").expect("write incomplete replacement");
        fs::rename(replacement, &path).expect("install incomplete replacement");
        assert_eq!(monitor.check(), ExecutableCheck::Missing);
        replace_atomically(&path, b"new image");

        assert_eq!(monitor.check(), ExecutableCheck::Replaced);
    }
}
