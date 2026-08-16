use std::process::Command;

#[test]
fn context_guard_regression() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("pi-context-guard.test.mjs");
    let output = match Command::new("node").arg(&script).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping context guard test: node is not installed");
            return;
        }
        Err(error) => panic!("failed to run context guard test: {error}"),
    };

    assert!(
        output.status.success(),
        "context guard regression failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
