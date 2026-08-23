//! Self-update from the repository's GitHub releases.
//!
//! The update path mirrors `website/public/install.sh`: resolve the latest
//! release, download the platform archive, and replace the running
//! executable (plus the macOS app bundle) in place. `curl` does the
//! transport so no new dependency enters the CLI.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use pix_core::ConfigStore;

use crate::output::CommandOutput;

const REPOSITORY: &str = "ZainCheung/pix";
const RELEASE_API: &str = "https://api.github.com/repos/ZainCheung/pix/releases/latest";

struct Release {
    tag: String,
    asset_url: String,
    asset_name: String,
}

pub(crate) fn update(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let release = latest_release()?;
    let latest = release
        .tag
        .strip_prefix('v')
        .unwrap_or(&release.tag)
        .to_owned();
    if latest == current {
        if output.is_json() {
            return output.success(
                "update",
                &serde_json::json!({
                    "current": current,
                    "latest": latest,
                    "updated": false,
                }),
            );
        }
        println!("Pix {current} is already the latest release.");
        return Ok(());
    }

    let executable = install_target()?;
    download_and_install(&release, &executable, output.is_json())?;

    let service_note = crate::commands::shared::host_service_control_live(store).unwrap_or(false);
    if output.is_json() {
        return output.success(
            "update",
            &serde_json::json!({
                "current": current,
                "latest": latest,
                "updated": true,
                "executable": executable.display().to_string(),
                "service_restart_required": service_note,
            }),
        );
    }
    println!(
        "Updated Pix {current} → {latest} at {}",
        executable.display()
    );
    if service_note {
        println!("Run `pix service restart` to move the running host onto the new build.");
    }
    Ok(())
}

fn latest_release() -> Result<Release> {
    let body = run_curl(
        &[
            "-fsSL",
            "--retry",
            "2",
            "--connect-timeout",
            "8",
            RELEASE_API,
        ],
        "querying the latest Pix release",
    )?;
    let payload: serde_json::Value =
        serde_json::from_str(&body).context("decoding the Pix release manifest")?;
    let tag = payload
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .context("the latest Pix release has no tag")?
        .to_owned();
    let suffix = asset_suffix()?;
    let asset = payload
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .context("the latest Pix release lists no assets")?
        .iter()
        .find(|asset| {
            asset
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name.ends_with(&suffix))
        })
        .context("no release asset matches this platform")?;
    let asset_name = asset
        .get("name")
        .and_then(serde_json::Value::as_str)
        .context("release asset has no name")?
        .to_owned();
    let asset_url = asset
        .get("browser_download_url")
        .and_then(serde_json::Value::as_str)
        .context("release asset has no download URL")?
        .to_owned();
    Ok(Release {
        tag,
        asset_url,
        asset_name,
    })
}

/// Resolves the latest release tag with tight timeouts; any failure means
/// "no hint". Used by the home screen's silent update check.
pub(crate) fn latest_version() -> Option<String> {
    let body = run_curl(
        &[
            "-fsSL",
            "--connect-timeout",
            "2",
            "--max-time",
            "3",
            RELEASE_API,
        ],
        "checking the latest Pix release",
    )
    .ok()?;
    let payload: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = payload
        .get("tag_name")
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    Some(tag.strip_prefix('v').unwrap_or(&tag).to_owned())
}

fn asset_suffix() -> Result<String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("macos-arm64.zip".to_owned()),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu.tar.gz".to_owned()),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu.tar.gz".to_owned()),
        (os, arch) => bail!(
            "Pix does not publish releases for {os}-{arch}; see https://github.com/{REPOSITORY}/releases"
        ),
    }
}

/// The update replaces the executable that is actually running so dev
/// checkouts and custom install locations keep working. Cargo target
/// directories are refused: a `cargo build` overwrites them anyway.
fn install_target() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("locating the running pix executable")?;
    let text = executable.display().to_string();
    if text.contains("/target/") {
        bail!(
            "this pix runs from a Cargo build directory; install a release build first (curl -fsSL https://pix.deepoke.com/install.sh | sh)"
        );
    }
    Ok(executable)
}

