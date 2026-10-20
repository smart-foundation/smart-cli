//! The **two implementations agree**: the mock (contract-only, written from
//! the schema alone) and the REAL daemon produce the same output SHAPE for
//! the same call.
//!
//! This is the "delete the library, write my own implementation, the CLI
//! works the same" proof. The two services share NOTHING but the contract:
//! the mock is a test double compiled from the schema in this tree, the
//! daemon is the real service compiled from the same schema in the engine
//! tree. The CLI is the same binary in both cases, and it cannot tell them
//! apart — so the ANSWER's shape (the contract's field names) must be the
//! same.
//!
//! The VALUES differ, and must: the mock answers with fixtures, the daemon
//! with the host's own truth. What must agree is the shape — the contract's
//! key names, in the contract's order.
//!
//! The daemon binary is found in `$SMART_SERVICE_BIN` (or on `PATH` as
//! `smart-service`). When neither is there the test says so and returns: the
//! CLI tree stays implementation-free, so it cannot ship a daemon.

use std::path::PathBuf;
use std::process::Command;

mod support;
use support::mock::MockService;
use support::shape::shape;

/// The daemon binary, if one was provided.
fn daemon_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SMART_SERVICE_BIN") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    let which = Command::new("sh").arg("-c").arg("command -v smart-service").output().ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&which.stdout).trim().to_string());
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Run the CLI against a unix socket.
fn net(args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
    cmd.args(args);
    let output = cmd.output().expect("spawn net");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

/// The calls whose shape the two implementations must agree on.
const CALLS: &[&[&str]] = &[
    &["--json", "interfaces"],
    &["--json", "status"],
    &["--json", "wifi", "scan"],
    &["--json", "routes"],
];

#[test]
fn the_mock_and_the_real_daemon_agree_on_the_output_shape() {
    let Some(daemon) = daemon_binary() else {
        eprintln!(
            "skipped: no daemon binary (set $SMART_SERVICE_BIN or put `smart-service` on PATH)"
        );
        return;
    };
    // The daemon: bound to a socket under the temp dir, for this test only.
    let socket = std::env::temp_dir().join(format!(
        "smart-cli-two-impls-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket);
    let daemon_process = Command::new(&daemon)
        .arg("--socket")
        .arg(&socket)
        // The daemon's own output goes nowhere: it must not hold the test's
        // stdout open, and it must not write to the test's terminal.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    // The guard stops the daemon and removes its socket whatever happens
    // (a panic included): no host state is left behind.
    let _daemon = Daemon {
        process: daemon_process,
        socket: socket.clone(),
    };
    // Wait for the socket to appear.
    let mut waited = 0;
    while !socket.exists() && waited < 100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
    }
    assert!(socket.exists(), "the daemon did not bind {}", socket.display());

    let mock = MockService::start(true).unwrap();
    let daemon_path = socket.to_str().unwrap().to_string();
    let mock_path = mock.path().to_str().unwrap().to_string();

    let mut compared = 0;
    for call in CALLS {
        let mut mock_args: Vec<&str> = vec!["--connect", mock_path.as_str()];
        mock_args.extend_from_slice(call);
        let mut daemon_args: Vec<&str> = vec!["--connect", daemon_path.as_str()];
        daemon_args.extend_from_slice(call);

        let (mock_out, mock_err, mock_code) = net(&mock_args);
        let (daemon_out, daemon_err, daemon_code) = net(&daemon_args);
        assert_eq!(mock_code, 0, "{call:?}: {mock_out} {mock_err}");
        assert_eq!(daemon_code, 0, "{call:?}: {daemon_out} {daemon_err}");

        let mock_shape = shape(&mock_out);
        let daemon_shape = shape(&daemon_out);
        assert_eq!(
            mock_shape, daemon_shape,
            "the two implementations disagree on the shape of {call:?}"
        );
        assert!(!mock_shape.is_empty(), "{call:?} produced no keys");
        compared += 1;
    }
    assert!(compared >= 2, "the parity proof compared too few calls");

    // The daemon is stopped and its socket removed by the guard above: no
    // host state is left.
}

/// The daemon this test started: stopped and cleaned up whatever happens.
struct Daemon {
    process: std::process::Child,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}
