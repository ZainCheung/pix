//! Host-service status file used by `pix serve` and `pix status`.
//!
//! The status file deliberately contains only process supervision fields
//! (`pid`, `port`, `started_at`). It never contains the host private key,
//! workspace paths, device identifiers, or session content.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::Builder;

#[cfg(unix)]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(all(unix, not(target_os = "linux")))]
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostServiceStatus {
    pub pid: u32,
    pub port: u16,
    pub started_at: u64,
    /// Process start identity prevents a reused PID from making an unrelated
    /// process look like Pix. On Linux this contains the `/proc` start tick
    /// and executable identity.
    #[serde(default)]
    pub process_start_identity: String,
}

impl HostServiceStatus {
    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("run")
            .join("host-service.json")
    }

    pub fn control_socket_path_for(config_path: &Path) -> PathBuf {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("run")
            .join("host-service.sock")
    }

    pub fn event_socket_path_for(config_path: &Path) -> PathBuf {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("run")
            .join("host-events.sock")
    }

    /// Writes a new status file atomically.
    ///
    /// # Errors
    ///
    /// Returns an error when the status file cannot be created or persisted.
    pub fn write(config_path: &Path, port: u16) -> Result<Self> {
        let pid = std::process::id();
        let status = Self {
            pid,
            port,
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            process_start_identity: process_identity(pid)
                .context("inspecting Pix process identity")?,
        };
        let path = Self::path_for(config_path);
        let parent = path
            .parent()
            .context("locating host service status directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "creating host service status directory {}",
                parent.display()
            )
        })?;
        let mut temporary = Builder::new()
            .prefix(".pix-host-service-status-")
            .tempfile_in(parent)
            .with_context(|| {
                format!(
                    "creating host service status temporary file in {}",
                    parent.display()
                )
            })?;
        temporary
            .write_all(
                serde_json::to_vec(&status)
                    .context("encoding host service status")?
                    .as_slice(),
            )
            .and_then(|()| temporary.as_file_mut().sync_all())
            .with_context(|| {
                format!(
                    "writing host service status temporary file {}",
                    temporary.path().display()
                )
            })?;
        temporary.persist(&path).map_err(|error| {
            anyhow::anyhow!(
                "persisting host service status to {}: {}",
                path.display(),
                error.error
            )
        })?;
        Ok(status)
    }

    /// Reads the current status file without validating process liveness.
    pub fn read(config_path: &Path) -> Option<Self> {
        let path = Self::path_for(config_path);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.file_type().is_file() {
            return None;
        }
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Removes a stale status file, but only when this process owns it.
    pub fn remove_if_owned(config_path: &Path) {
        let Some(status) = Self::read(config_path) else {
            return;
        };
        if status.pid == std::process::id()
            && process_identity(status.pid).as_deref()
                == Some(status.process_start_identity.as_str())
        {
            let _ = fs::remove_file(Self::path_for(config_path));
        }
    }

    /// Returns the current status only when the PID, process identity, and
    /// recorded Pix listener all still agree. A stale or reused status file is
    /// removed so later commands do not inherit false liveness.
    pub fn current(config_path: &Path) -> Option<Self> {
        let status = Self::read(config_path)?;
        let process_matches = process_identity(status.pid)
            .as_deref()
            .is_some_and(|identity| identity == status.process_start_identity);
        if process_matches && port_is_listening(status.port) {
            Some(status)
        } else {
            let _ = fs::remove_file(Self::path_for(config_path));
            None
        }
    }
}

/// Removes the status file when the owning `pix serve` exits.
pub struct HostServiceStatusGuard {
    config_path: PathBuf,
}

impl HostServiceStatusGuard {
    /// Writes the status file and returns a guard that removes it on drop.
    pub fn create(config_path: &Path, port: u16) -> Result<Self> {
        HostServiceStatus::write(config_path, port)?;
        Ok(Self {
            config_path: config_path.to_path_buf(),
        })
    }
}

impl Drop for HostServiceStatusGuard {
    fn drop(&mut self) {
        HostServiceStatus::remove_if_owned(&self.config_path);
    }
}

fn port_is_listening(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(100),
    )
    .is_ok()
}

