//! Installation and status commands for Pix's optional Pi TUI extension.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pix_core::{ConfigStore, HostEnvironment, PiInstallation, PiProbe};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::Builder as TempFileBuilder;
use uuid::Uuid;

use crate::PiBridgeCommand;
use crate::commands::shared::home_directory;
use crate::output::CommandOutput;
use crate::status::{HostServiceStatus, request_control_rpc};

const BRIDGE_EXTENSION_VERSION: u32 = 2;
const BRIDGE_MANIFEST_SCHEMA_VERSION: u32 = 1;
const BRIDGE_DIRECTORY: &str = "pix-bridge";
const BRIDGE_FILENAME: &str = "index.ts";
const BRIDGE_MANIFEST_FILENAME: &str = ".pix-managed.json";
const BRIDGE_SOURCE: &str = include_str!("../../resources/pix-bridge/index.ts");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExtensionState {
    Installed,
    Missing,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExtensionInspection {
    state: ExtensionState,
    version: Option<u32>,
    sha256: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize)]
struct ExtensionStatus {
    state: ExtensionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct PiStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    executable: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<semver::Version>,
    #[serde(skip_serializing_if = "Option::is_none")]
    supported: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct HostStatus {
    listener: &'static str,
    bridge_socket: PathBuf,
    bridge_socket_state: &'static str,
    active_tui: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct BridgeStatus {
    extension: ExtensionStatus,
    pi: PiStatus,
    host: HostStatus,
}

#[derive(Debug, Clone, Serialize)]
struct InstallResult {
    state: ExtensionState,
    version: u32,
    path: PathBuf,
    changed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct UninstallResult {
    state: ExtensionState,
    path: PathBuf,
    removed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BridgeManifest {
    schema_version: u32,
    extension_version: u32,
    file: String,
    sha256: String,
}

#[derive(Debug, Clone)]
struct BridgePaths {
    extension_dir: PathBuf,
    extension_file: PathBuf,
    manifest_file: PathBuf,
}

pub(crate) fn bridge_command(
    store: &ConfigStore,
    command: &PiBridgeCommand,
    output: CommandOutput,
) -> Result<()> {
    match command {
        PiBridgeCommand::Install => install(store, output),
        PiBridgeCommand::Uninstall => uninstall(output),
        PiBridgeCommand::Status => status(store, output),
    }
}

fn install(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let installation = inspect_pi(store)?;
    if !installation.supported {
        bail!(
            "Pi {} is outside the currently verified range {}",
            installation.version,
            pix_core::pi::SUPPORTED_PI_VERSION
        );
    }
    let home = home_directory().context("locating the home directory")?;
    let result = install_extension(&home)?;
    if output.is_json() {
        return output.success("pi.bridge.install", &result);
    }
    if result.changed {
        println!("Pix TUI Bridge installed.");
    } else {
        println!("Pix TUI Bridge is already installed.");
    }
    println!();
    println!("Continue using Pi normally:");
    println!();
    println!("    pi");
    Ok(())
}

fn uninstall(output: CommandOutput) -> Result<()> {
    let home = home_directory().context("locating the home directory")?;
    let result = uninstall_extension(&home)?;
    if output.is_json() {
        return output.success("pi.bridge.uninstall", &result);
    }
    if result.removed {
        println!("Pix TUI Bridge uninstalled.");
        println!(
            "A running Pi process may keep the extension loaded until it exits or /reload is used."
        );
    } else {
        println!("Pix TUI Bridge is not installed.");
    }
    Ok(())
}

fn status(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let home = home_directory().context("locating the home directory")?;
    let paths = bridge_paths(&home);
    let inspection = inspect_extension(&home)?;
    let installation = inspect_pi(store).ok();
    let host = host_status(store);
    let view = BridgeStatus {
        extension: ExtensionStatus {
            state: inspection.state,
            version: inspection.version,
            path: paths.extension_file,
        },
        pi: PiStatus {
            executable: installation.as_ref().map(|value| value.executable.clone()),
            version: installation.as_ref().map(|value| value.version.clone()),
            supported: installation.as_ref().map(|value| value.supported),
        },
        host,
    };
    if output.is_json() {
        return output.success("pi.bridge.status", &view);
    }
    println!("Pix TUI Bridge");
    match (view.extension.state, view.extension.version) {
        (ExtensionState::Installed, Some(version)) => {
            println!("  extension: installed (v{version});");
        }
        (ExtensionState::Modified, _) => {
            println!("  extension: modified (manual changes detected)");
        }
        _ => println!("  extension: missing"),
    }
    match (&view.pi.executable, &view.pi.version, view.pi.supported) {
        (Some(executable), Some(version), Some(true)) => {
            println!("  pi: {} ({version}, supported)", executable.display());
        }
        (Some(executable), Some(version), Some(false)) => {
            println!("  pi: {} ({version}, unsupported)", executable.display());
        }
        _ => println!("  pi: unavailable"),
    }
    println!("  host listener: {}", view.host.listener);
    println!(
        "  bridge socket: {} ({})",
        view.host.bridge_socket.display(),
        view.host.bridge_socket_state
    );
    match view.host.active_tui {
        Some(count) => println!("  active TUI: {count}"),
        None => println!("  active TUI: unavailable"),
    }
    Ok(())
}

fn inspect_pi(store: &ConfigStore) -> Result<PiInstallation> {
    let configured = match store.load() {
        Ok(config) => config.preferences.pi_executable,
        Err(pix_core::config::ConfigError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => return Err(error.into()),
    };
    PiProbe::new(configured)
        .with_environment(HostEnvironment::resolve_for("pi"))
        .inspect()
        .context("probing the installed Pi executable")
}

fn install_extension(home: &Path) -> Result<InstallResult> {
    ensure_extension_tree(home, true)?;
    let paths = bridge_paths(home);
    let inspection = inspect_extension(home)?;
    if inspection.state == ExtensionState::Modified {
        bail!(
            "refusing to overwrite a modified or unmanaged Pix TUI Bridge at {}",
            paths.extension_dir.display()
        );
    }
    if inspection
        .version
        .is_some_and(|version| version > BRIDGE_EXTENSION_VERSION)
    {
        bail!(
            "installed Pix TUI Bridge version is newer than this Pix binary; refusing to overwrite {}",
            paths.extension_dir.display()
        );
    }
    let source_hash = digest_bytes(BRIDGE_SOURCE.as_bytes());
    let changed = inspection.state != ExtensionState::Installed
        || inspection.version != Some(BRIDGE_EXTENSION_VERSION)
        || inspection.sha256 != Some(source_hash);
    if changed {
        atomic_write(&paths.extension_file, BRIDGE_SOURCE.as_bytes())?;
        let manifest = BridgeManifest {
            schema_version: BRIDGE_MANIFEST_SCHEMA_VERSION,
            extension_version: BRIDGE_EXTENSION_VERSION,
            file: BRIDGE_FILENAME.to_owned(),
            sha256: digest_hex(source_hash),
        };
        let encoded = serde_json::to_vec_pretty(&manifest).context("encoding bridge manifest")?;
        atomic_write(&paths.manifest_file, &encoded)?;
    }
    Ok(InstallResult {
        state: ExtensionState::Installed,
        version: BRIDGE_EXTENSION_VERSION,
        path: paths.extension_file,
        changed,
    })
}

fn uninstall_extension(home: &Path) -> Result<UninstallResult> {
    let paths = bridge_paths(home);
    if !ensure_extension_tree(home, false)? {
        return Ok(UninstallResult {
            state: ExtensionState::Missing,
            path: paths.extension_file,
            removed: false,
        });
    }
    let inspection = inspect_extension(home)?;
    if inspection.state == ExtensionState::Modified {
        bail!(
            "refusing to remove a modified or unmanaged Pix TUI Bridge at {}",
            paths.extension_dir.display()
        );
    }
    if inspection
        .version
        .is_some_and(|version| version > BRIDGE_EXTENSION_VERSION)
    {
        bail!(
            "installed Pix TUI Bridge version is newer than this Pix binary; refusing to remove {}",
            paths.extension_dir.display()
        );
    }
    if inspection.state == ExtensionState::Installed {
        fs::remove_file(&paths.extension_file).with_context(|| {
            format!(
                "removing managed bridge extension {}",
                paths.extension_file.display()
            )
        })?;
        fs::remove_file(&paths.manifest_file).with_context(|| {
            format!("removing bridge manifest {}", paths.manifest_file.display())
        })?;
        let _ = fs::remove_dir(&paths.extension_dir);
        return Ok(UninstallResult {
            state: ExtensionState::Missing,
            path: paths.extension_file,
            removed: true,
        });
    }
    Ok(UninstallResult {
        state: ExtensionState::Missing,
        path: paths.extension_file,
        removed: false,
    })
}

fn inspect_extension(home: &Path) -> Result<ExtensionInspection> {
    let paths = bridge_paths(home);
    if !ensure_extension_tree(home, false)? {
        return Ok(ExtensionInspection {
            state: ExtensionState::Missing,
            version: None,
            sha256: None,
        });
    }
    let index_metadata = match fs::symlink_metadata(&paths.extension_file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ExtensionInspection {
                state: ExtensionState::Missing,
                version: None,
                sha256: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let manifest_metadata = match fs::symlink_metadata(&paths.manifest_file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ExtensionInspection {
                state: ExtensionState::Modified,
                version: None,
                sha256: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !index_metadata.is_file()
        || index_metadata.file_type().is_symlink()
        || !manifest_metadata.is_file()
        || manifest_metadata.file_type().is_symlink()
    {
        return Ok(ExtensionInspection {
            state: ExtensionState::Modified,
            version: None,
            sha256: None,
        });
    }
    let Some(manifest) = fs::read_to_string(&paths.manifest_file)
        .ok()
        .and_then(|value| serde_json::from_str::<BridgeManifest>(&value).ok())
    else {
        return Ok(ExtensionInspection {
            state: ExtensionState::Modified,
            version: None,
            sha256: None,
        });
    };
    if manifest.schema_version != BRIDGE_MANIFEST_SCHEMA_VERSION
        || manifest.file != BRIDGE_FILENAME
        || manifest.extension_version == 0
    {
        return Ok(ExtensionInspection {
            state: ExtensionState::Modified,
            version: Some(manifest.extension_version),
            sha256: None,
        });
    }
    if manifest.extension_version == BRIDGE_EXTENSION_VERSION
        && manifest.sha256 != digest_hex(digest_bytes(BRIDGE_SOURCE.as_bytes()))
    {
        return Ok(ExtensionInspection {
            state: ExtensionState::Modified,
            version: Some(manifest.extension_version),
            sha256: None,
        });
    }
    let bytes = fs::read(&paths.extension_file)?;
    let digest = digest_bytes(&bytes);
    if digest_hex(digest) != manifest.sha256 {
        return Ok(ExtensionInspection {
            state: ExtensionState::Modified,
            version: Some(manifest.extension_version),
            sha256: Some(digest),
        });
    }
    Ok(ExtensionInspection {
        state: ExtensionState::Installed,
        version: Some(manifest.extension_version),
        sha256: Some(digest),
    })
}

fn bridge_paths(home: &Path) -> BridgePaths {
    let extension_dir = home
        .join(".pi")
        .join("agent")
        .join("extensions")
        .join(BRIDGE_DIRECTORY);
    BridgePaths {
        extension_file: extension_dir.join(BRIDGE_FILENAME),
        manifest_file: extension_dir.join(BRIDGE_MANIFEST_FILENAME),
        extension_dir,
    }
}

fn ensure_extension_tree(home: &Path, create: bool) -> Result<bool> {
    let metadata = fs::symlink_metadata(home)
        .with_context(|| format!("inspecting home directory {}", home.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "refusing to use an unsafe home directory {}",
            home.display()
        );
    }
    let mut current = home.to_path_buf();
    for component in [".pi", "agent", "extensions", BRIDGE_DIRECTORY] {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "refusing unsafe Pix extension directory {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&current).with_context(|| {
                    format!("creating Pix extension directory {}", current.display())
                })?;
                set_private_mode(&current, 0o700)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("locating parent for {}", path.display()))?;
    let mut temporary = TempFileBuilder::new()
        .prefix(".pix-bridge-")
        .tempfile_in(parent)
        .with_context(|| format!("creating temporary bridge file in {}", parent.display()))?;
    set_private_mode(temporary.path(), 0o600)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| {
        anyhow::anyhow!("persisting bridge file {}: {}", path.display(), error.error)
    })?;
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut result = [0_u8; 32];
    result.copy_from_slice(&digest);
    result
}

fn digest_hex(digest: [u8; 32]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing a String cannot fail");
    }
    encoded
}

fn set_private_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn host_status(store: &ConfigStore) -> HostStatus {
    let bridge_socket = HostServiceStatus::tui_bridge_socket_path_for(store.path());
    let listener_running = HostServiceStatus::current(store.path()).is_some();
    HostStatus {
        listener: if listener_running {
            "running"
        } else {
            "unavailable"
        },
        bridge_socket_state: bridge_socket_state(&bridge_socket),
        active_tui: listener_running.then(|| active_tui_count(store)).flatten(),
        bridge_socket,
    }
}

fn bridge_socket_state(path: &Path) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt as _;
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => "available",
            Ok(_) => "invalid",
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
            Err(_) => "unavailable",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        "unsupported"
    }
}

fn active_tui_count(store: &ConfigStore) -> Option<usize> {
    #[cfg(unix)]
    {
        let request = serde_json::json!({
            "schema_version": 1,
            "request_id": Uuid::new_v4(),
            "command": "session.list",
            "args": {},
        });
        let response = request_control_rpc(store.path(), &request, Duration::from_secs(2)).ok()?;
        if response.get("ok") != Some(&serde_json::Value::Bool(true)) {
            return None;
        }
        response
            .pointer("/data/sessions")
            .and_then(serde_json::Value::as_array)
            .map(|sessions| {
                sessions
                    .iter()
                    .filter(|session| session.get("backend") == Some(&serde_json::json!("tui")))
                    .count()
            })
    }
    #[cfg(not(unix))]
    {
        let _ = store;
        None
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        BRIDGE_EXTENSION_VERSION, BRIDGE_SOURCE, ExtensionState, digest_bytes, digest_hex,
        inspect_extension, install_extension, uninstall_extension,
    };
    use tempfile::tempdir;

    #[test]
    fn install_is_repeatable_and_records_integrity() {
        let home = tempdir().expect("home");
        let first = install_extension(home.path()).expect("install bridge");
        assert!(first.changed);
        assert_eq!(first.state, ExtensionState::Installed);
        let inspection = inspect_extension(home.path()).expect("inspect bridge");
        assert_eq!(inspection.state, ExtensionState::Installed);
        assert_eq!(
            inspection.sha256,
            Some(digest_bytes(BRIDGE_SOURCE.as_bytes()))
        );

        let second = install_extension(home.path()).expect("repeat install");
        assert!(!second.changed);
        assert_eq!(second.state, ExtensionState::Installed);
    }

    #[test]
    fn install_upgrades_a_previous_pix_managed_extension() {
        let home = tempdir().expect("home");
        let first = install_extension(home.path()).expect("install bridge");
        let old_source = b"// Pix-managed extension from an older release\n";
        fs::write(&first.path, old_source).expect("write old extension");
        let old_manifest = first
            .path
            .parent()
            .expect("extension dir")
            .join(".pix-managed.json");
        let old_hash = digest_hex(digest_bytes(old_source));
        fs::write(
            old_manifest,
            format!(
                "{{\"schema_version\":1,\"extension_version\":1,\"file\":\"index.ts\",\"sha256\":\"{old_hash}\"}}"
            ),
        )
        .expect("write old manifest");

        let upgraded = install_extension(home.path()).expect("upgrade bridge");
        assert!(upgraded.changed);
        assert_eq!(upgraded.version, BRIDGE_EXTENSION_VERSION);
        assert_eq!(
            fs::read(&upgraded.path).expect("read upgraded extension"),
            BRIDGE_SOURCE.as_bytes()
        );
    }

    #[test]
    fn modified_extension_refuses_uninstall() {
        let home = tempdir().expect("home");
        let result = install_extension(home.path()).expect("install bridge");
        fs::write(&result.path, b"// user modification\n").expect("modify bridge");
        let error = uninstall_extension(home.path()).expect_err("modified bridge must be refused");
        assert!(error.to_string().contains("modified or unmanaged"));
    }

    #[test]
    fn modified_manifest_refuses_uninstall() {
        let home = tempdir().expect("home");
        let result = install_extension(home.path()).expect("install bridge");
        let manifest = result
            .path
            .parent()
            .expect("extension dir")
            .join(".pix-managed.json");
        fs::write(
            manifest,
            br#"{"schema_version":1,"extension_version":1,"file":"index.ts","sha256":"00"}"#,
        )
        .expect("modify manifest");
        let error =
            uninstall_extension(home.path()).expect_err("modified manifest must be refused");
        assert!(error.to_string().contains("modified or unmanaged"));
    }

    #[test]
    fn uninstall_keeps_unrelated_files() {
        let home = tempdir().expect("home");
        let result = install_extension(home.path()).expect("install bridge");
        let extra = result
            .path
            .parent()
            .expect("extension dir")
            .join("notes.txt");
        fs::write(&extra, b"keep me").expect("write unrelated file");
        let removed = uninstall_extension(home.path()).expect("uninstall bridge");
        assert!(removed.removed);
        assert!(extra.is_file());
        assert_eq!(
            inspect_extension(home.path())
                .expect("inspect missing")
                .state,
            ExtensionState::Missing
        );
    }

    #[test]
    fn digest_hex_is_stable() {
        assert_eq!(
            digest_hex(digest_bytes(b"pix")),
            "57c712d37789c12225e9fa5c5af81338cfb2a7787cf84047d52d2b40fb73afb0"
        );
    }
}
