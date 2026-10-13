#![cfg(feature = "cmd-net")]
//! Remote parity (grammar law #9): `@host net …` uses the IDENTICAL grammar;
//! only the transport and the capability source change. These tests drive the
//! built `net` binary against the mock through the `@host` spellings and assert
//! (a) the output shape is byte-identical to the local one and (b) `--verbose`
//! names the transport every time, because a remote capability claim is
//! weaker (the conformance model's DEGRADED marking).

use std::process::Command;

mod support;
use support::mock::MockService;

/// Run `net` with the given argv (no `--connect`: the transport comes from the
/// `@host` token) and return (stdout, stderr, exit code).
fn run(argv: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
    cmd.args(argv);
    let output = cmd.output().expect("spawn net");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn remote_host_spelling_produces_identical_output_to_local() {
    let mock = MockService::start(false).unwrap();
    let path = mock.path().to_str().unwrap().to_string();

    let (local, local_code) = {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
        cmd.arg("--connect").arg(&path).arg("--json").arg("status");
        let out = cmd.output().expect("spawn net");
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            out.status.code().unwrap_or(-1),
        )
    };
    let (remote, _stderr, remote_code) = run(&[&format!("@{path}"), "--json", "status"]);

    assert_eq!(local_code, 0, "{local}");
    assert_eq!(remote_code, local_code, "{remote}");
    assert_eq!(remote, local, "remote parity: the output shape is identical");
}

#[test]
fn remote_verbose_names_the_transport_and_the_degradation() {
    let mock = MockService::start(false).unwrap();
    let path = mock.path().to_str().unwrap().to_string();
    let (out, err, code) = run(&[&format!("@{path}"), "--json", "--verbose", "wifi", "list"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(err.contains(&format!("unix:{path}")), "the transport is declared: {err}");
    assert!(
        err.contains("remote session") && err.contains("DEGRADED"),
        "the remote capability claim is marked every time: {err}"
    );
    // The audit trail names the schema identity of the call made.
    assert!(err.contains("network.capnp"), "the schema identity is named: {err}");
}

#[test]
fn remote_tcp_hostport_selects_the_tcp_transport() {
    // A TCP target with no listener: the CLI must select TCP (and say so)
    // rather than silently fall back to a unix socket. In JSON mode the
    // unavailable answer is the result object on stdout.
    let (out, _err, code) = run(&["@127.0.0.1:47999", "--json", "status"]);
    assert_eq!(code, 3, "no service at the remote target -> unavailable-with-reason: {code}");
    assert!(out.contains("tcp:127.0.0.1:47999"), "the transport is TCP: {out}");
    assert!(out.contains("unavailable"), "{out}");
}

#[test]
fn local_session_is_not_marked_remote() {
    let mock = MockService::start(false).unwrap();
    let path = mock.path().to_str().unwrap().to_string();
    let (_out, err, code) = run(&["--connect", &path, "--json", "--verbose", "wifi", "list"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains(&format!("unix:{path}")), "{err}");
    assert!(!err.contains("remote session"), "a local session is not remote: {err}");
}

#[test]
fn the_at_token_and_the_connect_override_coexist() {
    // `--connect` wins over `@host` for the concrete target, and the session
    // is still honestly marked remote (the override is a local path).
    let mock = MockService::start(false).unwrap();
    let path = mock.path().to_str().unwrap().to_string();
    let (out, err, code) = run(&["@server1", "--connect", &path, "--json", "--verbose", "wifi", "list"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(err.contains(&format!("unix:{path}")), "the override target is used: {err}");
    assert!(err.contains("@server1"), "the @host token is still declared: {err}");
}
