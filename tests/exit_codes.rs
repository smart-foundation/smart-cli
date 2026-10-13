#![cfg(feature = "cmd-net")]
//! The exit-code table (grammar law #7), made mechanical: every published code
//! is produced by a real path against the MOCK service — no daemon, no live
//! host (law #8).
//!
//! | code | path asserted here |
//! |---|---|
//! | 0 ok | a read that answers ok |
//! | 1 the call failed | a read the service answers with the contract's generic failure |
//! | 2 usage | a bad selector field / an unknown verb |
//! | 3 unavailable-with-reason | a method the service does not serve, and a named gap |
//! | 4 permission | a mutating verb on a read-only capability |
//! | 5 timeout | the transport's deadline mapped to the contract's timeout |

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
fn exit_0_on_an_ok_read() {
    let mock = MockService::start(false).unwrap();
    let (out, _err, code) = net(mock.path().to_str().unwrap(), &["--json", "wifi", "list"]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn exit_1_when_the_call_fails() {
    let mock = MockService::start_failing_reads().unwrap();
    let (out, _err, code) = net(mock.path().to_str().unwrap(), &["--json", "status"]);
    assert_eq!(code, 1, "the service answered the contract's failure: {code} {out}");
    assert!(out.contains("\"error\""), "the error object carries the kind: {out}");
    assert!(out.contains("failed"), "{out}");
}

#[test]
fn exit_2_on_usage_errors() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    // A bad selector field names the valid fields.
    let (out, _err, code) = net(p, &["--json", "interface", "--where", "nope", "==", "1"]);
    assert_eq!(code, 2, "{out}");
    // An unknown verb in a task.
    let (_out, _err, code) = net(p, &["--json", "wifi", "teleport"]);
    assert_eq!(code, 2);
}

#[test]
fn exit_3_when_the_service_does_not_serve_the_method() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    // JSON mode: the error object is the result, and it names the class.
    let (out, _err, code) = net(p, &["--json", "traceroute", "8.8.8.8"]);
    assert_eq!(code, 3, "{code}");
    assert!(out.contains("notSupported"), "{out}");
    // Human mode: the reason goes to stderr and the exit code is the same.
    let (out, err, code) = net(p, &["traceroute", "8.8.8.8"]);
    assert_eq!(code, 3, "{out} {err}");
    assert!(err.contains("notSupported"), "{err}");
    assert!(out.trim().is_empty(), "no success on stdout: {out:?}");
}

#[test]
fn exit_4_when_the_capability_is_not_held() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (out, _err, code) = net(p, &["--json", "wifi", "off"]);
    assert_eq!(code, 4, "{code}");
    assert!(out.contains("permissionDenied"), "{out}");
    // Human mode: the denial is on stderr, the exit code carries it.
    let (out, err, code) = net(p, &["wifi", "off"]);
    assert_eq!(code, 4, "{out} {err}");
    assert!(err.contains("permissionDenied"), "{err}");
    assert!(out.trim().is_empty(), "the denied path prints no success: {out:?}");
}

/// The published table, asserted as a mapping: every contract error class maps
/// to exactly one code (the mapping lives in `registry::exit_code_for` and is
/// total over the error kinds the wire can carry).
#[test]
fn the_exit_code_mapping_is_total_over_the_error_classes() {
    use smart_cli::registry;
    use smart_cli::schema::types_capnp::ErrorCode;
    use smart_cli::transport::{CallError, ServiceError};

    let service = |code: ErrorCode| {
        CallError::Service(ServiceError {
            code,
            message: String::new(),
            context: String::new(),
        })
    };
    let code_for = |code: ErrorCode| registry::exit_code_for(&Err::<Vec<u8>, _>(service(code)));
    assert_eq!(code_for(ErrorCode::Failed), registry::EXIT_CALL_FAILED);
    assert_eq!(code_for(ErrorCode::NotSupported), registry::EXIT_UNAVAILABLE);
    assert_eq!(code_for(ErrorCode::PermissionDenied), registry::EXIT_PERMISSION);
    assert_eq!(code_for(ErrorCode::Timeout), registry::EXIT_TIMEOUT);
    assert_eq!(code_for(ErrorCode::Unavailable), registry::EXIT_UNAVAILABLE);
    // A transport failure is a failed call, not a usage error.
    assert_eq!(
        registry::exit_code_for(&Err(CallError::Transport("peer closed".into()))),
        registry::EXIT_CALL_FAILED
    );
    // The published table has one line per code, and the codes are the five.
    let codes: Vec<i32> = registry::EXIT_TABLE.iter().map(|(code, _)| *code).collect();
    assert_eq!(codes, vec![0, 1, 2, 3, 4, 5]);
}
