//! Bounded, read-only access to files below an authorized workspace root.
//!
//! This module deliberately owns the filesystem boundary instead of asking Pi
//! to inspect files. Requests arrive with a workspace-relative path, while the
//! caller is responsible for revalidating the authorized root before invoking
//! the service. On Unix, descriptor-relative `openat` traversal with
//! `O_NOFOLLOW` prevents a path component from being swapped for a symlink
//! between authorization and the read.

use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use pix_wire::{
    DEFAULT_WORKSPACE_DIRECTORY_ENTRIES, MAX_WORKSPACE_DIRECTORY_ENTRIES,
    MAX_WORKSPACE_DIRECTORY_RESPONSE_BYTES, MAX_WORKSPACE_FILE_READ_BYTES,
    MAX_WORKSPACE_PATH_BYTES, WorkspaceFileContentKind, WorkspaceFileEncoding, WorkspaceFileEntry,
    WorkspaceFileEntryKind, WorkspaceFileList, WorkspaceFileRead, WorkspaceFileStat,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[cfg(unix)]
use nix::dir::{Dir, Type as DirectoryEntryType};
#[cfg(unix)]
use nix::fcntl::{AtFlags, OFlag, open, openat};
#[cfg(unix)]
use nix::sys::stat::{Mode, SFlag, fstatat};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

const PREFIX_SAMPLE_BYTES: usize = 8 * 1024;
const MAX_DIRECTORY_SCAN_ENTRIES: usize = MAX_WORKSPACE_DIRECTORY_ENTRIES as usize + 1;
const SUPPRESSED_DIRECTORY_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "DerivedData",
    ".build",
    "target",
    ".next",
    "dist",
    "build",
    ".cache",
];

