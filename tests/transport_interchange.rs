//! The **transport interchange**: the same command over unix and over tcp
//! produces BYTE-IDENTICAL output.
//!
//! The contract's transports are carriers, not dialects: what crosses the
//! wire is the contract's own call model either way. A client that renders
//! the carrier's name in its output would be lying about the contract — the
//! transport is a fact of the connection, never of the answer.
//!
//! The mock serves BOTH carriers at once (the same service, the same
//! contract), so this compares one implementation with itself over two
//! carriers.

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

/// The commands whose output must not depend on the carrier.
const COMMANDS: &[&[&str]] = &[
    &["--json", "status"],
    &["--json", "interfaces"],
    &["--json", "wifi", "scan"],
    &["status"],
    &["interfaces"],
];

#[test]
fn the_same_command_over_unix_and_over_tcp_is_byte_identical() {
    let mock = MockService::start(true).unwrap();
    let unix_path = mock.path().to_str().unwrap().to_string();
    let tcp_address = mock.tcp_address().to_string();
    for command in COMMANDS {
        let mut unix_args: Vec<&str> = vec!["--connect", unix_path.as_str()];
        unix_args.extend_from_slice(command);
        let mut tcp_args: Vec<&str> = vec!["--addr", tcp_address.as_str()];
        tcp_args.extend_from_slice(command);

        let (unix_out, unix_err, unix_code) = net(&unix_args);
        let (tcp_out, tcp_err, tcp_code) = net(&tcp_args);
        assert_eq!(unix_code, 0, "{command:?}: {unix_out} {unix_err}");
        assert_eq!(tcp_code, 0, "{command:?}: {tcp_out} {tcp_err}");
        assert_eq!(
            unix_out, tcp_out,
            "the same command over unix and over tcp must be byte-identical: {command:?}"
        );
        assert!(!unix_out.is_empty(), "{command:?} produced nothing");
    }
}

#[test]
fn the_carrier_is_in_the_audit_trail_and_nowhere_else() {
    let mock = MockService::start(true).unwrap();
    let unix_path = mock.path().to_str().unwrap().to_string();
    let tcp_address = mock.tcp_address().to_string();
    let (unix_out, unix_err, _) = net(&["--connect", unix_path.as_str(), "--verbose", "--json", "status"]);
    let (tcp_out, tcp_err, _) = net(&["--addr", tcp_address.as_str(), "--verbose", "--json", "status"]);
    // The ANSWER is identical...
    assert_eq!(unix_out, tcp_out);
    // ...and the carrier is named in the audit trail (stderr), never in the
    // answer.
    assert!(unix_err.contains("unix:"), "{unix_err}");
    assert!(tcp_err.contains("tcp:"), "{tcp_err}");
    assert!(!unix_out.contains("unix:"), "{unix_out}");
    assert!(!tcp_out.contains("tcp:"), "{tcp_out}");
}
