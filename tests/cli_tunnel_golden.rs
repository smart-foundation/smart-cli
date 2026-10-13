#![cfg(feature = "cmd-tunnel")]
// Compiled only against a cmd-tunnel build (its schema + command surface).
//! The `tunnel` family's golden tests: the true tunnels (`tunnel.capnp`)
//! opened, inspected and closed. The VPN *profiles* route to the network
//! family (`net vpn`, doc 04 §4) and are tested there.

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
fn tunnel_open_returns_the_handle_the_service_minted() {
    let mock = MockService::start(true).unwrap();
    let (out, err, code) = net(mock.path().to_str().unwrap(), &["--json", "tunnel", "open", "peer-a"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("\"tunnelId\"") && out.contains("11"), "{out}");
}

#[test]
fn tunnel_info_is_contract_shaped() {
    let mock = MockService::start(true).unwrap();
    let (out, err, code) = net(mock.path().to_str().unwrap(), &["--json", "tunnel", "info", "11"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("mock-peer"), "{out}");
}

#[test]
fn tunnel_close_is_a_void_mutator_gated_by_the_class() {
    let read_only = MockService::start(false).unwrap();
    let (out, err, code) = net(read_only.path().to_str().unwrap(), &["--json", "tunnel", "close", "11"]);
    assert_eq!(code, 4, "{out} {err}");

    let control = MockService::start(true).unwrap();
    let (out, err, code) = net(control.path().to_str().unwrap(), &["--json", "tunnel", "close", "11"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out.trim(), "{}", "{out}");
}
