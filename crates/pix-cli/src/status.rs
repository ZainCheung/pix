//! Host-service status file used by `pix serve` and `pix status`.
//!
//! The status file deliberately contains only process supervision fields
//! (`pid`, `port`, `started_at`). It never contains the host private key,
//! workspace paths, device identifiers, or session content.

use std::fs;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::Builder;

const LOCAL_CONTROL_SCHEMA_VERSION: u32 = 1;
const MAX_CONTROL_REQUEST_BYTES: u64 = 64 * 1024;

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
        prepare_private_run_directory(config_path)?;
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

/// One command accepted from the private local control socket.
pub enum HostControlCommand {
    /// The original one-line interface used by existing native clients. Its
    /// receipt acknowledgement has already been written to the caller.
    Legacy(String),
    /// A versioned request whose responder is completed after execution.
    Rpc {
        command: String,
        args: serde_json::Value,
        responder: HostControlResponder,
    },
}

/// Single-use response channel for a versioned local control request.
pub struct HostControlResponder {
    #[cfg(unix)]
    stream: UnixStream,
    request_id: uuid::Uuid,
}

impl HostControlResponder {
    pub fn success(self, data: &serde_json::Value) -> Result<()> {
        let request_id = self.request_id;
        self.write(serde_json::json!({
            "schema_version": LOCAL_CONTROL_SCHEMA_VERSION,
            "request_id": request_id,
            "ok": true,
            "data": data,
        }))
    }

    pub fn error(self, code: &str, message: &str) -> Result<()> {
        let request_id = self.request_id;
        self.write(serde_json::json!({
            "schema_version": LOCAL_CONTROL_SCHEMA_VERSION,
            "request_id": request_id,
            "ok": false,
            "error": {
                "code": code,
                "message": message,
            },
        }))
    }

