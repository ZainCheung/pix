#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

fn fake_pi(home: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = home.join("bin");
    fs::create_dir_all(&bin).expect("fake Pi bin");
    let executable = bin.join("pi");
    fs::write(
        &executable,
        "#!/bin/sh\ncase \"$1\" in\n  --version) printf '0.84.2\\n' ;;\n  --help) printf '%s\\n' '--mode <mode> --approve --session <path|id> --session-id <id>' ;;\n  *) exit 0 ;;\nesac\n",
    )
    .expect("write fake Pi");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make Pi executable");
    executable
}

fn run_pix(home: &Path, args: &[&str]) -> Output {
    let bin = home.join("bin");
    Command::new(env!("CARGO_BIN_EXE_pix"))
        .env("HOME", home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .args(args)
        .output()
        .expect("run pix")
}

fn json_output(output: &Output) -> Value {
    let bytes = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    assert!(
        !bytes.is_empty(),
        "Pix emitted no JSON; status={:?}, stdout={:?}, stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(bytes).expect("Pix JSON output")
}

#[test]
fn bridge_install_status_and_uninstall_are_integrity_checked() {
    let home = tempdir().expect("home");
    let _pi = fake_pi(home.path());
    let install = run_pix(
        home.path(),
        &["--output", "json", "pi", "bridge", "install"],
    );
    assert!(
        install.status.success(),
        "install stderr: {:?}",
        install.stderr
    );
    let install_json = json_output(&install);
    assert_eq!(install_json["ok"], true);
    assert_eq!(install_json["data"]["state"], "installed");
    assert_eq!(install_json["data"]["changed"], true);

    let extension = home.path().join(".pi/agent/extensions/pix-bridge/index.ts");
    assert!(extension.is_file());
    let extension_source = fs::read_to_string(&extension).expect("read installed extension");
    assert!(extension_source.contains("ownership handshake"));
    assert!(extension_source.contains("pendingPersistenceClaim"));
    assert!(extension_source.contains("existsSync(payload.sessionFile)"));
    assert!(extension_source.contains("const PIX_RUNNING_STATUS = \"Pix running\";"));
    assert!(extension_source.contains("function clearPixStatus(ctx)"));
    assert!(extension_source.contains("ctx.ui.setStatus(PIX_BRIDGE_STATUS_KEY, undefined);"));
    assert!(!extension_source.contains("setStatus(\"pix-bridge\", \"standalone\")"));
    assert!(!extension_source.contains("setStatus(\"pix-bridge\", \"attached\")"));
    assert!(!extension_source.contains("setStatus(\"pix-bridge\", \"reconnecting\")"));
    let grant = extension_source
        .find("finish({ kind: \"attached\", response });")
        .expect("attached result handler");
    let grant_tail = &extension_source[grant..];
    let coalesced_marker = "// A Host writer may coalesce register_result with the first";
    let marker = grant_tail
        .find(coalesced_marker)
        .expect("coalesced frame guard");
    let marker_tail = &grant_tail[marker..];
    let continue_pos = marker_tail
        .find("continue;")
        .expect("coalesced frame continue");
    let conflict_branch_pos = marker_tail
        .find("} else if (response.error === \"conflict\")")
        .expect("conflict branch after attached result");
    assert!(
        marker_tail.contains("snapshot request. Keep draining this same data chunk")
            && marker_tail.contains("frames that follow the grant are not stranded")
            && continue_pos < conflict_branch_pos,
        "coalesced snapshot request must not be stranded after attachment"
    );

    let repeat = run_pix(
        home.path(),
        &["--output", "json", "pi", "bridge", "install"],
    );
    assert!(
        repeat.status.success(),
        "repeat stderr: {:?}",
        repeat.stderr
    );
    assert_eq!(json_output(&repeat)["data"]["changed"], false);

    let status = run_pix(home.path(), &["--output", "json", "pi", "bridge", "status"]);
    assert!(
        status.status.success(),
        "status stderr: {:?}",
        status.stderr
    );
    let status_json = json_output(&status);
    assert_eq!(status_json["data"]["extension"]["state"], "installed");
    assert_eq!(status_json["data"]["pi"]["supported"], true);

    fs::OpenOptions::new()
        .append(true)
        .open(&extension)
        .expect("open extension")
        .write_all(b"\n// modified\n")
        .expect("modify extension");
    let uninstall = run_pix(
        home.path(),
        &["--output", "json", "pi", "bridge", "uninstall"],
    );
    assert!(!uninstall.status.success());
    let error = json_output(&uninstall);
    assert_eq!(error["ok"], false);
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("modified or unmanaged"))
    );
}