/// A filesystem failure whose public mapping never contains a local path.
#[derive(Debug, Error)]
pub enum WorkspaceFilesError {
    #[error("workspace-relative path is invalid")]
    InvalidPath,
    #[error("workspace entry was not found")]
    NotFound,
    #[error("workspace entry is a symbolic link")]
    Symlink,
    #[error("workspace entry is not a regular file or directory")]
    Unsupported,
    #[error("workspace file range is invalid")]
    InvalidRange,
    #[error("workspace file changed while it was being read")]
    RevisionMismatch,
    #[error("workspace entry permission was denied")]
    PermissionDenied,
    #[error("workspace filesystem operation failed")]
    Io(#[source] io::Error),
    #[error("workspace file service is unsupported on this platform")]
    UnsupportedPlatform,
}

/// Stateless filesystem capability. Authorization is intentionally supplied by
/// the caller so the service cannot accidentally discover unregistered roots.
pub struct WorkspaceFilesService;

impl WorkspaceFilesService {
    /// Lists one bounded directory without recursively walking descendants.
    /// The root and every nested component are opened relative to a directory
    /// descriptor, so a swapped symlink cannot redirect traversal.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceFilesError`] when the relative path is invalid, the
    /// root or directory cannot be opened, or the listing bound is invalid.
    #[allow(clippy::too_many_lines)]
    pub fn list(
        workspace_id: Uuid,
        root: &Path,
        path: &str,
        limit: Option<u32>,
    ) -> Result<WorkspaceFileList, WorkspaceFilesError> {
        let relative = RelativePath::parse(path)?;
        let limit = limit.unwrap_or(DEFAULT_WORKSPACE_DIRECTORY_ENTRIES);
        if limit == 0 || limit > MAX_WORKSPACE_DIRECTORY_ENTRIES {
            return Err(WorkspaceFilesError::InvalidRange);
        }

        #[cfg(not(unix))]
        {
            let _ = (workspace_id, root, relative, limit);
            return Err(WorkspaceFilesError::UnsupportedPlatform);
        }

        #[cfg(unix)]
        {
            let mut directory = open_directory(root, &relative.components)?;
            let (discovered, scan_truncated) = {
                let mut names: Vec<(String, WorkspaceFileEntryKind)> = Vec::new();
                let mut scan_truncated = false;
                for (scanned, item) in directory.iter().enumerate() {
                    if scanned >= MAX_DIRECTORY_SCAN_ENTRIES {
                        scan_truncated = true;
                        break;
                    }
                    let entry = item.map_err(map_nix_error)?;
                    let bytes = entry.file_name().to_bytes();
                    if bytes == b"." || bytes == b".." {
                        continue;
                    }
                    let Ok(name) = std::str::from_utf8(bytes) else {
                        // A wire path is UTF-8. Do not turn an unrepresentable
                        // local filename into a lossy path that could be acted
                        // on by a client.
                        continue;
                    };
                    if name.contains('\\') {
                        // Backslashes are intentionally not path separators on
                        // the wire. Such a local name cannot be addressed by a
                        // subsequent safe relative request, so omit it.
                        continue;
                    }
                    let kind = entry
                        .file_type()
                        .map_or(WorkspaceFileEntryKind::Other, entry_kind);
                    if is_suppressed(name, kind) {
                        continue;
                    }
                    names.push((name.to_owned(), kind));
                }
                names.sort_by(|left, right| {
                    let left_directory = left.1 == WorkspaceFileEntryKind::Directory;
                    let right_directory = right.1 == WorkspaceFileEntryKind::Directory;
                    right_directory
                        .cmp(&left_directory)
                        .then_with(|| left.0.cmp(&right.0))
                });
                (names, scan_truncated)
            };

            let total = discovered.len();
            let mut entries = Vec::with_capacity(total.min(limit as usize));
            for (name, discovered_kind) in discovered.into_iter().take(limit as usize) {
                let entry_path = join_relative(&relative.display, &name);
                if entry_path.len() > MAX_WORKSPACE_PATH_BYTES {
                    continue;
                }
                let (kind, metadata) = match discovered_kind {
                    WorkspaceFileEntryKind::Symlink => (WorkspaceFileEntryKind::Symlink, None),
                    WorkspaceFileEntryKind::Directory | WorkspaceFileEntryKind::File => {
                        match open_entry_metadata(&directory, &name, discovered_kind) {
                            Ok(value) => (value.0, Some(value.1)),
                            Err(WorkspaceFilesError::NotFound | WorkspaceFilesError::Symlink) => {
                                continue;
                            }
                            Err(_) => (discovered_kind, None),
                        }
                    }
                    WorkspaceFileEntryKind::Other => (WorkspaceFileEntryKind::Other, None),
                };
                let metadata_fields = metadata.as_ref().map(metadata_fields);
                entries.push(WorkspaceFileEntry {
                    name,
                    path: entry_path.clone(),
                    kind,
                    size: metadata_fields.as_ref().and_then(|fields| fields.size),
                    modified_at: metadata_fields
                        .as_ref()
                        .and_then(|fields| fields.modified_at.clone()),
                    language: (kind == WorkspaceFileEntryKind::File)
                        .then(|| language_for_path(&entry_path))
                        .flatten(),
                    // Listing does not sample file contents. Keep encoding
                    // unknown until `stat` or `read` can classify a bounded
                    // prefix without slowing down a directory tree fetch.
                    encoding: WorkspaceFileEncoding::Unknown,
                    revision: metadata_fields.and_then(|fields| fields.revision),
                });
            }

            let mut truncated = scan_truncated || total > limit as usize;
            while !entries.is_empty()
                && serialized_directory_size(&relative.display, &entries, truncated)
                    > MAX_WORKSPACE_DIRECTORY_RESPONSE_BYTES
            {
                entries.pop();
                truncated = true;
            }
            let revision = directory_revision(&relative.display, total, &entries);
            Ok(WorkspaceFileList {
                workspace_id,
                path: relative.display,
                entries,
                truncated,
                revision,
            })
        }
    }

    /// Returns metadata for a workspace-relative path. File encoding is
    /// classified from a small prefix; no whole-file read occurs.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceFilesError`] when the path is invalid, the entry is
    /// unavailable, or a symlink/special entry cannot be inspected safely.
    pub fn stat(
        workspace_id: Uuid,
        root: &Path,
        path: &str,
    ) -> Result<WorkspaceFileStat, WorkspaceFilesError> {
        let relative = RelativePath::parse(path)?;

        #[cfg(not(unix))]
        {
            let _ = (workspace_id, root, relative);
            return Err(WorkspaceFilesError::UnsupportedPlatform);
        }

        #[cfg(unix)]
        {
            let (file, kind) = open_target(root, &relative.components)?;
            let metadata = file.metadata().map_err(WorkspaceFilesError::Io)?;
            let (encoding, language) = if kind == WorkspaceFileEntryKind::File {
                let (content_kind, encoding) = classify_file(&file, metadata.len())?;
                let language = (content_kind == WorkspaceFileContentKind::Text)
                    .then(|| language_for_path(&relative.display))
                    .flatten();
                (encoding, language)
            } else {
                (WorkspaceFileEncoding::Unknown, None)
            };
            Ok(WorkspaceFileStat {
                workspace_id,
                path: relative.display,
                kind,
                size: Some(metadata.len()),
                modified_at: modified_at(&metadata),
                language,
                encoding,
                revision: Some(file_revision(&metadata)),
            })
        }
    }

