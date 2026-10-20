#![cfg(feature = "cmd-discovery")]
//! The **universal path**: the CLI's configured endpoint addresses the
//! DISCOVERY service; the CLI looks a domain up through `discovery.capnp`,
//! receives that domain's `ServiceEndpoint`, connects to it, negotiates the
//! ABI, and calls the method.
//!
//! Each hop is proved by the wire, in order:
//!
//! 1. the lookup — the discovery service answered for the name asked;
//! 2. the endpoint — the `ServiceEndpoint` it published (carrier + address);
//! 3. the ABI — the handshake on that endpoint, and the schema INTERSECTION
//!    both sides agreed on;
//! 4. the call — the method the service's own registry named, addressed with
//!    the ordinal the service published.
//!
//! The mock is CONTRACT-ONLY (it is implemented from the schema alone), so
//! this path proves the same thing against a real service: a client that
//! knows ONE endpoint can reach everything the service publishes.

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

#[test]
fn the_universal_path_resolves_the_endpoint_of_a_domain() {
    let mock = MockService::start(true).unwrap();
    let path = mock.path().to_str().unwrap();
    let (out, err, code) = net(&["--connect", path, "discovery", "endpoint", "mock-service"]);
    assert_eq!(code, 0, "{out} {err}");
    // The lookup answered for the name asked, and published the endpoint.
    assert!(out.contains("mock-service"), "{out}");
    assert!(out.contains("unix:"), "{out}");
    // The CALL MODEL the service published, and the carrier it answers over.
    assert!(out.contains("call model: capnp"), "{out}");
    assert!(out.contains("carrier: unix"), "{out}");
}

#[test]
fn the_universal_path_negotiates_the_abi_and_calls_the_method() {
    let mock = MockService::start(true).unwrap();
    let path = mock.path().to_str().unwrap();
    // The FIXTURE command: a schema this CLI never compiled. The service
    // publishes the schema's node tree and the method's ordinal, and the CLI
    // addresses the call from those.
    let (out, err, code) = net(&[
        "--connect",
        path,
        "--verbose",
        "discovery",
        "call",
        "mock-service",
        "net fixture echo",
        "host=spine",
    ]);
    assert_eq!(code, 0, "{out} {err}");
    // The call: the service's own answer.
    assert!(out.contains("pong:spine"), "{out}");
    // The hops, in order, on the audit trail.
    assert!(err.contains("discovery: `mock-service` published the endpoint"), "{err}");
    assert!(err.contains("call model: capnp"), "{err}");
    // The ABI: the intersection both sides agreed on, named.
    assert!(
        err.contains("negotiated schema intersection"),
        "{err}"
    );
    // The call's schema identity — the ordinal the SERVICE published.
    assert!(err.contains("fixture.capnp:Echo.echo @1"), "{err}");
}

#[test]
fn an_endpoint_whose_call_model_this_client_does_not_speak_is_unavailable_with_a_reason() {
    // A service that publishes a call model this client does not speak.
    let mock = MockService::start_with_call_model(true, "grpc").unwrap();
    let path = mock.path().to_str().unwrap();
    let (out, err, code) = net(&["--connect", path, "discovery", "endpoint", "mock-service"]);
    // Unavailable WITH A REASON (exit 3): never a crash, never a silent
    // wrong protocol.
    assert_eq!(code, 3, "{out} {err}");
    assert!(err.contains("grpc"), "{err}");
    assert!(err.contains("capnp"), "{err}");
}

#[test]
fn a_domain_the_service_does_not_publish_is_unavailable_with_a_reason() {
    let mock = MockService::start(true).unwrap();
    let path = mock.path().to_str().unwrap();
    let (out, err, code) = net(&["--connect", path, "discovery", "endpoint", "not-published"]);
    assert_eq!(code, 3, "{out} {err}");
    assert!(err.contains("not-published"), "{err}");
}