    #[allow(clippy::needless_pass_by_value)]
    fn write(mut self, response: serde_json::Value) -> Result<()> {
        #[cfg(unix)]
        {
            serde_json::to_writer(&mut self.stream, &response)
                .context("encoding host control response")?;
            self.stream.write_all(b"\n")?;
            self.stream.flush()?;
        }
        #[cfg(not(unix))]
        {
            let _ = response;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct HostControlRpcRequest {
    schema_version: u32,
    request_id: uuid::Uuid,
    command: String,
    #[serde(default)]
    args: serde_json::Value,
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
        prepare_private_run_directory(config_path)?;
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
    pub fn try_next_command(&self) -> Result<Option<HostControlCommand>> {
        #[cfg(unix)]
        {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // Linux may inherit the listener's nonblocking flag on
                    // accepted Unix sockets. This command path reads one
                    // complete line synchronously, so restore blocking mode
                    // before applying the bounded read timeout.
                    // A client that already closed (probe-style connect and
                    // drop) can make socket setup fail on macOS with EINVAL.
                    // Its connection is dropped; the host keeps serving.
                    if stream.set_nonblocking(false).is_err()
                        || stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                            .is_err()
                    {
                        return Ok(None);
                    }
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    match reader
                        .by_ref()
                        .take(MAX_CONTROL_REQUEST_BYTES + 1)
                        .read_line(&mut line)
                    {
                        // A probe client may connect and never send, and a
                        // stalled sender may hit the read timeout. Dropping
                        // that one connection must never take the host down.
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::UnexpectedEof
                            ) =>
                        {
                            return Ok(None);
                        }
                        Err(error) => {
                            return Err(error).context("reading host service control command");
                        }
                        Ok(_) => {}
                    }
                    let mut stream = reader.into_inner();
                    let command = line.trim().to_owned();
                    if command.len() as u64 > MAX_CONTROL_REQUEST_BYTES {
                        let _ = write_rpc_error(
                            &mut stream,
                            None,
                            "invalid_request",
                            "host control request is too large",
                        );
                        return Ok(None);
                    }
                    if command.starts_with('{') {
                        let request = match serde_json::from_str::<HostControlRpcRequest>(&command)
                        {
                            Ok(request) => request,
                            Err(error) => {
                                let _ = write_rpc_error(
                                    &mut stream,
                                    None,
                                    "invalid_request",
                                    &format!("invalid host control request: {error}"),
                                );
                                return Ok(None);
                            }
                        };
                        if request.schema_version != LOCAL_CONTROL_SCHEMA_VERSION {
                            let _ = write_rpc_error(
                                &mut stream,
                                Some(request.request_id),
                                "unsupported_version",
                                "unsupported host control schema version",
                            );
                            return Ok(None);
                        }
                        return Ok(Some(HostControlCommand::Rpc {
                            command: request.command,
                            args: request.args,
                            responder: HostControlResponder {
                                stream,
                                request_id: request.request_id,
                            },
                        }));
                    }
                    if !command.is_empty() {
                        let _ = stream.write_all(b"ok\n");
                        let _ = stream.flush();
                    }
                    Ok(Some(HostControlCommand::Legacy(command)))
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
fn write_rpc_error(
    stream: &mut UnixStream,
    request_id: Option<uuid::Uuid>,
    code: &str,
    message: &str,
) -> Result<()> {
    serde_json::to_writer(
        &mut *stream,
        &serde_json::json!({
            "schema_version": LOCAL_CONTROL_SCHEMA_VERSION,
            "request_id": request_id,
            "ok": false,
            "error": {"code": code, "message": message},
        }),
    )?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
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

#[cfg(unix)]
fn prepare_private_run_directory(config_path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let config_directory = config_path
        .parent()
        .context("locating Pix configuration directory")?;
    let owner = fs::metadata(config_directory)
        .with_context(|| format!("inspecting {}", config_directory.display()))?
        .uid();
    let run_directory = config_directory.join("run");
    match fs::symlink_metadata(&run_directory) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.uid() != owner {
                bail!(
                    "Pix run directory is not a user-owned real directory: {}",
                    run_directory.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&run_directory)
                .with_context(|| format!("creating {}", run_directory.display()))?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting {}", run_directory.display()));
        }
    }
    fs::set_permissions(&run_directory, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("securing {}", run_directory.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn prepare_private_run_directory(config_path: &Path) -> Result<()> {
    let run_directory = config_path
        .parent()
        .context("locating Pix configuration directory")?
        .join("run");
    fs::create_dir_all(&run_directory)
        .with_context(|| format!("creating {}", run_directory.display()))
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

/// Checks the config-scoped control listener and removes only definitively
/// stale Unix socket artifacts. A successful connect is the authority; the
/// status file is intentionally not used as a liveness gate.
pub fn control_socket_live(config_path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt as _;

        let control_path = HostServiceStatus::control_socket_path_for(config_path);
        match UnixStream::connect(&control_path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                for path in [
                    control_path,
                    HostServiceStatus::event_socket_path_for(config_path),
                ] {
                    if fs::symlink_metadata(&path)
                        .is_ok_and(|metadata| metadata.file_type().is_socket())
                    {
                        let _ = fs::remove_file(path);
                    }
                }
                let _ = HostServiceStatus::current(config_path);
                Ok(false)
            }
            Err(error) => Err(error).context("probing the Pix host control socket"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = config_path;
        Ok(false)
    }
}

/// Sends one versioned request and waits for its execution response on the
/// same private control socket. A legacy daemon replies with its historical
/// `ok` receipt, which is detected before any domain mutation is attempted.
#[cfg(unix)]
pub fn request_control_rpc(
    config_path: &Path,
    request: &serde_json::Value,
    timeout: std::time::Duration,
) -> Result<serde_json::Value> {
    let path = HostServiceStatus::control_socket_path_for(config_path);
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "connecting to host service control socket {}; run `pix service start` first",
            path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .context("configuring host control response timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("configuring host control request timeout")?;
    stream
        .set_nonblocking(false)
        .context("configuring blocking host control client")?;
    serde_json::to_writer(&mut stream, request).context("encoding host control request")?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(MAX_CONTROL_REQUEST_BYTES + 1)
        .read_line(&mut response)
        .context("reading host control response")?;
    if response.trim() == "ok" {
        bail!(
            "the running Pix host predates versioned control responses; restart it with `pix service restart` and retry"
        );
    }
    if response.trim().is_empty() {
        bail!(
            "the running Pix host closed the control connection without a response; it is likely older than this CLI — restart it with `pix service restart` and retry"
        );
    }
    if response.len() as u64 > MAX_CONTROL_REQUEST_BYTES {
        bail!("host control response is too large");
    }
    serde_json::from_str(response.trim()).context("decoding host control response")
}

#[cfg(not(unix))]
pub fn request_control_rpc(
    _config_path: &Path,
    _request: &serde_json::Value,
    _timeout: std::time::Duration,
) -> Result<serde_json::Value> {
    bail!("versioned Pix host control is currently available only on Unix hosts")
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

pub(crate) fn show_logs(store: &ConfigStore, tail: usize, output: CommandOutput) -> Result<()> {
    let path = HostLog::path_for(store.path());
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(tail);
            if output.is_json() {
                let entries = lines[start..]
                    .iter()
                    .map(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .unwrap_or_else(|_| serde_json::Value::String((*line).to_owned()))
                    })
                    .collect::<Vec<_>>();
                return output.success(
                    "logs",
                    &serde_json::json!({"path": path, "entries": entries}),
                );
            }
            println!("log file: {}", path.display());
            for line in &lines[start..] {
                println!("{line}");
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if output.is_json() {
                return output.success("logs", &serde_json::json!({"path": path, "entries": []}));
            }
            println!("log file: {}", path.display());
            println!("(no log entries yet)");
            Ok(())
        }
        Err(error) => Err(error).context("reading host log"),
    }
}

pub(crate) fn status_command(store: &ConfigStore, output: CommandOutput) -> Result<()> {
    let overview = HostOverview::collect(store);
    if output.is_json() {
        return output.success("status", &overview);
    }
    if std::io::stdout().is_terminal() {
        home::render_overview(&overview, SetupUi::new(true, false), true, None);
        return Ok(());
    }
    legacy_status_command(store, &overview)
}

pub(crate) fn legacy_status_command(store: &ConfigStore, overview: &HostOverview) -> Result<()> {
    println!("Pix status");
    println!("  config: {}", store.path().display());
    match store.load() {
        Ok(config) => {
            println!("  host: {}", terminal_label(&config.host.display_name));
            println!(
                "  host config: ok ({} workspace{}, {} paired device{})",
                config.workspaces.len(),
                plural(config.workspaces.len()),
                config.devices.len(),
                plural(config.devices.len())
            );
            match &config.preferences.relay_url {
                Some(url) if config.preferences.relay_enabled => {
                    println!("  relay: {url} (enabled)");
                }
                Some(url) => println!("  relay: {url} (disabled)"),
                None => println!("  relay: not configured"),
            }
            let pi_source = if config.preferences.pi_executable.is_some() {
                "configured"
            } else {
                "PATH discovery"
            };
            match &overview.pi.version {
                Some(version) => println!("  pi: {pi_source} ({version})"),
                None => println!("  pi: {pi_source}"),
            }
        }
        Err(pix_core::config::ConfigError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            println!("  host config: not created yet");
        }
        Err(error) => bail!("host config: {error}"),
    }

    if let Some(current) = crate::status::HostServiceStatus::current(store.path()) {
        println!(
            "  service: running (pid {}, port {}, started_at {})",
            current.pid, current.port, current.started_at
        );
    } else {
        let installed = service::managed_service_installed(store).unwrap_or(false);
        let active = service::managed_service_active(store).unwrap_or(false);
        if active {
            println!("  service: manager active (host status is not ready yet)");
        } else if installed {
            println!("  service: installed but not running");
        } else {
            println!("  service: not running");
        }
    }
    Ok(())
}

use crate::commands::shared::{plural, terminal_label};
use crate::home::HostOverview;
use crate::output::CommandOutput;
use crate::serve::HostLog;
use crate::setup_ui::SetupUi;
use crate::{home, service};
use pix_core::ConfigStore;

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

    #[cfg(unix)]
    #[test]
    fn probe_clients_never_kill_the_host() {
        use std::os::unix::net::UnixStream;
        use std::time::{Duration, Instant};

        // `host_service_control_live` probes liveness by connecting and
        // dropping immediately. On macOS the daemon may accept that socket
        // after the peer vanished; socket setup must degrade to dropping
        // the connection instead of failing the whole host.
        let directory = tempdir().expect("temp dir");
        let config_path = directory.path().join("config.json");
        let control =
            super::HostServiceControl::bind(&config_path).expect("bind host service sockets");
        for _ in 0..8 {
            drop(
                UnixStream::connect(HostServiceStatus::control_socket_path_for(&config_path))
                    .expect("probe connect"),
            );
            std::thread::sleep(Duration::from_millis(10));
            // The loop must keep accepting after each dead peer.
            let deadline = Instant::now() + Duration::from_secs(2);
            let _ = control.try_next_command().expect("poll after probe");
            let _ = deadline;
        }
    }

    #[cfg(unix)]
    #[test]
    fn silent_control_client_never_takes_the_host_down() {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;
        use std::time::{Duration, Instant};

        let directory = tempdir().expect("temp dir");
        let config_path = directory.path().join("config.json");
        let control =
            super::HostServiceControl::bind(&config_path).expect("bind host service sockets");

        // A probe client connects and never sends anything. The host must
        // drop it after the read timeout instead of surfacing an error that
        // would terminate `pix serve`.
        let silent = UnixStream::connect(HostServiceStatus::control_socket_path_for(&config_path))
            .expect("connect without sending");
        let started = Instant::now();
        loop {
            match control.try_next_command().expect("poll control command") {
                Some(_) => panic!("unexpected control command from a silent client"),
                None if started.elapsed() >= Duration::from_millis(1100) => break,
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        drop(silent);

        // The control loop keeps serving well-behaved clients afterwards.
        let mut client =
            UnixStream::connect(HostServiceStatus::control_socket_path_for(&config_path))
                .expect("connect well-behaved client");
        client
            .write_all(br#"{"schema_version":1,"request_id":"0199aaaa-f00d-7aa0-a0aa-000000000001","command":"status"}"#)
            .expect("send control request");
        client.write_all(b"\n").expect("send control newline");
        client.flush().expect("flush control request");
        let started = Instant::now();
        loop {
            if let Some(super::HostControlCommand::Rpc { command, .. }) =
                control.try_next_command().expect("poll control command")
            {
                assert_eq!(command, "status");
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "well-behaved control client was never served"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