#[cfg(target_os = "linux")]
fn process_identity(pid: u32) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let closing_parenthesis = stat.rfind(')')?;
    // `/proc/<pid>/stat` field 22 is the process start tick. Everything after
    // the command name starts at field 3, so it is item 19 in this split.
    let start_ticks = stat[closing_parenthesis + 1..].split_whitespace().nth(19)?;
    let executable = fs::metadata(format!("/proc/{pid}/exe")).ok()?;
    Some(format!("{}:{start_ticks}", executable.ino()))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_identity(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let start = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!start.is_empty()).then_some(start)
}

#[cfg(not(unix))]
fn process_identity(_pid: u32) -> Option<String> {
    None
}

/// Private local control/event sockets used by the platform-managed service.
/// They let the daemon stop cleanly without treating a manager's stdin
/// (normally `/dev/null`) as a lifecycle signal and give native clients a
/// transient JSONL event bridge.
pub struct HostServiceControl {
    #[cfg(unix)]
    listener: UnixListener,
    path: PathBuf,
    #[cfg(unix)]
    event_listener: UnixListener,
    #[cfg(unix)]
    event_path: PathBuf,
    #[cfg(unix)]
    event_subscribers: Vec<UnixStream>,
}

impl HostServiceControl {
    pub fn bind(config_path: &Path) -> Result<Self> {
        #[cfg(unix)]
        return Self::bind_unix(config_path);

        #[cfg(not(unix))]
        {
            let _ = config_path;
            bail!("Pix service control is unavailable on this platform")
        }
    }

    #[cfg(unix)]
    fn bind_unix(config_path: &Path) -> Result<Self> {
        let path = HostServiceStatus::control_socket_path_for(config_path);
        let parent = path
            .parent()
            .context("locating host service control directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "creating host service control directory {}",
                parent.display()
            )
        })?;
        let listener = bind_socket(&path, "control")?;
        let event_path = HostServiceStatus::event_socket_path_for(config_path);
        let event_listener = bind_socket(&event_path, "event")?;
        Ok(Self {
            listener,
            path,
            event_listener,
            event_path,
            event_subscribers: Vec::new(),
        })
    }

    /// Polls one local command without blocking the host event loop.
    pub fn try_next_command(&self) -> Result<Option<String>> {
        #[cfg(unix)]
        {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // Linux may inherit the listener's nonblocking flag on
                    // accepted Unix sockets. This command path reads one
                    // complete line synchronously, so restore blocking mode
                    // before applying the bounded read timeout.
                    stream
                        .set_nonblocking(false)
                        .context("configuring blocking host service control")?;
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                        .context("configuring host service control client")?;
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader
                        .read_line(&mut line)
                        .context("reading host service control command")?;
                    let mut stream = reader.into_inner();
                    let command = line.trim().to_owned();
                    if !command.is_empty() {
                        stream.write_all(b"ok\n")?;
                    }
                    stream.flush()?;
                    Ok(Some(command))
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
                Err(error) => Err(error).context("accepting host service control command"),
            }
        }
        #[cfg(not(unix))]
        {
            Ok(None)
        }
    }

    /// Accepts local UI event subscribers without blocking the host loop.
    pub fn poll_event_subscribers(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            loop {
                match self.event_listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_nonblocking(true)
                            .context("configuring host event subscriber")?;
                        self.event_subscribers.push(stream);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error).context("accepting host event subscriber"),
                }
            }
        }
        Ok(())
    }

    /// Publishes one JSONL event to each connected local UI subscriber.
    ///
    /// The socket is mode 0600 and never persists events. A stalled subscriber
    /// is dropped so a menu app cannot block the host or its secure channels.
    pub fn publish_event(&mut self, line: &str) -> Result<()> {
        self.poll_event_subscribers()?;
        #[cfg(unix)]
        {
            let mut bytes = line.as_bytes().to_vec();
            bytes.push(b'\n');
            self.event_subscribers
                .retain_mut(|stream| stream.write_all(&bytes).is_ok());
        }
        Ok(())
    }
}

#[cfg(unix)]
fn bind_socket(path: &Path, kind: &str) -> Result<UnixListener> {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::PermissionsExt;

    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            bail!(
                "refusing to replace non-socket {kind} path {}",
                path.display()
            );
        }
        match UnixStream::connect(path) {
            Ok(_) => bail!("a Pix host service already owns {}", path.display()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                fs::remove_file(path)
                    .with_context(|| format!("removing stale {kind} socket {}", path.display()))?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("checking existing {kind} socket {}", path.display())
                });
            }
        }
    }

    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding host {kind} socket {}", path.display()))?;
    listener
        .set_nonblocking(true)
        .with_context(|| format!("configuring host service {kind} socket"))?;
    // The socket is a local command surface and must not be writable or
    // readable by another user.
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)?;
    Ok(listener)
}

