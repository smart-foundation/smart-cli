#![cfg(any(
    feature = "cmd-files",
    feature = "cmd-process",
    feature = "cmd-settings",
    feature = "cmd-logging",
    feature = "cmd-clock",
    feature = "cmd-time"
))]
//! The SPINE surface test: the commands this CLI offers for the six
//! contract-served families come from the SERVICE's registry — not from a
//! table written here — and a command the service does not publish is
//! refused with the service's own list.
//!
//! It also proves the LIVE SHAPE: the JSON the CLI prints carries the
//! CONTRACT's key paths, the same ones the service's own answer carries.

use std::process::Command;

mod support;
use support::mock::MockService;
use support::shape::shape;

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

/// The six families this build serves, with the service's own path for each.
fn families() -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if cfg!(feature = "cmd-files") {
        out.push(("files", "files ls"));
    }
    if cfg!(feature = "cmd-process") {
        out.push(("process", "process list"));
    }
    if cfg!(feature = "cmd-settings") {
        out.push(("settings", "settings domains"));
    }
    if cfg!(feature = "cmd-logging") {
        out.push(("logging", "logging domains"));
    }
    if cfg!(feature = "cmd-clock") {
        out.push(("clock", "clock info"));
    }
    if cfg!(feature = "cmd-time") {
        out.push(("time", "time now"));
    }
    out
}

/// The service's registry IS the surface: every path the CLI runs for these
/// families is a path the service published.
#[test]
fn every_family_path_the_cli_runs_is_one_the_service_published() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    for (family, path) in families() {
        let (out, err, code) = cli(p, &["--json", family, path.split(' ').nth(1).unwrap()]);
        assert_eq!(code, 0, "{family}: {out}{err}");
        assert!(!out.is_empty(), "{family} answered nothing: {out}{err}");
    }
}

/// A path the service does NOT publish is refused — and the refusal names the
/// service's own list. The CLI has no parallel table to fall back on.
#[test]
fn a_path_the_service_does_not_publish_is_refused_with_the_services_own_list() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    for (family, _) in families() {
        let (out, err, code) = cli(p, &[family, "not-a-command-the-service-publishes"]);
        assert_eq!(code, 2, "{family}: {out}{err}");
        assert!(
            err.contains("the service publishes no command named"),
            "{family}: {err}"
        );
        // The refusal names what the service DOES publish for that family.
        assert!(err.contains("the service publishes:"), "{family}: {err}");
    }
}

/// The help screen prints the service's own published commands for the
/// families this build does not implement itself — the surface the service
/// offers, not a table written here.
#[test]
fn the_help_screen_lists_what_the_service_publishes() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (out, _, code) = cli(p, &["--help"]);
    assert_eq!(code, 0, "{out}");
    for (family, _) in families() {
        assert!(
            out.contains(family),
            "the help does not name the {family} family: {out}"
        );
    }
    assert!(
        out.contains("published by the service"),
        "the help does not carry the service's published list: {out}"
    );
}

/// The completions are generated from the service's registry too: the paths
/// the service publishes for these families are completable.
#[test]
fn the_completions_carry_the_services_paths() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (out, _, code) = cli(p, &["completions", "bash"]);
    assert_eq!(code, 0, "{out}");
    for (family, path) in families() {
        // The corpus carries the service's paths as a task word + its verbs.
        let verb = path.split(' ').nth(1).unwrap();
        assert!(
            out.contains(family),
            "the completions do not carry the {family} family: {out}"
        );
        assert!(
            out.contains(verb),
            "the completions do not carry the service's {path}: {out}"
        );
    }
}

/// The LIVE SHAPE: the JSON the CLI prints carries the contract's key paths.
#[test]
fn the_json_carries_the_contract_shape() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (files, _, code) = cli(p, &["--json", "files", "ls"]);
    if cfg!(feature = "cmd-files") {
        assert_eq!(code, 0, "{files}");
        let paths = shape(&files);
        assert!(paths.contains(&"entries".to_string()), "{paths:?}");
        assert!(paths.contains(&"entries[].name".to_string()), "{paths:?}");
        assert!(paths.contains(&"entries[].type".to_string()), "{paths:?}");
        assert!(paths.contains(&"entries[].size".to_string()), "{paths:?}");
    }
    let (process, _, code) = cli(p, &["--json", "process", "list"]);
    if cfg!(feature = "cmd-process") {
        assert_eq!(code, 0, "{process}");
        let paths = shape(&process);
        assert!(paths.contains(&"processes".to_string()), "{paths:?}");
        assert!(paths.contains(&"processes[].pid".to_string()), "{paths:?}");
        assert!(paths.contains(&"processes[].state".to_string()), "{paths:?}");
    }
    let (settings, _, code) = cli(p, &["--json", "settings", "list", "desktop"]);
    if cfg!(feature = "cmd-settings") {
        assert_eq!(code, 0, "{settings}");
        let paths = shape(&settings);
        assert!(paths.contains(&"settings".to_string()), "{paths:?}");
        assert!(paths.contains(&"settings[].key".to_string()), "{paths:?}");
        assert!(paths.contains(&"settings[].value".to_string()), "{paths:?}");
    }
    let (logging, _, code) = cli(p, &["--json", "logging", "domains"]);
    if cfg!(feature = "cmd-logging") {
        assert_eq!(code, 0, "{logging}");
        let paths = shape(&logging);
        assert!(paths.contains(&"domains".to_string()), "{paths:?}");
        assert!(paths.contains(&"domains[].level".to_string()), "{paths:?}");
    }
    let (clock, _, code) = cli(p, &["--json", "clock", "info"]);
    if cfg!(feature = "cmd-clock") {
        assert_eq!(code, 0, "{clock}");
        let paths = shape(&clock);
        assert!(paths.contains(&"info".to_string()), "{paths:?}");
        assert!(paths.contains(&"info.realtime".to_string()), "{paths:?}");
        assert!(paths.contains(&"info.sync".to_string()), "{paths:?}");
    }
    let (time, _, code) = cli(p, &["--json", "time", "now"]);
    if cfg!(feature = "cmd-time") {
        assert_eq!(code, 0, "{time}");
        let paths = shape(&time);
        assert!(paths.contains(&"dateTime".to_string()), "{paths:?}");
        assert!(paths.contains(&"dateTime.year".to_string()), "{paths:?}");
        assert!(paths.contains(&"dateTime.timezone".to_string()), "{paths:?}");
    }
}
