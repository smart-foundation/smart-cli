#![cfg(feature = "cmd-net")]
// The `net`-family CLI golden tests compile only against a cmd-net build.
//! The honesty law (grammar law #8): help / registry read the service's
//! availability answer (`cli.capnp listUnavailable`), so a command the service
//! reports not-runnable is never advertised as available — its reason is
//! nameable through `net registry list-unavailable`.

use std::process::Command;

mod support;
use support::mock::MockService;

fn net(mock_path: &str, args: &[&str]) -> (String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
    cmd.arg("--connect").arg(mock_path);
    cmd.args(args);
    let output = cmd.output().expect("spawn net");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn registry_list_unavailable_surfaces_the_reason() {
    let mock = MockService::start_with(false, vec!["net wifi scan".to_string()]).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["net", "registry", "list-unavailable"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("net wifi scan"), "the hidden command is named: {out}");
    assert!(out.contains("serviceUnavailable"), "its reason class is printed: {out}");
}

#[test]
fn registry_list_unavailable_json_shape() {
    let mock = MockService::start_with(false, vec!["net wifi scan".to_string()]).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "net", "registry", "list-unavailable"]);
    assert_eq!(code, 0);
    assert!(out.contains("net wifi scan"), "{out}");
}

#[test]
fn help_surfaces_what_cannot_run_without_advertising_it_as_runnable() {
    let mock = MockService::start_with(false, vec!["net wifi scan".to_string()]).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("not available on this host"), "help surfaces the gap: {out}");
    assert!(out.contains("net wifi scan"), "{out}");
}
