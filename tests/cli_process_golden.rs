#![cfg(feature = "cmd-process")]
// Compiled only against a cmd-process build (its schema + command surface).
//! The `process` family's golden tests: the enumeration, the detail, the
//! threads, the environment, the accounting — the coupled shell's
//! presentation, over the wire.

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
fn process_list_is_the_contract_shaped_enumeration() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["--json", "process", "list"]);
    assert_eq!(code, 0, "{out}");
    for key in ["processes", "pid", "name", "state", "memoryUsage", "cpuUsage", "threads"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn process_list_human_keeps_the_shells_columns() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(mock.path().to_str().unwrap(), &["process", "list"]);
    assert_eq!(code, 0, "{out}");
    for token in ["🧮", "PID", "NAME", "STATE", "UID", "MEM", "CPU", "THR"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
    // The state glyph: the shell's own vocabulary.
    assert!(out.contains('◉'), "the active glyph is missing: {out}");
}

#[test]
fn process_inspect_reads_the_contract_fields() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["--json", "process", "inspect", "4242"],
    );
    assert_eq!(code, 0, "{out}");
    for key in ["info", "pid", "command", "state", "memoryUsage", "startTime"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn process_inspect_human_keeps_the_shells_tree() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["process", "inspect", "4242"],
    );
    assert_eq!(code, 0, "{out}");
    for token in ["🔬", "Process", "State", "Parent", "Command", "Memory", "CPU"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

#[test]
fn process_threads_and_env_are_the_kernels_own_rows() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (threads, _, code) = cli(p, &["--json", "process", "threads", "4242"]);
    assert_eq!(code, 0, "{threads}");
    assert!(threads.contains("\"threads\""), "{threads}");
    let (env, _, code) = cli(p, &["--json", "process", "env", "4242"]);
    assert_eq!(code, 0, "{env}");
    assert!(env.contains("\"env\""), "{env}");
    assert!(env.contains("HOME"), "{env}");
}

#[test]
fn process_maps_limits_and_accounting_are_the_contract_readings() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (maps, _, code) = cli(p, &["--json", "process", "maps", "4242"]);
    assert_eq!(code, 0, "{maps}");
    for key in ["regions", "start", "end", "perms", "name"] {
        assert!(maps.contains(&format!("\"{key}\"")), "{key} is missing: {maps}");
    }
    let (limits, _, code) = cli(p, &["--json", "process", "limits", "4242"]);
    assert_eq!(code, 0, "{limits}");
    assert!(limits.contains("\"limits\""), "{limits}");
    let (accounting, _, code) = cli(p, &["--json", "process", "accounting", "4242"]);
    assert_eq!(code, 0, "{accounting}");
    for key in ["usage", "cpuTime", "memoryBytes", "peakMemoryBytes"] {
        assert!(
            accounting.contains(&format!("\"{key}\"")),
            "{key} is missing: {accounting}"
        );
    }
}

#[test]
fn process_groups_and_stats_are_the_kernels_own_aggregates() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (groups, _, code) = cli(p, &["--json", "process", "groups"]);
    assert_eq!(code, 0, "{groups}");
    assert!(groups.contains("\"groupId\""), "{groups}");
    let (stats, _, code) = cli(p, &["--json", "process", "stats"]);
    assert_eq!(code, 0, "{stats}");
    for key in ["totalProcesses", "active", "blocked", "totalMemoryUsage"] {
        assert!(stats.contains(&format!("\"{key}\"")), "{key} is missing: {stats}");
    }
}

#[test]
fn process_context_reports_what_the_process_is() {
    let mock = MockService::start(false).unwrap();
    let (out, _, code) = cli(
        mock.path().to_str().unwrap(),
        &["process", "context", "4242"],
    );
    assert_eq!(code, 0, "{out}");
    for token in ["🧭", "Execution context", "Capabilities", "Quota"] {
        assert!(out.contains(token), "{token} is missing: {out}");
    }
}

/// A write verb is gate-routed: the CLI issues the call, the service decides.
/// The read-only class refuses it — with the service's own reason.
#[test]
fn process_attach_is_refused_by_the_services_capability_class() {
    let mock = MockService::start(false).unwrap();
    let (out, err, code) = cli(
        mock.path().to_str().unwrap(),
        &["process", "attach", "4242"],
    );
    assert_eq!(code, 4, "{out}{err}");
    assert!(err.contains("permissionDenied"), "{err}");
}