    /// Reads one bounded byte range while holding the opened file descriptor.
    /// The file is checked before and after the read so a caller never receives
    /// a range assembled from two file revisions.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceFilesError`] when the range is invalid, the entry is
    /// unavailable or unsafe to open, or the file changes during the read.
    pub fn read(
        workspace_id: Uuid,
        root: &Path,
        path: &str,
        offset: u64,
        limit: u32,
        expected_revision: Option<&str>,
    ) -> Result<WorkspaceFileRead, WorkspaceFilesError> {
        let relative = RelativePath::parse(path)?;
        if limit == 0 || limit > MAX_WORKSPACE_FILE_READ_BYTES {
            return Err(WorkspaceFilesError::InvalidRange);
        }

        #[cfg(not(unix))]
        {
            let _ = (
                workspace_id,
                root,
                relative,
                offset,
                limit,
                expected_revision,
            );
            return Err(WorkspaceFilesError::UnsupportedPlatform);
        }

        #[cfg(unix)]
        {
            let (mut file, kind) = open_target(root, &relative.components)?;
            if kind != WorkspaceFileEntryKind::File {
                return Err(if kind == WorkspaceFileEntryKind::Symlink {
                    WorkspaceFilesError::Symlink
                } else {
                    WorkspaceFilesError::Unsupported
                });
            }
            let metadata = file.metadata().map_err(WorkspaceFilesError::Io)?;
            let total_size = metadata.len();
            let revision = file_revision(&metadata);
            if expected_revision.is_some_and(|expected| expected != revision) {
                return Err(WorkspaceFilesError::RevisionMismatch);
            }
            if offset > total_size {
                return Err(WorkspaceFilesError::InvalidRange);
            }

            let (kind, encoding) = classify_file(&file, total_size)?;
            let language = (kind == WorkspaceFileContentKind::Text)
                .then(|| language_for_path(&relative.display))
                .flatten();
            if kind != WorkspaceFileContentKind::Text {
                return Ok(WorkspaceFileRead {
                    workspace_id,
                    path: relative.display,
                    offset,
                    bytes_read: 0,
                    total_size,
                    eof: true,
                    kind,
                    data: String::new(),
                    encoding,
                    language,
                    revision,
                });
            }

            file.seek(SeekFrom::Start(offset))
                .map_err(WorkspaceFilesError::Io)?;
            let requested = u64::from(limit).min(total_size.saturating_sub(offset));
            let requested =
                usize::try_from(requested).map_err(|_| WorkspaceFilesError::InvalidRange)?;
            let mut bytes = vec![0_u8; requested];
            let bytes_read = file.read(&mut bytes).map_err(WorkspaceFilesError::Io)?;
            bytes.truncate(bytes_read);
            let after = file.metadata().map_err(WorkspaceFilesError::Io)?;
            let after_revision = file_revision(&after);
            if after_revision != revision {
                return Err(WorkspaceFilesError::RevisionMismatch);
            }
            let bytes_read = u32::try_from(bytes_read).unwrap_or(limit);
            Ok(WorkspaceFileRead {
                workspace_id,
                path: relative.display,
                offset,
                bytes_read,
                total_size,
                eof: offset.saturating_add(u64::from(bytes_read)) >= total_size,
                kind,
                data: STANDARD.encode(bytes),
                encoding,
                language,
                revision,
            })
        }
    }
}

#[derive(Debug)]
struct RelativePath {
    components: Vec<String>,
    display: String,
}

impl RelativePath {
    fn parse(value: &str) -> Result<Self, WorkspaceFilesError> {
        if value.is_empty() {
            return Ok(Self {
                components: Vec::new(),
                display: String::new(),
            });
        }
        if value.len() > MAX_WORKSPACE_PATH_BYTES
            || value.starts_with('/')
            || value.ends_with('/')
            || value.contains("//")
            || value.contains('\\')
            || value.as_bytes().contains(&0)
        {
            return Err(WorkspaceFilesError::InvalidPath);
        }
        let components = value
            .split('/')
            .map(|component| {
                if component.is_empty() || component == "." || component == ".." {
                    Err(WorkspaceFilesError::InvalidPath)
                } else {
                    Ok(component.to_owned())
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            components,
            display: value.to_owned(),
        })
    }
}

#[derive(Debug, Clone)]
struct MetadataFields {
    size: Option<u64>,
    modified_at: Option<String>,
    revision: Option<String>,
}

#[cfg(unix)]
fn open_directory(root: &Path, components: &[String]) -> Result<Dir, WorkspaceFilesError> {
    let mut directory = Dir::open(root, directory_flags(), Mode::empty()).map_err(map_nix_error)?;
    for component in components {
        directory = match Dir::openat(
            &directory,
            component.as_str(),
            directory_flags(),
            Mode::empty(),
        ) {
            Ok(directory) => directory,
            Err(nix::errno::Errno::ENOTDIR) => {
                // On macOS and some Linux filesystems, opening a symlink with
                // both O_DIRECTORY and O_NOFOLLOW reports ENOTDIR instead of
                // ELOOP. Inspect the directory entry itself to preserve the
                // fail-closed symlink error without following it.
                let metadata =
                    fstatat(&directory, component.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
                        .map_err(map_nix_error)?;
                if SFlag::from_bits_truncate(metadata.st_mode).contains(SFlag::S_IFLNK) {
                    return Err(WorkspaceFilesError::Symlink);
                }
                return Err(WorkspaceFilesError::NotFound);
            }
            Err(error) => return Err(map_nix_error(error)),
        };
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_target(
    root: &Path,
    components: &[String],
) -> Result<(File, WorkspaceFileEntryKind), WorkspaceFilesError> {
    if components.is_empty() {
        let fd = open(root, directory_flags(), Mode::empty()).map_err(map_nix_error)?;
        return Ok((File::from(fd), WorkspaceFileEntryKind::Directory));
    }
    let (name, parent_components) = components
        .split_last()
        .ok_or(WorkspaceFilesError::InvalidPath)?;
    let parent = open_directory(root, parent_components)?;
    let fd = openat(&parent, name.as_str(), file_flags(), Mode::empty()).map_err(map_nix_error)?;
    let file = File::from(fd);
    let metadata = file.metadata().map_err(WorkspaceFilesError::Io)?;
    let kind = if metadata.is_dir() {
        WorkspaceFileEntryKind::Directory
    } else if metadata.is_file() {
        WorkspaceFileEntryKind::File
    } else if metadata.file_type().is_symlink() {
        WorkspaceFileEntryKind::Symlink
    } else {
        WorkspaceFileEntryKind::Other
    };
    Ok((file, kind))
}

#[cfg(unix)]
fn open_entry_metadata(
    directory: &Dir,
    name: &str,
    discovered_kind: WorkspaceFileEntryKind,
) -> Result<(WorkspaceFileEntryKind, Metadata), WorkspaceFilesError> {
    let flags = match discovered_kind {
        WorkspaceFileEntryKind::Directory => directory_flags(),
        WorkspaceFileEntryKind::File => file_flags(),
        WorkspaceFileEntryKind::Symlink | WorkspaceFileEntryKind::Other => {
            return Err(WorkspaceFilesError::Unsupported);
        }
    };
    let fd = openat(directory, name, flags, Mode::empty()).map_err(map_nix_error)?;
    let file = File::from(fd);
    let metadata = file.metadata().map_err(WorkspaceFilesError::Io)?;
    let kind = if metadata.is_dir() {
        WorkspaceFileEntryKind::Directory
    } else if metadata.is_file() {
        WorkspaceFileEntryKind::File
    } else {
        WorkspaceFileEntryKind::Other
    };
    Ok((kind, metadata))
}

#[cfg(unix)]
fn directory_flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY
}

#[cfg(unix)]
fn file_flags() -> OFlag {
    // O_NONBLOCK is harmless for regular files and prevents a FIFO or other
    // special node from stalling the authenticated connection during the
    // metadata/type check. Special entries are reported as unsupported.
    OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK
}

#[cfg(unix)]
fn entry_kind(kind: DirectoryEntryType) -> WorkspaceFileEntryKind {
    match kind {
        DirectoryEntryType::Directory => WorkspaceFileEntryKind::Directory,
        DirectoryEntryType::File => WorkspaceFileEntryKind::File,
        DirectoryEntryType::Symlink => WorkspaceFileEntryKind::Symlink,
        DirectoryEntryType::Fifo
        | DirectoryEntryType::CharacterDevice
        | DirectoryEntryType::BlockDevice
        | DirectoryEntryType::Socket => WorkspaceFileEntryKind::Other,
    }
}

#[cfg(unix)]
fn map_nix_error(error: nix::errno::Errno) -> WorkspaceFilesError {
    match error {
        nix::errno::Errno::ENOENT | nix::errno::Errno::ENOTDIR => WorkspaceFilesError::NotFound,
        nix::errno::Errno::ELOOP => WorkspaceFilesError::Symlink,
        nix::errno::Errno::EACCES | nix::errno::Errno::EPERM => {
            WorkspaceFilesError::PermissionDenied
        }
        error => WorkspaceFilesError::Io(io::Error::from_raw_os_error(error as i32)),
    }
}

#[cfg(unix)]
fn metadata_fields(metadata: &Metadata) -> MetadataFields {
    MetadataFields {
        size: Some(metadata.len()),
        modified_at: modified_at(metadata),
        revision: Some(file_revision(metadata)),
    }
}

fn modified_at(metadata: &Metadata) -> Option<String> {
    metadata
        .modified()
        .ok()
        .map(|value| DateTime::<Utc>::from(value).to_rfc3339())
}

#[cfg(unix)]
fn file_revision(metadata: &Metadata) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"Pix workspace file revision v1\0");
    hasher.update(metadata.dev().to_le_bytes());
    hasher.update(metadata.ino().to_le_bytes());
    hasher.update(metadata.len().to_le_bytes());
    hasher.update(metadata.mtime().to_le_bytes());
    hasher.update(metadata.mtime_nsec().to_le_bytes());
    format!("sha256:{}", hex(hasher.finalize().as_slice()))
}

fn directory_revision(path: &str, total: usize, entries: &[WorkspaceFileEntry]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"Pix workspace directory revision v1\0");
    hasher.update(path.as_bytes());
    hasher.update((total as u64).to_le_bytes());
    for entry in entries {
        hasher.update(entry.name.as_bytes());
        hasher.update(entry.path.as_bytes());
        hasher.update(format!("{:?}", entry.kind).as_bytes());
        if let Some(size) = entry.size {
            hasher.update(size.to_le_bytes());
        }
        if let Some(revision) = &entry.revision {
            hasher.update(revision.as_bytes());
        }
    }
    format!("sha256:{}", hex(hasher.finalize().as_slice()))
}

fn serialized_directory_size(path: &str, entries: &[WorkspaceFileEntry], truncated: bool) -> usize {
    let entries_size = serde_json::to_vec(entries).map_or(usize::MAX, |bytes| bytes.len());
    entries_size
        .saturating_add(path.len())
        .saturating_add(256)
        .saturating_add(usize::from(truncated))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[usize::from(byte >> 4)] as char);
        result.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    result
}

