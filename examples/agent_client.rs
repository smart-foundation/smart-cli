//! `agent_client` — the AI-first proof: a headless client that acquires the
//! network service's capability over a transport and drives the domain with
//! NO CLI in the loop — no argument parser, no rendering, no shell vocabulary.
//!
//! It is the L6 gate (knowledge/05 §4, plan Part 4.4). The same scenario the
//! `net` commands run, minus the shell: build capnp messages directly, call
//! schema methods, pull a subscription, and assert a capability denial. The
//! `cmd` command surface is absent from this build (`--no-default-features
//! --features schema-network,std`); `tests/headless_gate.rs` asserts the graph
//! excludes it.
//!
//! Run it against a mock (charter law #8 — no live service):
//!     cargo run --example agent_client --no-default-features \
//!         --features schema-network,std

use std::process::ExitCode;

// The FIXTURE schema's bindings (the same module the test support uses): a
// schema the CLI library never compiles.
mod support {
    #[allow(dead_code, unused_imports, clippy::all)]
    pub mod fixture {
        #[allow(dead_code, unused_imports, clippy::all)]
        pub mod fixture_capnp {
            include!(concat!(env!("OUT_DIR"), "/fixture_capnp.rs"));
        }
    }
}

// The one test double (speaks the contract's own call model) and the shared
// scenario.
include!("../tests/support/mock.rs");
include!("../tests/support/agent_scenario.rs");

fn main() -> ExitCode {
    // A read-only-class service: control verbs are permission-denied, which is
    // the honest denial the scenario asserts.
    let service = match MockService::start(false) {
        Ok(service) => service,
        Err(e) => {
            eprintln!("agent_client: could not start the mock service: {e}");
            return ExitCode::FAILURE;
        }
    };
    let path = match service.path().to_str() {
        Some(p) => p.to_string(),
        None => {
            eprintln!("agent_client: mock path is not utf-8");
            return ExitCode::FAILURE;
        }
    };

    match run_against(&path) {
        Ok(()) => {
            println!("agent_client: headless scenario passed (read, observe, denial) — no CLI code linked");
            ExitCode::SUCCESS
        }
        Err(step) => {
            eprintln!("agent_client: headless scenario FAILED at: {step}");
            ExitCode::FAILURE
        }
    }
}
