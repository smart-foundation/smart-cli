#![cfg(feature = "schema-network")]
// Needs the network schema bindings (schema-network).
//! The L6 gate: the headless agent path (knowledge/05 §4, plan Part 4.4). A
//! caller drives the network surface with NO CLI in the loop, and a CI check
//! asserts the example's source links no command-parsing code path — the
//! `cmd` modules are provably absent (the example builds with them off:
//! `cargo run --example agent_client --no-default-features
//!  --features schema-network,std`).

use std::path::PathBuf;
use std::process::Command;

mod support;
use support::agent_scenario;
use support::mock::MockService;

#[test]
fn headless_client_drives_read_observe_denial() {
    let mock = MockService::start(false).unwrap(); // read-only class
    let path = mock.path().to_str().unwrap().to_string();
    agent_scenario::run_against(&path).expect("the headless scenario must pass");
}

#[test]
fn headless_example_source_links_no_command_surface() {
    let src = include_str!("../examples/agent_client.rs");
    let scenario = include_str!("../tests/support/agent_scenario.rs");
    // No command parsing, no rendering, no shell vocabulary in the headless
    // path: the only config is the transport + schema bindings + frame codec.
    for banned in ["mod cmd", "cmd::", "smart_cli::cmd", "clap", "render::", "render ."] {
        assert!(
            !src.contains(banned) && !scenario.contains(banned),
            "the headless path must not reference command/rendering code ({banned})"
        );
    }
    // The scenario uses ONLY the contract client core: the transport, the
    // contract's own generated bindings, and the contract's own call model.
    assert!(scenario.contains("smart_cli::transport"));
    assert!(scenario.contains("smart_cli::schema"));
    // The second wire vocabulary is GONE: the headless path speaks the
    // contract's own call model, so no frame codec may be linked.
    assert!(
        !scenario.contains("smart_cli::frame"),
        "the frame codec is deleted: the headless path speaks capnp RPC"
    );
}

/// The binary graph proof (knowledge/05 §4.6): the example `cargo test` just
/// built must contain NO symbol from the command surface — the AI-first path
/// links the client core (transport/frame/schema/json/selector) and nothing
/// that parses or renders a shell. This holds in EVERY feature config: even
/// when the crate is built with a command family on, the headless binary
/// contains none of it, because the agent path never references it.
#[test]
fn headless_binary_graph_excludes_the_command_modules() {
    let binary = example_binary().unwrap_or_else(|| {
        panic!(
            "the headless example binary was not found; build it with \
             `cargo build --example agent_client --no-default-features --features schema-network,std`"
        )
    });
    let output = Command::new("nm")
        .arg("-C")
        .arg(&binary)
        .output()
        .expect("nm the headless example binary");
    assert!(output.status.success(), "nm exited {:?}", output.status);
    let symbols = String::from_utf8_lossy(&output.stdout);

    let count = |needle: &str| symbols.matches(needle).count();
    assert_eq!(
        count("smart_cli::cmd"),
        0,
        "the headless binary links {} command-surface symbol(s): the agent path must not contain the CLI",
        count("smart_cli::cmd")
    );
    assert_eq!(
        count("smart_cli::render"),
        0,
        "the headless binary links rendering code ({}) — decoration is not the AI-first path",
        count("smart_cli::render")
    );
    // The client core IS there: the proof is an exclusion, not an empty binary.
    assert!(
        count("smart_cli::transport") > 0,
        "the headless binary must link the transport core"
    );
    assert!(
        count("smart_cli::rpc") > 0,
        "the headless binary must link the contract's own call model"
    );
    assert_eq!(
        count("smart_cli::frame"),
        0,
        "the headless binary must NOT link a second wire vocabulary"
    );
}

/// Locate the example binary `cargo test` built (debug profile; the
/// `CARGO_TARGET_DIR` override is honoured when set).
fn example_binary() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        candidates.push(PathBuf::from(dir));
    } else {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        candidates.push(manifest.join("target"));
    }
    for root in candidates {
        let path = root.join("debug").join("examples").join("agent_client");
        if path.is_file() {
            return Some(path);
        }
    }
    None
}
