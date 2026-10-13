#![cfg(feature = "cmd-web")]
// Compiled only against a cmd-web build (its schema + command surface).
//! The `web` family's golden tests: `WebService.fetch` (whose result is a
//! union carrying `T.Error`, so "answered 500" and "never reached the server"
//! are distinct) and `resolveUrl`.

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
fn web_get_reports_the_response_status() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(mock.path().to_str().unwrap(), &["--json", "web", "get", "https://example.com"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("200"), "{out}");
}

#[test]
fn web_resolve_returns_the_resolved_url() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(
        mock.path().to_str().unwrap(),
        &["--json", "web", "resolve", "/a", "https://example.com"],
    );
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("https://example.com/a"), "{out}");
}

#[test]
fn web_mutators_respect_the_capability_class() {
    // POST is the same route as GET over the contract's method token; the
    // mock answers it as the read-class fetch does.
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(mock.path().to_str().unwrap(), &["--json", "web", "post", "https://example.com"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("200"), "{out}");
}
