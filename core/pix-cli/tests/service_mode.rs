#![cfg(target_os = "linux")]

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

fn run_pix(config: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pix"))
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .expect("run pix CLI")
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn service_mode_ignores_stdin_eof_and_stops_over_control_socket() {
    let directory = tempdir().expect("temporary config directory");
    let config = directory.path().join("config.json");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let initialized = run_pix(&config, &["workspace", "add", workspace.to_str().unwrap()]);
    assert!(
        initialized.status.success(),
        "workspace add: {initialized:?}"
    );

    let child = Command::new(env!("CARGO_BIN_EXE_pix"))
        .arg("--config")
        .arg(&config)
        .args(["serve", "--service"])
        .env("PIX_DISABLE_KEYCHAIN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn service-mode pix");
    let mut guard = ChildGuard(Some(child));
    let status_path = directory.path().join("run/host-service.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !status_path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(status_path.exists(), "service status was not published");
    thread::sleep(Duration::from_millis(250));
    assert!(
        guard
            .0
            .as_mut()
            .expect("child")
            .try_wait()
            .expect("poll service")
            .is_none(),
        "service exited after stdin EOF"
    );

    let second = run_pix(&config, &["serve", "--service"]);
    assert!(
        !second.status.success(),
        "a second host service was allowed"
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already running"),
        "second service error did not identify the existing owner: {second:?}"
    );

    let stopped = run_pix(&config, &["service", "stop"]);
    assert!(stopped.status.success(), "service stop: {stopped:?}");
    let child = guard.0.take().expect("child");
    let output = child.wait_with_output().expect("wait for service");
    assert!(
        output.status.success(),
        "service stderr: {:?}",
        output.stderr
    );
    let mut stdout = String::new();
    output
        .stdout
        .as_slice()
        .read_to_string(&mut stdout)
        .expect("read service stdout");
    assert!(stdout.is_empty(), "service emitted raw stdout: {stdout:?}");
}
