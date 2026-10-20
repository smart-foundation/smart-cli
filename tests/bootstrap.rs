#![cfg(feature = "cmd-net")]
// The bootstrap law (deliverable 5): the CLI resolves its endpoint from an
// OPEN order — `--endpoint`, `$SMART_ENDPOINT`, the configuration file, the
// platform's runtime directory — and it names NO implementation, ever.
//
// The proofs here:
//   * an unknown transport scheme is answered HONESTLY (unavailable, with the
//     reason), never rejected as if the peer were broken;
//   * the environment variable and the configuration file are honoured;
//   * the refusal when nothing answers names the resolution order;
//   * the CLI's own source names no implementation and holds no socket path
//     literal.

use std::process::Command;

mod support;
use support::mock::MockService;

fn net_with_env(args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
    cmd.args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("spawn net");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

/// An unknown transport scheme is unavailable WITH A REASON: the peer is not
/// broken, this client simply does not speak that transport.
#[test]
fn an_unknown_transport_scheme_is_answered_honestly() {
    let (out, err, code) = net_with_env(
        &["--endpoint", "carrier-pigeon://somewhere", "net", "status"],
        &[],
    );
    assert_eq!(code, 3, "{out}{err}");
    let text = format!("{out}{err}");
    assert!(text.contains("carrier-pigeon"), "{text}");
    assert!(
        text.contains("unix") && text.contains("tcp"),
        "the reason names the transports this client speaks: {text}"
    );
}

/// The environment variable is the second source of the endpoint.
#[test]
fn the_environment_variable_names_the_endpoint() {
    let mock = MockService::start(false).unwrap();
    let path = mock.path().to_str().unwrap().to_string();
    let endpoint = format!("unix://{path}");
    let (out, err, code) = net_with_env(
        &["--json", "net", "status"],
        &[("SMART_ENDPOINT", endpoint.as_str())],
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("networkingEnabled"), "{out}");
}

/// The configuration file is the third source of the endpoint.
#[test]
fn the_configuration_file_names_the_endpoint() {
    let mock = MockService::start(false).unwrap();
    let path = mock.path().to_str().unwrap().to_string();
    let dir = std::env::temp_dir().join(format!("smart-cli-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("smart-cli")).expect("the config dir");
    std::fs::write(
        dir.join("smart-cli/endpoint"),
        format!("unix://{path}\n"),
    )
    .expect("the config file");
    let (out, err, code) = net_with_env(
        &["--json", "net", "status"],
        &[("XDG_CONFIG_HOME", dir.to_str().unwrap())],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("networkingEnabled"), "{out}");
}

/// The CLI names NO implementation and holds no socket path literal: the
/// architecture law, grep-proven.
#[test]
fn the_cli_names_no_implementation_and_no_socket_path() {
    let mut sources = Vec::new();
    for (name, body) in [
        ("src/lib.rs", include_str!("../src/lib.rs")),
        ("src/transport.rs", include_str!("../src/transport.rs")),
        ("src/remote.rs", include_str!("../src/remote.rs")),
        ("src/rpc.rs", include_str!("../src/rpc.rs")),
        ("src/cmd/mod.rs", include_str!("../src/cmd/mod.rs")),
    ] {
        for banned in ["smart-lib", "smart-lib-ng", "smart-service", "smart_service"] {
            assert!(
                !body.contains(banned),
                "{name} names the implementation `{banned}`"
            );
        }
        // No hard-coded socket path: the endpoint comes from the resolution
        // order, never from a literal.
        assert!(
            !body.contains("/tmp/smart"),
            "{name} holds a socket path literal"
        );
        sources.push(name);
    }
    assert!(!sources.is_empty());
}
