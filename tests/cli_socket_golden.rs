#![cfg(feature = "cmd-socket")]
// Compiled only against a cmd-socket build (its schema + command surface).
//! The `socket` family's golden tests: doc 04 §4 routes `connections`/`ports`
//! here, and this build proves the per-socket half that DOES have a route —
//! create/info/name/close over `SocketService` — plus the honest answer for
//! the half that does not (the all-sockets enumeration is the socket batch's
//! gap, and the CLI names it instead of inventing it).

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
fn socket_create_returns_the_handle_the_service_minted() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "socket", "create", "stream"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"socketId\"") && out.contains('7'), "{out}");
}

#[test]
fn socket_info_and_name_are_contract_shaped() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (info, code) = net(p, &["--json", "socket", "info", "7"]);
    assert_eq!(code, 0, "{info}");
    assert!(info.contains("\"localPort\"") && info.contains("4242"), "{info}");

    let (name, code) = net(p, &["--json", "socket", "name", "7"]);
    assert_eq!(code, 0, "{name}");
    assert!(name.contains("\"port\""), "{name}");
}

#[test]
fn socket_close_is_a_void_mutator() {
    let mock = MockService::start(true).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "socket", "close", "7"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "{}", "{out}");
}

#[test]
fn the_net_family_reaches_the_same_socket_route() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (family, family_code) = net(p, &["--json", "socket", "info", "7"]);
    let (alias, alias_code) = net(p, &["--json", "net", "socket", "info", "7"]);
    assert_eq!(family_code, 0, "{family}");
    assert_eq!(alias_code, 0, "{alias}");
    assert_eq!(alias, family, "`net socket …` is the same call, not a second implementation");
}

/// The gap this family reports rather than fills: no all-sockets enumeration.
#[test]
fn socket_list_names_the_gap_instead_of_faking_a_list() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "socket", "list"]);
    assert_eq!(code, 2, "the gap is a usage answer that names the owning batch: {code}");
    assert!(out.contains("socket") && out.contains("enumeration"), "{out}");
}
