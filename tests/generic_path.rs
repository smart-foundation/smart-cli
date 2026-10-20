#![cfg(feature = "cmd-net")]
// The generic path (paradigm agnosticism): the CLI calls a schema it has
// NEVER COMPILED.
//!
//! The mock service serves a FIXTURE schema (`tests/fixtures/fixture.capnp`)
//! that the CLI's library never compiles — no generated binding exists on the
// client side. The service publishes the schema's node tree in the registry's
// metadata, and the CLI addresses the call from the contract's own
//! introspection: the interface's type id, the method's id, the params and the
//! results all come from the node tree.
//
//! A 2046 service — one that ships a contract this CLI has never seen — must
//! therefore work with NO CLI change at all.

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

/// The CLI drives a schema it never compiled: the registry names the method,
/// the service publishes the schema, and the CLI answers with the contract's
/// own field names.
#[test]
fn the_cli_calls_a_schema_it_never_compiled() {
    let mock = MockService::start(true).unwrap();
    let (out, err, code) = net(
        mock.path().to_str().unwrap(),
        &["fixture", "echo", "host=example.com", "count=2"],
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("example.com"),
        "the answer carries the contract's own field names: {out}"
    );
    assert!(out.contains("pong"), "{out}");
    assert!(out.contains("count"), "{out}");
}

/// The JSON form is the contract's shape: the field names are the schema's.
#[test]
fn the_generic_path_answers_in_contract_shaped_json() {
    let mock = MockService::start(true).unwrap();
    let (out, err, code) = net(
        mock.path().to_str().unwrap(),
        &["--json", "fixture", "echo", "host=json.example"],
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("\"pong\""), "{out}");
    assert!(out.contains("json.example"), "{out}");
}

/// The CLI says so when the schema carries no shape it can use — never a
/// fabricated success.
#[test]
fn the_generic_path_refuses_an_unknown_field_honestly() {
    let mock = MockService::start(true).unwrap();
    let (out, err, code) = net(
        mock.path().to_str().unwrap(),
        &["fixture", "echo", "nosuchfield=1"],
    );
    assert_eq!(code, 2, "{out}{err}");
    assert!(
        err.contains("nosuchfield") || out.contains("nosuchfield"),
        "the refusal names the field: {out}{err}"
    );
}

/// The CLI has no binding for the fixture schema at all: the proof is that
/// the generic path is the ONLY way in.
#[test]
fn the_cli_library_holds_no_binding_for_the_fixture_schema() {
    let source = include_str!("../src/cmd/net.rs");
    assert!(
        !source.contains("fixture_capnp"),
        "the CLI must not name the fixture schema's bindings"
    );
    let library = include_str!("../src/lib.rs");
    assert!(
        !library.contains("fixture"),
        "the CLI library must not compile the fixture schema"
    );
}

