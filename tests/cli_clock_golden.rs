#![cfg(feature = "cmd-clock")]
// Compiled only against a cmd-clock build (its schema + command surface).
//! The `clock` family's golden tests: the measured state of this host's
//! clocks, the domains, the sync verdict — the coupled shell's presentation,
//! over the wire.

use std::process::Command;

mod support;
use support::mock::MockService;

fn cli(mock_path: &str, args: &[&str]) -> (String, String, i32) {
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
fn clock_info_is_the_contract_shaped_state() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "clock", "info"]);
    assert_eq!(code, 0, "{out}");
    for key in ["info", "monotonic", "realtime", "resolution", "sourceName", "sync"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn clock_info_human_keeps_the_shells_tree() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["clock", "info"]);
    assert_eq!(code, 0, "{out}");
    for token in [
        "🕰",
        "Clock",
        "Realtime",
        "Monotonic",
        "Uptime",
        "Resolution",
        "Sync",
        "Security",
        "Domains",
        "CLOCK_REALTIME",
    ] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

#[test]
fn clock_now_and_monotonic_are_the_two_readings() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (now, _, code) = cli(p, &["--json", "clock", "now"]);
    assert_eq!(code, 0, "{now}");
    assert!(now.contains("\"time\""), "{now}");
    let (monotonic, _, code) = cli(p, &["--json", "clock", "monotonic"]);
    assert_eq!(code, 0, "{monotonic}");
    assert!(monotonic.contains("\"time\""), "{monotonic}");
}

#[test]
fn clock_uptime_and_security_keep_their_own_views() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (uptime, _, code) = cli(p, &["clock", "uptime"]);
    assert_eq!(code, 0, "{uptime}");
    assert!(uptime.contains("⏳"), "{uptime}");
    assert!(uptime.contains("Uptime"), "{uptime}");
    let (security, _, code) = cli(p, &["clock", "security"]);
    assert_eq!(code, 0, "{security}");
    assert!(security.contains("🛡"), "{security}");
    assert!(security.contains("Clock security"), "{security}");
}

#[test]
fn clock_domains_and_resolution_keep_the_table() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (domains, _, code) = cli(p, &["--json", "clock", "domains"]);
    assert_eq!(code, 0, "{domains}");
    for key in ["domains", "name", "resolution", "available"] {
        assert!(domains.contains(&format!("\"{key}\"")), "{key} is missing: {domains}");
    }
    let (resolution, _, code) = cli(p, &["clock", "resolution"]);
    assert_eq!(code, 0, "{resolution}");
    assert!(resolution.contains("📐"), "{resolution}");
    assert!(resolution.contains("Resolution"), "{resolution}");
}

#[test]
fn clock_sync_quality_and_drift_are_the_measured_verdicts() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (sync, _, code) = cli(p, &["--json", "clock", "sync"]);
    assert_eq!(code, 0, "{sync}");
    for key in ["synced", "source", "stratum", "offset"] {
        assert!(sync.contains(&format!("\"{key}\"")), "{key} is missing: {sync}");
    }
    let (quality, _, code) = cli(p, &["--json", "clock", "quality"]);
    assert_eq!(code, 0, "{quality}");
    for key in ["accuracy", "stability", "reliability", "sampleCount"] {
        assert!(
            quality.contains(&format!("\"{key}\"")),
            "{key} is missing: {quality}"
        );
    }
    let (drift, _, code) = cli(p, &["--json", "clock", "drift"]);
    assert_eq!(code, 0, "{drift}");
    assert!(drift.contains("\"drift\""), "{drift}");
}

#[test]
fn clock_timers_are_the_stores_own_list() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "clock", "timers"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"timers\""), "{out}");
    assert!(out.contains("\"label\""), "{out}");
}
