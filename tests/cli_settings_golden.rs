#![cfg(feature = "cmd-settings")]
// Compiled only against a cmd-settings build (its schema + command surface).
//! The `settings` family's golden tests: the domains, the values, the
//! versions, the change trail — the coupled shell's presentation, over the
//! wire.

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
fn settings_domains_are_the_schema_source_the_host_publishes() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "settings", "domains"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"domains\""), "{out}");
    assert!(out.contains("desktop"), "{out}");
}

#[test]
fn settings_domains_human_keeps_the_shells_table() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["settings", "domains"]);
    assert_eq!(code, 0, "{out}");
    for token in ["⚙", "Settings", "DOMAIN", "the host's own schema source"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

#[test]
fn settings_list_is_the_contract_shaped_map() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["--json", "settings", "list", "desktop"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"settings\""), "{out}");
    assert!(out.contains("\"key\""), "{out}");
    assert!(out.contains("\"value\""), "{out}");
    // The contract's own union arm: a value is typed by its arm.
    assert!(out.contains("\"string\"") || out.contains("\"boolean\""), "{out}");
}

#[test]
fn settings_list_human_keeps_the_shells_columns() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["settings", "list", "desktop"],
    );
    assert_eq!(code, 0, "{out}");
    for token in ["🗂", "KEY", "VALUE", "TYPE"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

#[test]
fn settings_get_reports_the_value_and_the_not_set_answer() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (value, _, code) = cli(p, &["--json", "settings", "get", "desktop", "theme"]);
    assert_eq!(code, 0, "{value}");
    assert!(value.contains("\"value\""), "{value}");
    // `notSet` is an ANSWER, never a sentinel.
    let (unset, _, code) = cli(p, &["--json", "settings", "get", "desktop", "missing"]);
    assert_eq!(code, 0, "{unset}");
    assert!(unset.contains("notSet"), "{unset}");
}

#[test]
fn settings_version_history_and_diff_are_the_contract_readings() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (version, _, code) = cli(p, &["--json", "settings", "version", "desktop", "theme"]);
    assert_eq!(code, 0, "{version}");
    assert!(version.contains("\"version\""), "{version}");
    let (history, _, code) = cli(p, &["--json", "settings", "history", "desktop", "theme"]);
    assert_eq!(code, 0, "{history}");
    for key in ["changes", "actorId", "version"] {
        assert!(history.contains(&format!("\"{key}\"")), "{key} is missing: {history}");
    }
    let (diff, _, code) = cli(p, &["--json", "settings", "diff", "desktop"]);
    assert_eq!(code, 0, "{diff}");
    assert!(diff.contains("\"currentVersion\""), "{diff}");
}

#[test]
fn settings_category_and_bulk_get_are_the_contract_reads() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (category, _, code) = cli(p, &["--json", "settings", "category", "appearance"]);
    assert_eq!(code, 0, "{category}");
    assert!(category.contains("\"entries\""), "{category}");
    let (bulk, _, code) = cli(p, &["--json", "settings", "bulk-get", "desktop", "theme"]);
    assert_eq!(code, 0, "{bulk}");
    assert!(bulk.contains("\"entries\""), "{bulk}");
}

/// A write verb is gate-routed: the CLI issues the call, the service decides.
#[test]
fn settings_set_is_refused_by_the_services_capability_class() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = cli(
        mock.path().to_str().unwrap(),
        &["settings", "set", "desktop", "theme", "light"],
    );
    assert_eq!(code, 4, "{out}{err}");
    assert!(err.contains("permissionDenied"), "{err}");
}
