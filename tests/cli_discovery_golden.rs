#![cfg(feature = "cmd-discovery")]
// Compiled only against a cmd-discovery build (its schema + command surface).
//! The `discovery` family's golden tests: the smart-protocol SERVICE registry
//! (doc 04 §4) — lookup and resolve are ungated, so a caller can learn what
//! exists before it holds authority.

use std::process::Command;

mod support;
use support::mock::MockService;

fn net(mock_path: &str, args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
    cmd.arg("--connect").arg(mock_path);
    cmd.args(args);
    let output = cmd.output().expect("spawn net");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn discovery_peers_is_the_bounded_lookup() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(mock.path().to_str().unwrap(), &["--json", "discovery", "peers"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("mock-service"), "{out}");
}

#[test]
fn discovery_resolve_reports_presence() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(mock.path().to_str().unwrap(), &["--json", "discovery", "resolve", "mock-service"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("mock-service"), "{out}");
}
