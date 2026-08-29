use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::host_environment::HostEnvironment;
use crate::pi_rpc::{RpcClient, RpcError};
use crate::session_lock::{ProcessIdentity, SessionId, SessionLease, SessionLockError};

const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLaunch {
    Create { id: SessionId, name: Option<String> },
    Existing { id: SessionId, reference: String },
}

impl SessionLaunch {
    #[must_use]
    pub const fn id(&self) -> SessionId {
        match self {
            Self::Create { id, .. } | Self::Existing { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PiRuntimeOptions {
    pub executable: PathBuf,
    pub workspace: PathBuf,
    pub lock_directory: PathBuf,
    pub launch: SessionLaunch,
    pub extra_arguments: Vec<String>,
    /// Environment Pi runs in. Version-manager installations need the login
    /// shell environment to locate their interpreter (for example `node`).
    pub environment: HostEnvironment,
}

/// One active Pi RPC child and its exclusive session lease.
pub struct PiRuntime {
    child: Mutex<Child>,
    rpc: RpcClient,
    lease: SessionLease,
}

impl PiRuntime {
    /// Claims a session and starts Pi in RPC mode for an authorized workspace.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when paths are invalid, the session is already
    /// owned, Pi cannot be spawned, or its standard streams are unavailable.
    pub fn start(options: &PiRuntimeOptions) -> Result<Self, RuntimeError> {
        let executable = runnable_executable(&options.executable)?;
        let workspace =
            fs::canonicalize(&options.workspace).map_err(|source| RuntimeError::Canonicalize {
                path: options.workspace.clone(),
                source,
            })?;
        if !workspace.is_dir() {
            return Err(RuntimeError::WorkspaceNotDirectory(workspace));
        }

        let mut lease = SessionLease::acquire_for_workspace(
            &options.lock_directory,
            options.launch.id(),
            &workspace,
        )?;
        let mut command = Command::new(executable);
        options.environment.apply(&mut command);
        command
            .current_dir(workspace)
            .args(["--mode", "rpc", "--approve"]);
        match &options.launch {
            SessionLaunch::Create { id, name } => {
                command.args(["--session-id", &id.to_string()]);
                if let Some(name) = name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    command.args(["--name", name]);
                }
            }
            SessionLaunch::Existing { reference, .. } => {
                command.args(["--session", reference]);
            }
        }
        command
            .args(&options.extra_arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = command.spawn().map_err(RuntimeError::Spawn)?;
        let writer_identity = match ProcessIdentity::inspect(child.id()) {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                let pid = child.id();
                cleanup_spawned_child(&mut child);
                return Err(RuntimeError::SessionLock(
                    SessionLockError::ProcessNotFound(pid),
                ));
            }
            Err(error) => {
                cleanup_spawned_child(&mut child);
                return Err(RuntimeError::SessionLock(error));
            }
        };
        if let Err(error) = lease.set_writer_process(writer_identity) {
            cleanup_spawned_child(&mut child);
            return Err(RuntimeError::SessionLock(error));
        }
        let Some(input) = child.stdin.take() else {
            cleanup_spawned_child(&mut child);
            return Err(RuntimeError::MissingStdin);
        };
        let Some(output) = child.stdout.take() else {
            cleanup_spawned_child(&mut child);
            return Err(RuntimeError::MissingStdout);
        };
        let rpc = match RpcClient::new(input, output) {
            Ok(rpc) => rpc,
            Err(error) => {
                cleanup_spawned_child(&mut child);
                return Err(error.into());
            }
        };
        Ok(Self {
            child: Mutex::new(child),
            rpc,
            lease,
        })
    }

    #[must_use]
    pub const fn rpc(&self) -> &RpcClient {
        &self.rpc
    }

    #[must_use]
    pub const fn lease(&self) -> &SessionLease {
        &self.lease
    }

    /// Reports whether the Pi child has exited without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] if the operating system cannot inspect the
    /// child process.
    pub fn try_wait(&self) -> Result<Option<ExitStatus>, RuntimeError> {
        self.child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_wait()
            .map_err(RuntimeError::Wait)
    }

    /// Terminates Pi, waits for exit, and releases its session lease.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] if process signaling or waiting fails.
    pub fn stop(self) -> Result<ExitStatus, RuntimeError> {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(status) = child.try_wait().map_err(RuntimeError::Wait)? {
            self.rpc.close();
            return Ok(status);
        }
        terminate(child.id())?;
        let deadline = Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait().map_err(RuntimeError::Wait)? {
                self.rpc.close();
                return Ok(status);
            }
            if Instant::now() >= deadline {
                child.kill().map_err(RuntimeError::Terminate)?;
                let status = child.wait().map_err(RuntimeError::Wait)?;
                self.rpc.close();
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for PiRuntime {
    fn drop(&mut self) {
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if child.try_wait().ok().flatten().is_none() {
            let _ = terminate(child.id());
            let deadline = Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT;
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn runnable_executable(path: &std::path::Path) -> Result<PathBuf, RuntimeError> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    fs::canonicalize(path).map_err(|source| RuntimeError::Canonicalize {
        path: path.to_path_buf(),
        source,
    })
}

/// Best-effort cleanup for a child that was spawned before `PiRuntime` finished
/// initializing its RPC and lease state. Startup errors must not orphan Pi.
fn cleanup_spawned_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = terminate(child.id());
    let deadline = Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn terminate(pid: u32) -> Result<(), RuntimeError> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .map_err(RuntimeError::Terminate)?;
    if status.success() {
        Ok(())
    } else {
        Err(RuntimeError::SignalRejected { pid, status })
    }
}

#[cfg(not(unix))]
fn terminate(_pid: u32) -> Result<(), RuntimeError> {
    Err(RuntimeError::UnsupportedPlatform)
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("failed to canonicalize {path}: {source}")]
    Canonicalize { path: PathBuf, source: io::Error },
    #[error("authorized workspace is not a directory: {0}")]
    WorkspaceNotDirectory(PathBuf),
    #[error(transparent)]
    SessionLock(#[from] SessionLockError),
    #[error("failed to spawn Pi: {0}")]
    Spawn(io::Error),
    #[error("Pi child has no stdin")]
    MissingStdin,
    #[error("Pi child has no stdout")]
    MissingStdout,
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("failed to terminate Pi: {0}")]
    Terminate(io::Error),
    #[error("failed waiting for Pi: {0}")]
    Wait(io::Error),
    #[error("the operating system rejected SIGTERM for Pi process {pid}: {status}")]
    SignalRejected { pid: u32, status: ExitStatus },
    #[error("graceful Pi termination is unsupported on this platform")]
    UnsupportedPlatform,
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{PiRuntime, PiRuntimeOptions, runnable_executable};
    use crate::host_environment::HostEnvironment;
    use crate::pi_rpc::PiCommand;
    use crate::session_lock::SessionId;

    use super::SessionLaunch;

    #[test]
    fn keeps_version_manager_shim_path() {
        let directory = tempdir().expect("temporary directory");
        let dispatcher = directory.path().join("mise");
        let shim = directory.path().join("pi");
        fs::write(&dispatcher, b"#!/bin/sh\nexit 0\n").expect("write dispatcher");
        symlink(&dispatcher, &shim).expect("create shim");

        assert_eq!(runnable_executable(&shim).expect("runnable shim"), shim);
        assert_ne!(
            runnable_executable(&shim).expect("runnable shim"),
            dispatcher
        );
    }

    #[test]
    fn executable_directory_supplies_env_interpreter_for_gui_environment() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().expect("temporary executable directory");
        let executable = directory.path().join("pi");
        let interpreter = directory.path().join("node");
        fs::write(&interpreter, "#!/bin/sh\nexec /bin/sh \"$@\"\n")
            .expect("write fake node interpreter");
        fs::write(
            &executable,
            r#"#!/usr/bin/env node
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"sessionId\":\"fake-session\"}}"
done
"#,
        )
        .expect("write env-based Pi script");
        for path in [&interpreter, &executable] {
            let mut permissions = fs::metadata(path).expect("script metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("make script executable");
        }

        let workspace = tempdir().expect("temporary workspace");
        let locks = tempdir().expect("temporary lock directory");
        let environment = HostEnvironment::captured_for_tests(
            PathBuf::from("/bin/zsh"),
            vec![
                (OsString::from("HOME"), OsString::from("/Users/example")),
                (
                    OsString::from("PATH"),
                    OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"),
                ),
            ],
        );
        let runtime = PiRuntime::start(&PiRuntimeOptions {
            executable,
            workspace: workspace.path().to_path_buf(),
            lock_directory: locks.path().to_path_buf(),
            launch: SessionLaunch::Create {
                id: SessionId::new(),
                name: Some("GUI service regression".to_owned()),
            },
            extra_arguments: Vec::new(),
            environment,
        })
        .expect("start env-based Pi");
        assert!(runtime.lease().record().workspace_fingerprint.is_some());
        assert!(runtime.lease().record().writer_pid.is_some());
        assert!(
            runtime
                .lease()
                .record()
                .writer_process_start_identity
                .is_some()
        );

        let response = runtime
            .rpc()
            .request(&PiCommand::GetState, Duration::from_secs(10))
            .expect("Pi resolves the sibling interpreter and answers RPC");
        assert_eq!(response.command, "get_state");
        runtime.stop().expect("stop fake Pi");
    }
}
