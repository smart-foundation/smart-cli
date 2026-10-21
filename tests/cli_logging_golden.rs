#![cfg(feature = "cmd-logging")]
// Compiled only against a cmd-logging build (its schema + command surface).
//! The `logging` family's golden tests: the domains, the retained lines, the
//! export — the coupled shell's presentation, over the wire.

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
fn logging_domains_are_the_contract_shaped_listing() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["--json", "logging", "domains"],
    );
    assert_eq!(code, 0, "{out}");
    for key in ["domains", "name", "level", "description", "samplingRate"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn logging_domains_human_keeps_the_shells_columns() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["logging", "domains"]);
    assert_eq!(code, 0, "{out}");
    for token in ["🗂", "DOMAIN", "LEVEL", "SAMPLING", "WHAT IT IS"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
    // The level glyph vocabulary: the shell's own.
    assert!(out.contains('◉'), "the info glyph is missing: {out}");
    assert!(out.contains('◐'), "the warn glyph is missing: {out}");
}

#[test]
fn logging_sinks_is_the_same_reading_as_the_domains() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (sinks, _, code) = cli(p, &["--json", "logging", "sinks"]);
    assert_eq!(code, 0, "{sinks}");
    let (domains, _, code) = cli(p, &["--json", "logging", "domains"]);
    assert_eq!(code, 0, "{domains}");
    // One method, one presenter: the two paths answer alike.
    assert_eq!(sinks, domains, "the service publishes both paths against one method");
}

#[test]
fn logging_tail_and_entries_are_the_retained_lines() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (tail, _, code) = cli(p, &["--json", "logging", "tail"]);
    assert_eq!(code, 0, "{tail}");
    for key in ["entries", "level", "domain", "message", "sequence"] {
        assert!(tail.contains(&format!("\"{key}\"")), "{key} is missing: {tail}");
    }
    let (entries, _, code) = cli(p, &["--json", "logging", "entries"]);
    assert_eq!(code, 0, "{entries}");
    assert!(entries.contains("\"entries\""), "{entries}");
}

#[test]
fn logging_tail_human_keeps_the_shells_line() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["logging", "tail"]);
    assert_eq!(code, 0, "{out}");
    for token in ["📜", "Tail", "the host's own lines, read natively"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

#[test]
fn logging_level_and_emit_are_the_contract_readings() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (level, _, code) = cli(p, &["--json", "logging", "level", "--domain", "smart.cli"]);
    assert_eq!(code, 0, "{level}");
    assert!(level.contains("\"level\""), "{level}");
    let (emit, _, code) = cli(p, &["--json", "logging", "emit", "--level", "info", "hello"]);
    assert_eq!(code, 0, "{emit}");
    assert!(emit.contains("\"outcome\""), "{emit}");
}

#[test]
fn logging_export_renders_the_open_content_type() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["--json", "logging", "export"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"mimeType\""), "{out}");
    assert!(out.contains("\"bytes\""), "{out}");
}

/// A write verb is gate-routed: the CLI issues the call, the service decides.
#[test]
fn logging_set_level_is_refused_by_the_services_capability_class() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = cli(
        mock.path().to_str().unwrap(),
        &["logging", "set-level", "--domain", "smart.cli", "--level", "debug"],
    );
    assert_eq!(code, 4, "{out}{err}");
    assert!(err.contains("permissionDenied"), "{err}");
}
