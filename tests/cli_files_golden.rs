#![cfg(feature = "cmd-files")]
// Compiled only against a cmd-files build (its schema + command surface).
//! The `files` family's golden tests: the listing, the stat, the usage, the
//! versions, the trash, the content search — the coupled shell's presentation,
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
fn files_ls_is_the_contract_shaped_listing() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "files", "ls"]);
    assert_eq!(code, 0, "{out}");
    for key in ["entries", "name", "path", "type", "size"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn files_ls_human_keeps_the_shells_table() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["files", "ls"]);
    assert_eq!(code, 0, "{out}");
    for token in ["📁", "NAME", "KIND", "SIZE", "MODIFIED", "README.md", "src"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
    // The kind glyphs: the shell's own vocabulary.
    assert!(out.contains('▣'), "the file glyph is missing: {out}");
    assert!(out.contains('▤'), "the directory glyph is missing: {out}");
}

#[test]
fn files_stat_reads_the_contract_fields() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "files", "stat", "README.md"]);
    assert_eq!(code, 0, "{out}");
    for key in ["result", "path", "type", "size", "modified", "mimeType"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn files_stat_human_keeps_the_shells_tree() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["files", "stat", "README.md"]);
    assert_eq!(code, 0, "{out}");
    for token in ["🔎", "Stat", "Type", "Size", "Created", "Modified", "Mime"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

#[test]
fn files_du_reports_the_hosts_own_usage() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "files", "du", "."]);
    assert_eq!(code, 0, "{out}");
    for key in ["usage", "total", "used", "free", "mountPoint"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn files_versions_lists_the_store_trail() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["--json", "files", "versions", "README.md"],
    );
    assert_eq!(code, 0, "{out}");
    for key in ["versions", "versionId", "size", "message", "createdAt"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn files_locks_keeps_the_shells_columns() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["files", "locks", "."]);
    assert_eq!(code, 0, "{out}");
    for token in ["🔒", "ID", "TYPE", "OWNER", "ACQUIRED"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

#[test]
fn files_trash_and_recents_are_the_hosts_own() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (trash, _, code) = cli(p, &["--json", "files", "trash"]);
    assert_eq!(code, 0, "{trash}");
    assert!(trash.contains("\"originalPath\""), "{trash}");
    let (recents, _, code) = cli(p, &["--json", "files", "recents"]);
    assert_eq!(code, 0, "{recents}");
    assert!(recents.contains("\"files\""), "{recents}");
}

#[test]
fn files_grep_reports_the_content_search() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["--json", "files", "grep", "spine"],
    );
    assert_eq!(code, 0, "{out}");
    for key in ["results", "path", "lineNumber", "snippet"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn files_health_and_quota_are_the_hosts_verdicts() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (health, _, code) = cli(p, &["--json", "files", "health", "."]);
    assert_eq!(code, 0, "{health}");
    assert!(health.contains("\"status\""), "{health}");
    let (quota, _, code) = cli(p, &["--json", "files", "quota", "."]);
    assert_eq!(code, 0, "{quota}");
    for key in ["quota", "maxSizeBytes", "currentFiles"] {
        assert!(quota.contains(&format!("\"{key}\"")), "{key} is missing: {quota}");
    }
}

/// A write verb is GATE-ROUTED: the CLI issues the call, the service decides.
/// On a read-only connection the service refuses — and the refusal is the
/// service's own reason, never a fabricated success.
#[test]
fn files_rm_is_refused_by_the_services_capability_class() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = cli(
        mock.path().to_str().unwrap(),
        &["files", "rm", "/tmp/never-created-by-this-test"],
    );
    // The CLI's own exit table: a refused capability is exit 4.
    assert_eq!(code, 4, "{out}{err}");
    assert!(err.contains("permissionDenied"), "{err}");
}
