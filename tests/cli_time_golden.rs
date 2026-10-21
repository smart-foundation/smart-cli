#![cfg(feature = "cmd-time")]
// Compiled only against a cmd-time build (its schema + command surface).
//! The `time` family's golden tests: the wall clock, the arithmetic calendars,
//! the date arithmetic — the coupled shell's presentation, over the wire.

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
fn time_now_is_the_contract_shaped_reading() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "time", "now"]);
    assert_eq!(code, 0, "{out}");
    // The contract's own field names, verbatim.
    for key in ["dateTime", "year", "month", "day", "weekday", "timezone"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn time_now_human_keeps_the_shells_tree() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["time", "now"]);
    assert_eq!(code, 0, "{out}");
    // The shell's own words and glyphs.
    for token in ["🕐", "Now", "Wall", "Weekday", "Zone", "CLOCK_REALTIME"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

#[test]
fn time_cal_lists_the_contract_calendars() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "time", "cal"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"conversions\""), "{out}");
    assert!(out.contains("\"calendar\""), "{out}");
    // The contract's own calendar names.
    for calendar in ["gregorian", "julian", "islamic", "ethiopian"] {
        assert!(out.contains(calendar), "{calendar} is missing: {out}");
    }
}

#[test]
fn time_cal_human_keeps_the_shells_glyphs() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["time", "cal"]);
    assert_eq!(code, 0, "{out}");
    for glyph in ["▣", "▤", "◍", "◈"] {
        assert!(out.contains(glyph), "{glyph} is missing: {out}");
    }
    assert!(out.contains("CALENDAR"), "{out}");
}

#[test]
fn time_add_and_sub_are_the_arithmetic() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (add, _, code) = cli(p, &["time", "add", "2026-01-31", "1month"]);
    assert_eq!(code, 0, "{add}");
    assert!(add.contains("📅"), "{add}");
    let (sub, _, code) = cli(p, &["time", "sub", "2026-03-31", "1month"]);
    assert_eq!(code, 0, "{sub}");
    assert!(sub.contains("📅"), "{sub}");
}

#[test]
fn time_diff_reports_the_difference() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["--json", "time", "diff", "2026-01-01", "2027-03-04"],
    );
    assert_eq!(code, 0, "{out}");
    for key in ["years", "months", "days", "totalDays"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

/// The surface is the SERVICE's: a verb the service does not publish is
/// refused with the service's own list, never answered from this build.
#[test]
fn time_tai_is_refused_because_the_service_does_not_publish_it() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = cli(mock.path().to_str().unwrap(), &["time", "tai"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(
        err.contains("the service publishes no command named"),
        "{err}"
    );
}
