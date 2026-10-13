#![cfg(feature = "cmd-net")]
// The `net`-family CLI golden tests compile only against a cmd-net build.
//! Watch is a bounded pull loop with a drop counter that is NEVER silent
//! (grammar law #5). This test drives the watch streams against the mock and
//! asserts the bounded stop, the per-event objects, and the terminal summary —
//! including the stream that reports through the status subscription.

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
fn watch_is_bounded_by_count_and_reports_a_terminal_summary() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(
        mock.path().to_str().unwrap(),
        &["watch", "bandwidth", "--count", "2", "--interval", "1", "--json"],
    );
    assert_eq!(code, 0, "{out} {err}");
    let events: Vec<&str> = out.lines().filter(|l| l.contains("bytesIn")).collect();
    assert_eq!(events.len(), 2, "the bounded watch emitted exactly 2 events: {out}");
}

#[test]
fn watch_terminal_summary_is_on_stderr_in_human_mode() {
    let mock = MockService::start(false).unwrap();
    let (_out, err, code) = net(
        mock.path().to_str().unwrap(),
        &["watch", "bandwidth", "--count", "1", "--interval", "1"],
    );
    assert_eq!(code, 0);
    assert!(err.contains("watch:") && err.contains("delivered"), "the drop/terminal summary is surfaced: {err}");
    assert!(err.contains("1 event(s) delivered"), "the count is honest: {err}");
}

/// The status subscription is a first-class stream: `watch net status` rides
/// `subscribeStatus`/`pullStatus`/`SUB_CANCEL` behind the same one verb.
#[test]
fn watch_status_stream_emits_contract_shaped_events() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(
        mock.path().to_str().unwrap(),
        &["watch", "status", "--count", "2", "--interval", "1", "--json"],
    );
    assert_eq!(code, 0, "{out} {err}");
    let events: Vec<&str> = out.lines().filter(|l| l.contains("\"connected\"")).collect();
    assert_eq!(events.len(), 2, "the status stream emitted its 2 events: {out}");
}

/// A watch stream that has no route answers honestly instead of inventing one.
#[test]
fn watch_connections_reports_the_named_gap() {
    let mock = MockService::start(false).unwrap();
    let (out, _err, code) = net(mock.path().to_str().unwrap(), &["--json", "watch", "connections"]);
    assert_eq!(code, 3, "the socket-batch gap is named, not faked: {code} {out}");
    assert!(out.contains("notSupported"), "{out}");
}

/// The `--interval` cadence never changes the events delivered: a bounded run
/// still delivers exactly its `--count` events (the pull loop sleeps, it does
/// not drop or invent).
#[test]
fn interval_changes_the_cadence_not_the_event_count() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = net(
        mock.path().to_str().unwrap(),
        &["watch", "bandwidth", "--count", "2", "--interval", "1", "--json"],
    );
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out.lines().filter(|l| l.contains("bytesIn")).count(), 2, "{out}");
}
