//! Resolution of the user's login shell environment.
//!
//! GUI-launched host processes (the macOS menu bar app, or a systemd user
//! service on Linux) do not inherit the `PATH` that version managers such as
//! mise, nvm, asdf, volta, or bun configure in shell initialization files.
//! Discovering Pi with the bare process environment therefore fails, and even
//! an absolute Pi path may not start when its interpreter (for example
//! `node`) is missing from `PATH`.
//!
//! [`HostEnvironment`] captures the variables exported by the user's login
//! shell once, and Pi discovery, probing, and spawning all use that same
//! capture. When no shell produces a usable capture, the current process
//! environment is used unchanged.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// How long one login shell may take to print its environment before Pix
/// gives up on it and tries the next candidate.
#[cfg(unix)]
const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);

/// Variables that describe the capture invocation itself rather than the
/// user's configuration; forwarding them to Pi children would be misleading.
#[cfg(unix)]
const EXCLUDED_VARIABLES: [&str; 4] = ["_", "OLDPWD", "PWD", "SHLVL"];

/// Origin of a [`HostEnvironment`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentSource {
    /// Captured from the user's login shell.
    LoginShell {
        /// The shell binary that produced the capture.
        shell: PathBuf,
    },
    /// Inherited unchanged from the current process.
    Process,
}

/// The environment Pix uses to locate and run the Pi executable.
#[derive(Clone)]
pub struct HostEnvironment {
    source: EnvironmentSource,
    /// Captured variables; `None` leaves the process environment untouched.
    variables: Option<Vec<(OsString, OsString)>>,
}

impl HostEnvironment {
    /// Prefers the current process environment when it can already locate
    /// `executable` (terminal launches keep their exact environment), and
    /// otherwise captures the login shell environment (GUI and service
    /// launches, where version-manager `PATH` entries are missing).
    #[must_use]
    pub fn resolve_for(executable: &str) -> Self {
        let process = Self::from_process();
        if process.find_executable(executable).is_some() {
            return process;
        }
        Self::resolve()
    }

    /// Captures the user's login shell environment, falling back to the
    /// current process environment when no shell produces a usable capture.
    #[must_use]
    pub fn resolve() -> Self {
        #[cfg(unix)]
        {
            Self::resolve_with(env::var_os("SHELL").as_deref(), RESOLUTION_TIMEOUT)
        }
        #[cfg(not(unix))]
        {
            Self::from_process()
        }
    }

    /// Uses the current process environment unchanged.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            source: EnvironmentSource::Process,
            variables: None,
        }
    }

    #[cfg(unix)]
    fn resolve_with(shell: Option<&OsStr>, timeout: Duration) -> Self {
        for candidate in shell_candidates(shell) {
            if let Some(variables) = capture_login_environment(&candidate, timeout) {
                return Self {
                    source: EnvironmentSource::LoginShell {
                        shell: candidate.program,
                    },
                    variables: Some(variables),
                };
            }
        }
        Self::from_process()
    }

    #[must_use]
    pub const fn source(&self) -> &EnvironmentSource {
        &self.source
    }

    /// Human-readable origin for diagnostics; never includes variable values.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.source {
            EnvironmentSource::LoginShell { shell } => {
                format!("login shell ({})", shell.display())
            }
            EnvironmentSource::Process => "process environment".to_owned(),
        }
    }

    /// The `PATH` value this environment would give a spawned child.
    #[must_use]
    pub fn path(&self) -> Option<OsString> {
        match &self.variables {
            Some(variables) => variables
                .iter()
                .find(|(key, _)| key == "PATH")
                .map(|(_, value)| value.clone()),
            None => env::var_os("PATH"),
        }
    }

    /// Number of non-empty `PATH` entries, for payload-free diagnostics.
    #[must_use]
    pub fn path_entry_count(&self) -> usize {
        self.path().map_or(0, |path| {
            env::split_paths(&path)
                .filter(|entry| !entry.as_os_str().is_empty())
                .count()
        })
    }

    /// Returns one captured environment value. Pi preferences use this to
    /// honor the same `PI_CODING_AGENT_DIR` that a spawned Pi child sees.
    #[must_use]
    pub fn value(&self, name: &str) -> Option<OsString> {
        match &self.variables {
            Some(variables) => variables
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone()),
            None => env::var_os(name),
        }
    }

    /// Finds `name` on this environment's `PATH`.
    ///
    /// The matching `PATH` entry is returned as-is. Version managers such as
    /// mise expose `pi` as a shim symlink whose target dispatches on
    /// `argv[0]`; canonicalizing the shim would run the wrong program.
    #[must_use]
    pub fn find_executable(&self, name: &str) -> Option<PathBuf> {
        let path = self.path()?;
        env::split_paths(&path)
            .filter(|directory| !directory.as_os_str().is_empty())
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    }

    /// Creates a command for `program` that runs inside this environment.
    #[must_use]
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        self.apply(&mut command);
        command
    }

    /// Replaces the command's environment with the captured variables.
    /// Process-sourced environments leave the command inheriting as usual.
    ///
    /// When `program` is an absolute or relative path, its parent directory is
    /// prepended to `PATH`. Version-manager installs commonly place a `pi`
    /// script and its `/usr/bin/env node` interpreter beside each other; GUI
    /// services can otherwise resolve the configured script while the script
    /// itself immediately fails to resolve `node`.
    pub fn apply(&self, command: &mut Command) {
        let program = command.get_program().to_os_string();
        if let Some(variables) = &self.variables {
            command.env_clear();
            command.envs(variables.iter().map(|(key, value)| (key, value)));
        }
        if let Some(path) = self.path_with_program_directory(&program) {
            command.env("PATH", path);
        }
    }

    fn path_with_program_directory(&self, program: &OsStr) -> Option<OsString> {
        let directory = std::path::Path::new(program).parent()?;
        if directory.as_os_str().is_empty() {
            return None;
        }
        let mut entries = vec![directory.to_path_buf()];
        if let Some(path) = self.path() {
            entries.extend(env::split_paths(&path).filter(|entry| entry != directory));
        }
        env::join_paths(entries).ok()
    }

    #[cfg(test)]
    pub(crate) fn captured_for_tests(shell: PathBuf, variables: Vec<(OsString, OsString)>) -> Self {
        Self {
            source: EnvironmentSource::LoginShell { shell },
            variables: Some(variables),
        }
    }
}