fn download_and_install(release: &Release, executable: &Path, quiet: bool) -> Result<()> {
    let staging = tempfile::tempdir().context("creating the Pix update staging directory")?;
    let archive = staging.path().join(&release.asset_name);
    if !quiet {
        println!("Downloading Pix {} ({})", release.tag, release.asset_name);
    }
    let mut arguments = vec!["-fL", "--retry", "2", "--connect-timeout", "8"];
    if quiet {
        arguments.push("-sS");
    } else {
        arguments.push("--progress-bar");
    }
    let archive_path = archive.display().to_string();
    arguments.extend_from_slice(&["-o", &archive_path, &release.asset_url]);
    run_curl_to_terminal(&arguments, "downloading the Pix release archive")?;

    let extracted = extract_cli(&archive, staging.path())?;
    let app_bundle = staging.path().join("unpacked").join("Pix.app");
    if app_bundle.is_dir()
        && let Some(home) = crate::commands::shared::home_directory()
    {
        install_app_bundle(&app_bundle, &home.join("Applications"))?;
    }
    replace_executable(&extracted, executable)
}

fn extract_cli(archive: &Path, staging: &Path) -> Result<PathBuf> {
    let name = archive.display().to_string();
    if name.to_ascii_lowercase().ends_with(".zip") {
        let unpacked = staging.join("unpacked");
        std::fs::create_dir_all(&unpacked)?;
        run_command(
            "unzip",
            &["-q", &name, &unpacked.display().to_string()],
            "unpacking the Pix release archive",
        )?;
        let cli = unpacked
            .join("Pix.app")
            .join("Contents")
            .join("Resources")
            .join("pix");
        if cli.is_file() {
            return Ok(cli);
        }
        bail!("the release archive did not contain the Pix CLI");
    }
    run_command(
        "tar",
        &["-xzf", &name, "-C", &staging.display().to_string()],
        "unpacking the Pix release archive",
    )?;
    let extracted = walk_for_pix(staging)?;
    Ok(extracted)
}

fn walk_for_pix(directory: &Path) -> Result<PathBuf> {
    fn visit(path: &Path, found: &mut Option<PathBuf>) {
        if found.is_some() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit(&path, found);
                } else if path.file_name().is_some_and(|name| name == "pix")
                    && path
                        .components()
                        .any(|component| component.as_os_str() == "bin")
                {
                    *found = Some(path);
                    return;
                }
            }
        }
    }
    let mut found = None;
    visit(directory, &mut found);
    found.context("the release archive did not contain the Pix CLI")
}

fn install_app_bundle(bundle: &Path, applications: &Path) -> Result<()> {
    std::fs::create_dir_all(applications)?;
    let destination = applications.join("Pix.app");
    if destination.is_dir() && std::fs::remove_dir_all(&destination).is_err() {
        println!("Pix.app is in use; the app bundle was not replaced.");
        return Ok(());
    }
    let status = Command::new("cp")
        .args([
            "-R",
            &bundle.display().to_string(),
            &destination.display().to_string(),
        ])
        .status()
        .context("copying the Pix app bundle")?;
    if status.success() {
        println!("Updated Pix.app in ~/Applications.");
    }
    Ok(())
}

fn replace_executable(new_binary: &Path, executable: &Path) -> Result<()> {
    let staged = executable.with_extension("pix-new");
    std::fs::copy(new_binary, &staged).context("staging the new pix executable")?;
    set_executable(&staged)?;
    std::fs::rename(&staged, executable).context("replacing the pix executable")?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn run_curl(arguments: &[&str], what: &str) -> Result<String> {
    let output = Command::new("curl")
        .args(arguments)
        .output()
        .with_context(|| format!("{what}: running curl"))?;
    if !output.status.success() {
        bail!("{what}: curl exited with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Downloads inherit the terminal so curl's progress bar stays visible.
fn run_curl_to_terminal(arguments: &[&str], what: &str) -> Result<()> {
    let status = Command::new("curl")
        .args(arguments)
        .status()
        .with_context(|| format!("{what}: running curl"))?;
    if !status.success() {
        bail!("{what}: curl exited with {status}");
    }
    Ok(())
}

fn run_command(program: &str, arguments: &[&str], what: &str) -> Result<()> {
    let status = Command::new(program)
        .args(arguments)
        .status()
        .with_context(|| format!("{what}: running {program}"))?;
    if !status.success() {
        bail!("{what}: {program} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::asset_suffix;

    #[test]
    fn release_assets_match_the_installer_contract() {
        // The suffix must stay in sync with website/public/install.sh.
        let suffix = asset_suffix().expect("supported platform");
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => assert_eq!(suffix, "macos-arm64.zip"),
            ("linux", "x86_64") => assert_eq!(suffix, "x86_64-unknown-linux-gnu.tar.gz"),
            ("linux", "aarch64") => assert_eq!(suffix, "aarch64-unknown-linux-gnu.tar.gz"),
            _ => {}
        }
    }
}
