#![cfg(feature = "cmd-net")]
// The `net`-family CLI golden tests compile only against a cmd-net build.
//! Completions are GENERATED from the same registry table the help screen
//! reads (plan Part 4.3) — never hand-written per shell. These tests drive the
//! built `net` binary against the mock service and assert, per shell: the
//! command list, the `--where`/`watch` verbs, the honesty filter, the honest
//! refusal for a shell that is not expressed, and that `install` writes only
//! where the user points.

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

/// Each shell's completer must name the command list and the selector/watch
/// verbs the grammar law requires.
#[test]
fn every_shell_completes_the_registry() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    for shell in ["bash", "zsh", "fish", "elvish"] {
        let (out, err, code) = net(p, &["completions", shell]);
        assert_eq!(code, 0, "{shell}: {err}");
        for word in [
            "wifi", "dns", "firewall", "routes", "interface", "watch", "status", "networking",
        ] {
            assert!(out.contains(word), "{shell} does not complete '{word}':\n{out}");
        }
        assert!(out.contains("--where"), "{shell} does not complete the selector flag:\n{out}");
        assert!(
            out.contains("--count") || out.contains("--interval") || out.contains("--follow"),
            "{shell} does not complete the watch flags:\n{out}"
        );
        // The two-level shape: a task's verbs follow it.
        assert!(out.contains("stats") && out.contains("connect"), "{shell} lacks wifi verbs");
    }
}

/// The generator is honest about a shell it does not express: exit 2 naming
/// the shell and what IS expressed — never a fake completion.
#[test]
fn an_unexpressed_shell_refuses_honestly() {
    let mock = MockService::start(false).unwrap();
    let (_out, err, code) = net(mock.path().to_str().unwrap(), &["completions", "powershell"]);
    assert_eq!(code, 2, "the refusal is a usage answer: {code}");
    // The human refusal is on stderr; the JSON one is the result object.
    assert!(err.contains("powershell"), "stderr: {err}");
    assert!(err.contains("bash"), "the refusal names the expressed shells: {err}");
    let (json, _err, code) = net(mock.path().to_str().unwrap(), &["--json", "completions", "powershell"]);
    assert_eq!(code, 2, "{json}");
    assert!(json.contains("usage") && json.contains("powershell"), "{json}");
}

/// The shell-agnostic form is the registry itself, in JSON when asked.
#[test]
fn the_json_corpus_is_the_registry_data() {
    let mock = MockService::start(false).unwrap();
    let (out, _err, code) = net(mock.path().to_str().unwrap(), &["--json", "completions"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"completions\""), "{out}");
    assert!(out.contains("\"task\":\"wifi\""), "the task word is the registry's: {out}");
    assert!(out.contains("\"verbs\""), "{out}");
    assert!(out.contains("--where"), "{out}");
    // The mock PUBLISHES a registry (an empty unavailable list), so the
    // availability filter is verified — not assumed.
    assert!(out.contains("\"availabilityVerified\":true"), "{out}");
}

/// The honesty law applies to completion exactly as it applies to help: a
/// command the service reports not runnable is NOT completed as if it can.
#[test]
fn unavailable_commands_are_not_completed() {
    let mock = MockService::start_with(false, vec!["wifi scan".to_string()]).unwrap();
    let p = mock.path().to_str().unwrap();
    let (out, err, _code) = net(p, &["completions", "bash"]);
    assert!(err.contains("withheld"), "the human view names what was withheld: {err}");
    assert!(err.contains("wifi scan"), "{err}");
    assert!(
        !out.contains("wifi)"),
        "the withheld task must not be completed as if it can:\n{out}"
    );
}

/// `install` writes ONLY where the user points, and `--dry-run` writes nothing.
#[test]
fn install_writes_only_where_the_user_points_and_dry_run_writes_nothing() {
    let mock = MockService::start(true).unwrap();
    let p = mock.path().to_str().unwrap();
    let target = std::env::temp_dir().join(format!("net-completions-{}.bash", std::process::id()));

    // Without --path: an honest refusal, no write.
    let (_out, err, code) = net(p, &["install", "completions", "--shell", "bash"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("--path"), "{err}");
    assert!(!target.exists(), "no file was written: {:?}", target);

    // --dry-run prints what it would write and writes nothing.
    let (out, _err, code) = net(
        p,
        &[
            "install",
            "completions",
            "--shell",
            "bash",
            "--path",
            target.to_str().unwrap(),
            "--dry-run",
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("would write"), "{out}");
    assert!(out.contains("complete -F _net net"), "the completer is printed: {out}");
    assert!(!target.exists(), "a dry run must not write: {:?}", target);

    // A real install writes the file at the user's path.
    let (out, _err, code) = net(
        p,
        &["install", "completions", "--shell", "zsh", "--path", target.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("installed zsh completion"), "{out}");
    let written = std::fs::read_to_string(&target).expect("the file the user pointed at");
    assert!(written.contains("#compdef net"), "the generated completer is what landed: {written}");
    assert!(written.contains("wifi"), "{written}");
    let _ = std::fs::remove_file(&target);
}

/// The completion corpus and the help screen read the SAME registry: any task
/// word help prints appears in the completion output.
#[test]
fn help_and_completions_read_the_same_registry() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (help, _err, code) = net(p, &["--help"]);
    assert_eq!(code, 0);
    let (bash, _err, code) = net(p, &["completions", "bash"]);
    assert_eq!(code, 0, "{bash}");
    for word in ["wifi", "dns", "firewall", "routes", "proxy", "ping", "diagnose", "profile"] {
        assert!(help.contains(word), "help does not list '{word}':\n{help}");
        assert!(bash.contains(word), "completions do not offer '{word}':\n{bash}");
    }
}

/// With no service reachable, the completions still work and SAY that
/// availability is unverified (honest, not "everything runs").
#[test]
fn completions_work_without_a_service_and_say_so() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
    cmd.arg("--connect")
        .arg("/tmp/kilo/net-absent-service.sock")
        .args(["completions", "bash"]);
    let output = cmd.output().expect("spawn net");
    assert!(output.status.success());
    let out = String::from_utf8_lossy(&output.stdout).to_string();
    let err = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(out.contains("wifi"), "the local registry still completes: {out}");
    assert!(err.contains("UNVERIFIED"), "and it says availability is unverified: {err}");
}