impl Default for HostEnvironment {
    fn default() -> Self {
        Self::from_process()
    }
}

impl fmt::Debug for HostEnvironment {
    /// Omits variable names and values: login shell captures regularly
    /// contain credentials exported from shell profiles, and those must never
    /// reach logs or diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostEnvironment")
            .field("source", &self.source)
            .field("variables", &self.variables.as_ref().map(Vec::len))
            .finish()
    }
}

#[cfg(unix)]
struct ShellCandidate {
    program: PathBuf,
    login_arguments: &'static [&'static str],
}

#[cfg(unix)]
fn shell_candidates(shell: Option<&OsStr>) -> Vec<ShellCandidate> {
    let mut candidates = Vec::new();
    if let Some(shell) = shell {
        let program = PathBuf::from(shell);
        let known = program
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| matches!(name, "zsh" | "bash" | "fish"));
        if known {
            // Interactive plus login: version-manager PATH edits commonly
            // live in rc files (`.zshrc`) that plain login shells never read.
            candidates.push(ShellCandidate {
                program,
                login_arguments: &["-i", "-l", "-c"],
            });
        }
    }
    let fallback = PathBuf::from("/bin/sh");
    if candidates
        .iter()
        .all(|candidate| candidate.program != fallback)
    {
        // `sh -l` reads /etc/profile, which runs path_helper on macOS. This
        // covers unknown or exotic user shells with the standard locations.
        candidates.push(ShellCandidate {
            program: fallback,
            login_arguments: &["-l", "-c"],
        });
    }
    candidates
}

#[cfg(unix)]
fn capture_login_environment(
    candidate: &ShellCandidate,
    timeout: Duration,
) -> Option<Vec<(OsString, OsString)>> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::time::Instant;

    let marker = uuid::Uuid::new_v4().simple().to_string();
    // Interactive shells may print banners or rc-file output; the markers
    // around the NUL-delimited dump keep parsing immune to that noise.
    let script = format!("printf '%s' '{marker}'; env -0; printf '%s' '{marker}'");
    let mut command = Command::new(&candidate.program);
    command
        .args(candidate.login_arguments)
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Shell startup files may leave descendants holding stdout open. Put the
    // shell in its own process group so the timeout can close the whole pipe
    // tree instead of waiting for an orphaned descendant.
    command.process_group(0);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (output_sender, output_receiver) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        let _ = output_sender.send(output);
    });

    let deadline = Instant::now() + timeout;
    let succeeded = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() >= deadline => {
                kill_process_group(&mut child);
                let _ = child.wait();
                break false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                kill_process_group(&mut child);
                let _ = child.wait();
                break false;
            }
        }
    };
    if !succeeded {
        // A shell startup file can leave descendants holding stdout open even
        // after the shell process group is terminated. Do not let a blocked
        // reader turn the bounded shell timeout into an unbounded startup
        // delay; the detached reader exits when those descendants close the
        // inherited pipe.
        drop(reader);
        return None;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let Ok(output) = output_receiver.recv_timeout(remaining) else {
        // The shell may have exited while an rc-file descendant kept the
        // capture pipe open. Terminate that process group and keep the
        // same total timeout instead of blocking forever in join().
        kill_process_group(&mut child);
        drop(reader);
        return None;
    };
    let _ = reader.join();
    parse_marked_environment(&output, marker.as_bytes())
}

