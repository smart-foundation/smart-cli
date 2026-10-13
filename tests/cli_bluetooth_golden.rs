#![cfg(feature = "cmd-bt")]
// Compiled only against a cmd-bt build (its schema + command surface).
//! The `bluetooth` family's golden tests: doc 04 §4 routes status/on/off/name/
//! devices to `BluetoothService`, and the subscription-shaped verbs (scan,
//! pair, connect) are named rather than half-routed.

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
fn bluetooth_status_is_contract_shaped() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(mock.path().to_str().unwrap(), &["--json", "bluetooth", "status"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("on"), "the radio state the mock reports: {out}");
}

#[test]
fn bluetooth_name_get_and_devices() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (name, err, code) = net(p, &["--json", "bluetooth", "name"]);
    assert_eq!(code, 0, "{name} {err}");
    assert!(name.contains("mock-host"), "{name}");

    let (devices, err, code) = net(p, &["--json", "bluetooth", "devices"]);
    assert_eq!(code, 0, "{devices} {err}");
    assert!(devices.contains("Mock Buds"), "{devices}");
}

#[test]
fn bluetooth_control_verbs_respect_the_capability_class() {
    let read_only = MockService::start(false).unwrap();
    let (out, err, code) = net(read_only.path().to_str().unwrap(), &["--json", "bluetooth", "off"]);
    assert_eq!(code, 4, "the read-only class denies the control verb: {code} {out} {err}");

    let control = MockService::start(true).unwrap();
    let (out, err, code) = net(control.path().to_str().unwrap(), &["--json", "bluetooth", "off"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out.trim(), "{}", "the void mutator answers {{}}: {out}");
}

/// The verbs whose route is a subscription capability are NAMED as such, not
/// silently accepted and not faked (doc 04 §4's gap ledger).
#[test]
fn bluetooth_subscription_verbs_are_named_not_half_routed() {
    let mock = MockService::start(false).unwrap();
    let (out, _err, code) = net(mock.path().to_str().unwrap(), &["--json", "bluetooth", "scan"]);
    assert_eq!(code, 2, "the honest answer for a verb with no direct route: {code}");
    assert!(out.contains("subscription"), "{out}");
}