fn classify_file(
    file: &File,
    total_size: u64,
) -> Result<(WorkspaceFileContentKind, WorkspaceFileEncoding), WorkspaceFilesError> {
    let mut probe = file.try_clone().map_err(WorkspaceFilesError::Io)?;
    let sample_size = usize::try_from(total_size.min(PREFIX_SAMPLE_BYTES as u64))
        .map_err(|_| WorkspaceFilesError::InvalidRange)?;
    let mut sample = vec![0_u8; sample_size];
    let bytes_read = probe.read(&mut sample).map_err(WorkspaceFilesError::Io)?;
    sample.truncate(bytes_read);
    if sample.contains(&0) || std::str::from_utf8(&sample).is_err() {
        Ok((
            WorkspaceFileContentKind::Binary,
            WorkspaceFileEncoding::Binary,
        ))
    } else {
        Ok((WorkspaceFileContentKind::Text, WorkspaceFileEncoding::Utf8))
    }
}

fn is_suppressed(name: &str, kind: WorkspaceFileEntryKind) -> bool {
    name == ".DS_Store"
        || (kind == WorkspaceFileEntryKind::Directory && SUPPRESSED_DIRECTORY_NAMES.contains(&name))
}

fn join_relative(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn language_for_path(path: &str) -> Option<String> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    let lower = file_name.to_ascii_lowercase();
    let language = match lower.as_str() {
        "dockerfile" => Some("dockerfile"),
        "makefile" | "gnumakefile" => Some("makefile"),
        ".gitignore" | ".dockerignore" => Some("gitignore"),
        ".env" | ".env.example" | ".env.local" => Some("dotenv"),
        ".prettierrc" | ".eslintrc" => Some("json"),
        _ => Path::new(file_name)
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .and_then(|extension| match extension.as_str() {
                "c" => Some("c"),
                "cc" | "cpp" | "cxx" | "h" | "hpp" => Some("cpp"),
                "cs" => Some("csharp"),
                "css" => Some("css"),
                "go" => Some("go"),
                "html" | "htm" => Some("html"),
                "java" => Some("java"),
                "js" | "mjs" | "cjs" | "jsx" => Some("javascript"),
                "json" | "jsonc" => Some("json"),
                "kt" | "kts" => Some("kotlin"),
                "md" | "markdown" | "mdown" => Some("markdown"),
                "php" => Some("php"),
                "py" | "pyw" => Some("python"),
                "rb" => Some("ruby"),
                "rs" => Some("rust"),
                "sh" | "bash" | "zsh" => Some("shell"),
                "sql" => Some("sql"),
                "swift" => Some("swift"),
                "toml" => Some("toml"),
                "ts" | "mts" | "cts" | "tsx" => Some("typescript"),
                "xml" => Some("xml"),
                "yaml" | "yml" => Some("yaml"),
                _ => None,
            }),
    }?;
    Some(language.to_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::{WorkspaceFilesError, WorkspaceFilesService};
    use pix_wire::{
        MAX_WORKSPACE_FILE_READ_BYTES, MAX_WORKSPACE_PATH_BYTES, WorkspaceFileContentKind,
        WorkspaceFileEncoding, WorkspaceFileEntryKind,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

    fn workspace() -> (tempfile::TempDir, Uuid) {
        let root = tempdir().expect("workspace");
        let id = Uuid::new_v4();
        (root, id)
    }

    #[test]
    fn lists_directories_lazily_with_suppression_and_stable_sorting() {
        let (root, id) = workspace();
        fs::create_dir(root.path().join("src")).expect("src");
        fs::write(root.path().join(".gitignore"), "target\n").expect("gitignore");
        fs::create_dir(root.path().join("target")).expect("target");
        fs::write(root.path().join("src/main.rs"), "fn main() {}\n").expect("source");

        let listed =
            WorkspaceFilesService::list(id, root.path(), "", None).expect("list workspace");
        assert!(!listed.truncated);
        assert_eq!(
            listed
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src", ".gitignore"]
        );
        assert_eq!(listed.entries[0].kind, WorkspaceFileEntryKind::Directory);
        assert_eq!(listed.entries[1].language.as_deref(), Some("gitignore"));

        assert!(!listed.entries.iter().any(|entry| entry.path == "target"));

        fs::write(root.path().join("notes.txt"), "notes").expect("notes");
        let bounded =
            WorkspaceFilesService::list(id, root.path(), "", Some(1)).expect("bounded list");
        assert_eq!(bounded.entries.len(), 1);
        assert!(bounded.truncated);
    }

    #[test]
    fn reads_utf8_ranges_and_rejects_revision_mismatch() {
        let (root, id) = workspace();
        let path = root.path().join("README.md");
        fs::write(&path, "hello workspace\n").expect("readme");
        let stat = WorkspaceFilesService::stat(id, root.path(), "README.md").expect("stat");
        assert_eq!(stat.kind, WorkspaceFileEntryKind::File);
        assert_eq!(stat.encoding, WorkspaceFileEncoding::Utf8);
        assert_eq!(stat.language.as_deref(), Some("markdown"));
        let revision = stat.revision.clone().expect("revision");
        let read = WorkspaceFilesService::read(
            id,
            root.path(),
            "README.md",
            0,
            MAX_WORKSPACE_FILE_READ_BYTES,
            Some(&revision),
        )
        .expect("read range");
        assert_eq!(read.kind, WorkspaceFileContentKind::Text);
        assert!(read.eof);
        assert_eq!(read.bytes_read, 16);

        fs::write(&path, "changed workspace\n").expect("rewrite");
        assert!(matches!(
            WorkspaceFilesService::read(id, root.path(), "README.md", 0, 5, Some(&revision)),
            Err(WorkspaceFilesError::RevisionMismatch)
        ));
    }

    #[test]
    fn binary_and_special_entries_are_not_read_as_text() {
        let (root, id) = workspace();
        fs::write(root.path().join("image.bin"), [0, 1, 2, 3]).expect("binary");
        nix::unistd::mkfifo(
            &root.path().join("pipe"),
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .expect("fifo");
        symlink(
            root.path().join("image.bin"),
            root.path().join("image-link"),
        )
        .expect("symlink");

        let binary = WorkspaceFilesService::read(id, root.path(), "image.bin", 0, 32, None)
            .expect("binary read");
        assert_eq!(binary.kind, WorkspaceFileContentKind::Binary);
        assert_eq!(binary.data, "");
        assert_eq!(binary.encoding, WorkspaceFileEncoding::Binary);

        let special = WorkspaceFilesService::stat(id, root.path(), "pipe").expect("fifo stat");
        assert_eq!(special.kind, WorkspaceFileEntryKind::Other);
        assert!(matches!(
            WorkspaceFilesService::read(id, root.path(), "pipe", 0, 32, None),
            Err(WorkspaceFilesError::Unsupported)
        ));

        let listed = WorkspaceFilesService::list(id, root.path(), "", None).expect("list symlink");
        assert_eq!(
            listed
                .entries
                .iter()
                .find(|entry| entry.name == "image-link")
                .map(|entry| entry.kind),
            Some(WorkspaceFileEntryKind::Symlink)
        );
        assert!(matches!(
            WorkspaceFilesService::read(id, root.path(), "image-link", 0, 32, None),
            Err(WorkspaceFilesError::Symlink)
        ));
    }

    #[test]
    fn rejects_traversal_and_intermediate_symlink_escape() {
        let (root, id) = workspace();
        let outside = tempdir().expect("outside");
        fs::write(outside.path().join("secret.txt"), "secret").expect("secret");
        symlink(outside.path(), root.path().join("linked")).expect("linked directory");
        for path in ["../secret.txt", "/tmp/secret.txt", "linked/secret.txt"] {
            assert!(matches!(
                WorkspaceFilesService::stat(id, root.path(), path),
                Err(WorkspaceFilesError::InvalidPath | WorkspaceFilesError::Symlink)
            ));
        }
    }

    #[test]
    fn enforces_directory_limit_and_range_limit() {
        let (root, id) = workspace();
        fs::write(root.path().join("file.txt"), "file").expect("file");
        let large = vec![b'x'; usize::try_from(MAX_WORKSPACE_FILE_READ_BYTES).expect("bound") + 1];
        fs::write(root.path().join("large.txt"), large).expect("large file");
        assert!(matches!(
            WorkspaceFilesService::list(id, root.path(), "", Some(0)),
            Err(WorkspaceFilesError::InvalidRange)
        ));
        assert!(matches!(
            WorkspaceFilesService::stat(id, root.path(), &"x".repeat(MAX_WORKSPACE_PATH_BYTES + 1)),
            Err(WorkspaceFilesError::InvalidPath)
        ));
        assert!(matches!(
            WorkspaceFilesService::read(
                id,
                root.path(),
                "file.txt",
                0,
                MAX_WORKSPACE_FILE_READ_BYTES + 1,
                None
            ),
            Err(WorkspaceFilesError::InvalidRange)
        ));

        let first = WorkspaceFilesService::read(
            id,
            root.path(),
            "large.txt",
            0,
            MAX_WORKSPACE_FILE_READ_BYTES,
            None,
        )
        .expect("first bounded range");
        assert_eq!(first.bytes_read, MAX_WORKSPACE_FILE_READ_BYTES);
        assert!(!first.eof);
        let second = WorkspaceFilesService::read(
            id,
            root.path(),
            "large.txt",
            u64::from(MAX_WORKSPACE_FILE_READ_BYTES),
            1,
            Some(&first.revision),
        )
        .expect("second bounded range");
        assert_eq!(second.bytes_read, 1);
        assert!(second.eof);
    }
}