#[cfg(unix)]
fn kill_process_group(child: &mut std::process::Child) {
    let process_group = format!("-{}", child.id());
    // The process group is created by process_group(0) immediately before
    // spawn, so a negative PID targets only this shell capture tree.
    let _ = Command::new("/bin/kill")
        .args(["-KILL", &process_group])
        .status();
    let _ = child.kill();
}

#[cfg(unix)]
fn parse_marked_environment(output: &[u8], marker: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    use std::os::unix::ffi::OsStringExt;

    let start = find_subsequence(output, marker)? + marker.len();
    let end = rfind_subsequence(output, marker)?;
    if end < start {
        return None;
    }
    let mut variables = Vec::new();
    for entry in output[start..end].split(|byte| *byte == 0) {
        let Some(equals) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = &entry[..equals];
        if key.is_empty()
            || EXCLUDED_VARIABLES
                .iter()
                .any(|excluded| excluded.as_bytes() == key)
        {
            continue;
        }
        variables.push((
            OsString::from_vec(key.to_vec()),
            OsString::from_vec(entry[equals + 1..].to_vec()),
        ));
    }
    // A capture without PATH cannot help locate or run Pi.
    let has_path = variables
        .iter()
        .any(|(key, value)| key == "PATH" && !value.is_empty());
    has_path.then_some(variables)
}

#[cfg(unix)]
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(unix)]
fn rfind_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

#[cfg(all(test, unix))]
mod tests {
    use std::env;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::{
        EnvironmentSource, HostEnvironment, ShellCandidate, capture_login_environment,
        parse_marked_environment, shell_candidates,
    };

    fn write_shell(directory: &Path, name: &str, contents: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, contents).expect("write fake shell");
        let mut permissions = fs::metadata(&path)
            .expect("fake shell metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("make fake shell executable");
        path
    }

    #[test]
    fn parses_environment_between_markers_and_drops_shell_bookkeeping() {
        let marker = b"PIXMARKER";
        let mut output = Vec::new();
        output.extend_from_slice(b"rc-file banner noise\n");
        output.extend_from_slice(marker);
        output.extend_from_slice(b"PATH=/custom/bin\0SHLVL=2\0PWD=/tmp\0API_TOKEN=secret\0");
        output.extend_from_slice(marker);
        output.extend_from_slice(b"trailing prompt fragment");

        let variables = parse_marked_environment(&output, marker).expect("parsed environment");

        assert!(
            variables
                .iter()
                .any(|(key, value)| key == "PATH" && value == "/custom/bin")
        );
        assert!(variables.iter().any(|(key, _)| key == "API_TOKEN"));
        assert!(
            variables
                .iter()
                .all(|(key, _)| key != "SHLVL" && key != "PWD")
        );
    }

    #[test]
    fn rejects_captures_without_a_usable_path() {
        let marker = b"PIXMARKER";
        let mut output = Vec::new();
        output.extend_from_slice(marker);
        output.extend_from_slice(b"HOME=/Users/dev\0");
        output.extend_from_slice(marker);

        assert!(parse_marked_environment(&output, marker).is_none());
    }

    #[test]
    fn rejects_output_with_a_single_marker() {
        let marker = b"PIXMARKER";
        let mut output = Vec::new();
        output.extend_from_slice(marker);
        output.extend_from_slice(b"PATH=/custom/bin\0");

        assert!(parse_marked_environment(&output, marker).is_none());
    }

