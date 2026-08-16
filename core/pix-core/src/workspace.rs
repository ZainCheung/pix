use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use crate::config::{HostConfig, WorkspaceRecord};

pub struct WorkspaceRegistry<'a> {
    config: &'a mut HostConfig,
}

impl<'a> WorkspaceRegistry<'a> {
    pub fn new(config: &'a mut HostConfig) -> Self {
        Self { config }
    }

    /// Authorizes an existing directory after resolving its canonical path.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] if the path is unavailable, is not a
    /// directory, is already authorized, or has an empty display name.
    pub fn add(
        &mut self,
        path: impl AsRef<Path>,
        name: Option<String>,
    ) -> Result<&WorkspaceRecord, WorkspaceError> {
        let canonical = canonical_directory(path.as_ref())?;
        if self
            .config
            .workspaces
            .iter()
            .any(|workspace| workspace.path == canonical)
        {
            return Err(WorkspaceError::AlreadyAuthorized(canonical));
        }

        let display_name = name.unwrap_or_else(|| default_display_name(&canonical));
        if display_name.trim().is_empty() {
            return Err(WorkspaceError::EmptyName);
        }

        let index = self.config.workspaces.len();
        self.config.workspaces.push(WorkspaceRecord {
            id: Uuid::new_v4(),
            name: display_name,
            path: canonical,
            created_at: Utc::now(),
        });
        Ok(&self.config.workspaces[index])
    }

    /// Revalidates and returns an authorized workspace root.
    ///
    /// The equality check detects a registered directory that was later
    /// replaced by a symlink to a different location.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the workspace is unknown, unavailable,
    /// or no longer resolves to the originally authorized canonical path.
    pub fn authorized_root(&self, workspace_id: Uuid) -> Result<PathBuf, WorkspaceError> {
        let workspace = self
            .config
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or(WorkspaceError::UnknownWorkspace(workspace_id))?;
        let canonical = canonical_directory(&workspace.path)?;
        if canonical != workspace.path {
            return Err(WorkspaceError::AuthorizedRootChanged {
                configured: workspace.path.clone(),
                resolved: canonical,
            });
        }
        Ok(workspace.path.clone())
    }

    /// Resolves an existing path and proves it remains within an authorized root.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the workspace is unknown, the requested
    /// path cannot be canonicalized, or canonicalization reveals a traversal
    /// or symbolic-link escape.
    pub fn resolve_existing_path(
        &self,
        workspace_id: Uuid,
        requested_path: impl AsRef<Path>,
    ) -> Result<PathBuf, WorkspaceError> {
        let workspace = self
            .config
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or(WorkspaceError::UnknownWorkspace(workspace_id))?;
        let canonical = fs::canonicalize(requested_path.as_ref()).map_err(|source| {
            WorkspaceError::Canonicalize {
                path: requested_path.as_ref().to_path_buf(),
                source,
            }
        })?;
        if !canonical.starts_with(&workspace.path) {
            return Err(WorkspaceError::OutsideAuthorizedRoot {
                requested: canonical,
                root: workspace.path.clone(),
            });
        }
        Ok(canonical)
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, WorkspaceError> {
    let canonical = fs::canonicalize(path).map_err(|source| WorkspaceError::Canonicalize {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::metadata(&canonical).map_err(|source| WorkspaceError::Canonicalize {
        path: canonical.clone(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(WorkspaceError::NotDirectory(canonical));
    }
    Ok(canonical)
}

fn default_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), ToOwned::to_owned)
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("could not resolve workspace path {path}: {source}")]
    Canonicalize { path: PathBuf, source: io::Error },
    #[error("workspace path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("workspace is already authorized: {0}")]
    AlreadyAuthorized(PathBuf),
    #[error("workspace display name cannot be empty")]
    EmptyName,
    #[error("unknown workspace: {0}")]
    UnknownWorkspace(Uuid),
    #[error("requested path {requested} is outside authorized root {root}")]
    OutsideAuthorizedRoot { requested: PathBuf, root: PathBuf },
    #[error("authorized workspace {configured} now resolves to {resolved}")]
    AuthorizedRootChanged {
        configured: PathBuf,
        resolved: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{WorkspaceError, WorkspaceRegistry};
    use crate::config::HostConfig;

    #[test]
    fn authorizes_a_canonical_directory_and_allows_children() {
        let directory = tempdir().expect("temporary directory");
        let root = directory.path().join("workspace");
        let child = root.join("src/lib.rs");
        fs::create_dir_all(child.parent().expect("child parent")).expect("create directories");
        fs::write(&child, "test").expect("write child");
        let mut config = HostConfig::new("Test Mac");
        let mut registry = WorkspaceRegistry::new(&mut config);
        let id = registry.add(&root, None).expect("add workspace").id;

        assert_eq!(
            registry
                .resolve_existing_path(id, &child)
                .expect("resolve child"),
            fs::canonicalize(child).expect("canonical child")
        );
    }

    #[test]
    fn rejects_parent_traversal() {
        let directory = tempdir().expect("temporary directory");
        let root = directory.path().join("workspace");
        let outside = directory.path().join("secret.txt");
        fs::create_dir(&root).expect("create workspace");
        fs::write(&outside, "secret").expect("write outside file");
        let mut config = HostConfig::new("Test Mac");
        let mut registry = WorkspaceRegistry::new(&mut config);
        let id = registry.add(&root, None).expect("add workspace").id;

        assert!(matches!(
            registry.resolve_existing_path(id, root.join("../secret.txt")),
            Err(WorkspaceError::OutsideAuthorizedRoot { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let root = directory.path().join("workspace");
        let outside = directory.path().join("secret.txt");
        fs::create_dir(&root).expect("create workspace");
        fs::write(&outside, "secret").expect("write outside file");
        symlink(&outside, root.join("link")).expect("create symlink");
        let mut config = HostConfig::new("Test Mac");
        let mut registry = WorkspaceRegistry::new(&mut config);
        let id = registry.add(&root, None).expect("add workspace").id;

        assert!(matches!(
            registry.resolve_existing_path(id, root.join("link")),
            Err(WorkspaceError::OutsideAuthorizedRoot { .. })
        ));
    }

    #[test]
    fn missing_directory_is_unavailable() {
        let directory = tempdir().expect("temporary directory");
        let root = directory.path().join("workspace");
        fs::create_dir(&root).expect("create workspace");
        let mut config = HostConfig::new("Test Mac");
        let mut registry = WorkspaceRegistry::new(&mut config);
        let id = registry.add(&root, None).expect("add workspace").id;
        fs::remove_dir(&root).expect("remove workspace");

        assert!(matches!(
            registry.authorized_root(id),
            Err(WorkspaceError::Canonicalize { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_an_authorized_root_replaced_by_a_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let root = directory.path().join("workspace");
        let outside = directory.path().join("replacement");
        fs::create_dir(&root).expect("create workspace");
        fs::create_dir(&outside).expect("create replacement");
        let mut config = HostConfig::new("Test Mac");
        let mut registry = WorkspaceRegistry::new(&mut config);
        let id = registry.add(&root, None).expect("add workspace").id;
        fs::remove_dir(&root).expect("remove original workspace");
        symlink(&outside, &root).expect("replace root with symlink");

        assert!(matches!(
            registry.authorized_root(id),
            Err(WorkspaceError::AuthorizedRootChanged { .. })
        ));
    }
}
