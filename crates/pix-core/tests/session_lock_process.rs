use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use pix_core::{SessionId, SessionLease};
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn blocks_a_second_pix_process() {
    const HELPER_ENV: &str = "PIX_TEST_LOCK_HELPER";
    let directory = tempdir().expect("temporary lock directory");
    let session_id = SessionId::new();
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--ignored",
            "--exact",
            "session_lock_subprocess_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, directory.path())
        .env("PIX_TEST_SESSION_ID", session_id.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start lock holder process");

    let mut output = BufReader::new(child.stdout.take().expect("helper stdout"));
    let mut ready = String::new();
    loop {
        let count = output.read_line(&mut ready).expect("read helper readiness");
        assert!(count > 0, "helper exited before acquiring the session lock");
        if ready.contains("READY") {
            break;
        }
        ready.clear();
    }

    let Err(error) = SessionLease::acquire(directory.path(), session_id) else {
        panic!("second process acquired the same session");
    };
    assert!(error.to_string().contains("already owned"));

    drop(child.stdin.take());
    assert!(child.wait().expect("wait for helper").success());
}

#[test]
#[ignore = "subprocess helper invoked by blocks_a_second_pix_process"]
fn session_lock_subprocess_helper() {
    let Some(directory) = std::env::var_os("PIX_TEST_LOCK_HELPER") else {
        return;
    };
    let raw_id = std::env::var("PIX_TEST_SESSION_ID").expect("session ID environment");
    let session_id = SessionId::from_uuid(Uuid::parse_str(&raw_id).expect("valid session ID"));
    let _lease = SessionLease::acquire(std::path::Path::new(&directory), session_id)
        .expect("helper session lease");
    println!("READY");
    std::io::stdout().flush().expect("flush readiness");
    let mut buffer = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buffer)
        .expect("wait for parent EOF");
}