impl Drop for HostServiceControl {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if fs::symlink_metadata(&self.path)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
            {
                let _ = fs::remove_file(&self.path);
            }
            if fs::symlink_metadata(&self.event_path)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
            {
                let _ = fs::remove_file(&self.event_path);
            }
        }
    }
}

/// Sends a command to a running host service. `Ok(false)` means no control
/// socket exists, which lets `service stop` remain idempotent for a stopped
/// service while still failing closed when a live daemon is unreachable.
pub fn request_control_command(config_path: &Path, command: &str) -> Result<bool> {
    #[cfg(unix)]
    {
        let path = HostServiceStatus::control_socket_path_for(config_path);
        let mut stream = match UnixStream::connect(&path) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(false);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "connecting to host service control socket {}",
                        path.display()
                    )
                });
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(8)))
            .context("configuring host service control client")?;
        // Some Unix implementations establish the stream with nonblocking
        // semantics while connecting. The response is read synchronously,
        // so make that contract explicit after applying the timeout.
        stream
            .set_nonblocking(false)
            .context("configuring blocking host service control client")?;
        stream.write_all(command.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        stream.shutdown(Shutdown::Write)?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response)?;
        if response.trim() == "ok" {
            Ok(true)
        } else {
            bail!("host service rejected control command")
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (config_path, command);
        Ok(false)
    }
}

/// Connects to the ephemeral JSONL event stream of a running host service.
/// Events are never persisted by this socket; subscribers receive only events
/// emitted after their connection is accepted.
#[cfg(unix)]
pub fn connect_event_stream(config_path: &Path) -> Result<UnixStream> {
    let path = HostServiceStatus::event_socket_path_for(config_path);
    UnixStream::connect(&path).with_context(|| {
        format!(
            "connecting to host service event socket {}; run `pix service start` first",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::BufRead;

    use tempfile::tempdir;

    use std::net::TcpListener;

    use super::{HostServiceStatus, HostServiceStatusGuard};

    #[test]
    fn status_file_round_trips() {
        let directory = tempdir().expect("temp dir");
        let config_path = directory.path().join("config.json");
        let guard = HostServiceStatusGuard::create(&config_path, 4123).expect("write status");
        let status = HostServiceStatus::read(&config_path).expect("read status");
        assert_eq!(status.port, 4123);
        assert_eq!(status.pid, std::process::id());
        assert!(!status.process_start_identity.is_empty());
        drop(guard);
        assert!(HostServiceStatus::read(&config_path).is_none());
    }

    #[test]
    fn current_requires_the_recorded_process_and_listener() {
        let directory = tempdir().expect("temp dir");
        let config_path = directory.path().join("config.json");
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("listener address").port();
        let status = HostServiceStatus::write(&config_path, port).expect("write status");
        assert_eq!(
            HostServiceStatus::current(&config_path),
            Some(status.clone())
        );
        let mut mismatched = status;
        mismatched.process_start_identity = "different-process".to_owned();
        fs::write(
            HostServiceStatus::path_for(&config_path),
            serde_json::to_vec(&mismatched).expect("encode mismatched status"),
        )
        .expect("write mismatched status");
        assert!(HostServiceStatus::current(&config_path).is_none());
        drop(listener);
    }

    #[cfg(unix)]
    #[test]
    fn event_socket_publishes_jsonl_without_persisting_events() {
        use std::os::unix::net::UnixStream;
        use std::time::Duration;

        let directory = tempdir().expect("temp dir");
        let config_path = directory.path().join("config.json");
        let mut control =
            super::HostServiceControl::bind(&config_path).expect("bind host service sockets");
        let mut client =
            UnixStream::connect(HostServiceStatus::event_socket_path_for(&config_path))
                .expect("connect event socket");
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set read timeout");
        control
            .poll_event_subscribers()
            .expect("accept event subscriber");
        control
            .publish_event(r#"{"type":"ready"}"#)
            .expect("publish event");
        let mut line = String::new();
        std::io::BufReader::new(&mut client)
            .read_line(&mut line)
            .expect("read event");
        assert_eq!(line.trim(), r#"{"type":"ready"}"#);
        assert!(!HostServiceStatus::event_socket_path_for(&config_path).is_file());
        assert!(HostServiceStatus::event_socket_path_for(&config_path).exists());
    }
}
