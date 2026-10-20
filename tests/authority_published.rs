#![cfg(feature = "cmd-discovery")]
//! **Authority is published, not assumed.**
//!
//! Two proofs:
//!
//! 1. The CLI ASKS the service what it holds (capability.capnp) and renders
//!    the class the service's own answer names — the client does not decide
//!    what the caller may do.
//! 2. A mutating verb is refused BY THE SERVICE, with the service's own
//!    reason, and the CLI shows that reason — the client does not refuse on
//!    its own behalf, and never invents a reason of its own.

use std::process::Command;

mod support;
use support::mock::MockService;

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

#[test]
fn the_cli_renders_the_class_the_service_published() {
    // A service that holds only the read operation.
    let read_only = MockService::start(false).unwrap();
    let (out, err, code) = net(&[
        "--connect",
        read_only.path().to_str().unwrap(),
        "--json",
        "discovery",
        "capability",
    ]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("\"class\":\"ReadOnly\""), "{out}");
    assert!(out.contains("\"operations\":[\"read\"]"), "{out}");
    assert!(out.contains("\"issuer\":\"mock-service\""), "{out}");

    // A service that holds a mutating operation.
    let control = MockService::start(true).unwrap();
    let (out, err, code) = net(&[
        "--connect",
        control.path().to_str().unwrap(),
        "--json",
        "discovery",
        "capability",
    ]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("\"class\":\"Control\""), "{out}");
    assert!(out.contains("\"execute\""), "{out}");
}

#[test]
fn a_mutating_verb_is_refused_by_the_service_with_its_own_reason() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(&[
        "--connect",
        mock.path().to_str().unwrap(),
        "wifi",
        "on",
    ]);
    // Permission denied (exit 4): the SERVICE refused, not the client.
    assert_eq!(code, 4, "{out} {err}");
    // The service's own reason, carried on the wire.
    assert!(
        err.contains("permissionDenied"),
        "the service's own reason must reach the user: {err}"
    );
    assert!(err.contains("ReadOnly"), "{err}");
    // Nothing was rendered as if it had succeeded.
    assert!(!out.contains("enabled"), "{out}");
}

#[test]
fn the_human_view_names_the_class_too() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(&[
        "--connect",
        mock.path().to_str().unwrap(),
        "discovery",
        "capability",
    ]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("class ReadOnly"), "{out}");
    assert!(out.contains("read"), "{out}");
}