    #[test]
    fn captures_the_login_shell_path_despite_rc_noise() {
        let directory = tempdir().expect("temporary shell directory");
        let shell = write_shell(
            directory.path(),
            "zsh",
            concat!(
                "#!/bin/sh\n",
                "printf 'banner from rc files\\n'\n",
                "export PATH=\"/pix-custom/bin:$PATH\"\n",
                "exec /bin/sh -c \"$4\"\n",
            ),
        );

        let environment =
            HostEnvironment::resolve_with(Some(shell.as_os_str()), Duration::from_secs(10));

        assert_eq!(
            environment.source(),
            &EnvironmentSource::LoginShell {
                shell: shell.clone()
            }
        );
        let path = environment.path().expect("captured PATH");
        let first = env::split_paths(&path).next().expect("first PATH entry");
        assert_eq!(first, PathBuf::from("/pix-custom/bin"));
    }

    #[test]
    fn capture_gives_up_on_shells_that_fail_or_hang() {
        let directory = tempdir().expect("temporary shell directory");

        let failing = write_shell(directory.path(), "bash", "#!/bin/sh\nexit 3\n");
        let failing_candidate = ShellCandidate {
            program: failing,
            login_arguments: &["-i", "-l", "-c"],
        };
        assert!(capture_login_environment(&failing_candidate, Duration::from_secs(10)).is_none());

        let hanging = write_shell(directory.path(), "zsh", "#!/bin/sh\nsleep 30\n");
        let hanging_candidate = ShellCandidate {
            program: hanging,
            login_arguments: &["-i", "-l", "-c"],
        };
        let started = Instant::now();
        assert!(
            capture_login_environment(&hanging_candidate, Duration::from_millis(200)).is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(5));

        let inherited_pipe =
            write_shell(directory.path(), "fish", "#!/bin/sh\nsleep 2 &\nexit 0\n");
        let inherited_pipe_candidate = ShellCandidate {
            program: inherited_pipe,
            login_arguments: &["-i", "-l", "-c"],
        };
        let started = Instant::now();
        assert!(
            capture_login_environment(&inherited_pipe_candidate, Duration::from_millis(200))
                .is_none()
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "stdout descendants must not outlive the capture timeout"
        );
    }

    #[test]
    fn unknown_shells_are_not_executed_and_fall_back_to_bin_sh() {
        let unknown = shell_candidates(Some(OsStr::new("/opt/nu/bin/nu")));
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].program, PathBuf::from("/bin/sh"));

        let known = shell_candidates(Some(OsStr::new("/bin/zsh")));
        assert_eq!(known.len(), 2);
        assert_eq!(known[0].program, PathBuf::from("/bin/zsh"));
        assert_eq!(known[1].program, PathBuf::from("/bin/sh"));
    }

    #[test]
    fn resolve_for_prefers_the_process_environment_when_it_finds_the_executable() {
        // `sh` is on every Unix PATH, so no login shell needs to run.
        let environment = HostEnvironment::resolve_for("sh");
        assert_eq!(environment.source(), &EnvironmentSource::Process);
    }

    #[test]
    fn path_lookup_keeps_shim_symlink_instead_of_dispatcher() {
        let directory = tempdir().expect("temporary PATH directory");
        let dispatcher = directory.path().join("mise");
        let shim = directory.path().join("pi");
        fs::write(&dispatcher, b"#!/bin/sh\nexit 0\n").expect("write dispatcher");
        symlink(&dispatcher, &shim).expect("create shim");

        let environment = HostEnvironment::captured_for_tests(
            PathBuf::from("/bin/zsh"),
            vec![(
                OsString::from("PATH"),
                directory.path().as_os_str().to_owned(),
            )],
        );

        assert_eq!(
            environment.find_executable("pi").as_deref(),
            Some(shim.as_path())
        );
    }

    #[test]
    fn apply_replaces_the_child_environment_with_the_capture() {
        let environment = HostEnvironment::captured_for_tests(
            PathBuf::from("/bin/zsh"),
            vec![
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                (
                    OsString::from("PIX_TEST_SENTINEL"),
                    OsString::from("resolved"),
                ),
            ],
        );

        let output = environment
            .command("/usr/bin/env")
            .output()
            .expect("run env in the captured environment");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(stdout.contains("PIX_TEST_SENTINEL=resolved"));
        // The process environment always carries HOME; the capture does not,
        // which proves the child environment was replaced, not extended.
        assert!(!stdout.contains("HOME="));
    }

    #[test]
    fn debug_output_redacts_captured_variables() {
        let environment = HostEnvironment::captured_for_tests(
            PathBuf::from("/bin/zsh"),
            vec![(OsString::from("SECRET_TOKEN"), OsString::from("hunter2"))],
        );

        let debug = format!("{environment:?}");

        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("SECRET_TOKEN"));
    }
}
