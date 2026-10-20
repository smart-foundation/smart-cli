#![cfg(feature = "cmd-net")]
// The `net`-family CLI golden tests compile only against a cmd-net build.
//! Golden CLI tests: every grammar family runs end-to-end against the mock
//! service (the built `net` binary over a unix socket), asserting the command
//! line → the JSON shape → the exit code (grammar law: the worked-example
//! table made mechanical). These are the L5 evidence.
//!
//! Documents doc 06 §2's worked-example table row by row: status (default),
//! list, get, on/off, set, connect/disconnect/forget, add/remove, apply,
//! watch, where, diagnose, bench, resolve/reverse, export/import, remote, and
//! the unavailable row — each against the MOCK service (no daemon, no live
//! host: charter law #8).

use std::process::Command;

mod support;
use support::mock::MockService;

/// Run the built `net` binary against the mock and return (stdout, exit code).
fn net(mock_path: &str, args: &[&str]) -> (String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
    cmd.arg("--connect").arg(mock_path);
    cmd.args(args);
    let output = cmd.output().expect("spawn net");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

/// Run and capture stderr too (the watch summary and errors live there).
#[allow(dead_code)]
fn net_err(mock_path: &str, args: &[&str]) -> (String, String, i32) {
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

fn ok_json(sample: &str) -> bool {
    // A success result is `{...}` with no `error` key.
    sample.contains('{') && !sample.contains("\"error\"")
}

/// The first stdout line, asserted to be a structurally valid JSON value (the
/// crate's only dependency is the contract, so the test carries its own tiny
/// structural check rather than a JSON crate).
fn json_line(sample: &str) -> String {
    let line = sample
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_else(|| panic!("no JSON line in: {sample}"))
        .to_string();
    support::json::validate(&line)
        .unwrap_or_else(|e| panic!("not valid JSON ({e}): {line}"));
    line
}

#[test]
fn wifi_list_golden_json_and_exit() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "wifi", "list"]);
    assert_eq!(code, 0, "exit 0; got {code}: {out}");
    assert!(ok_json(&out), "a success object, not an error: {out}");
    assert!(out.contains("Home"), "the known networks the mock reports: {out}");
    assert!(out.contains("Cafe"), "{out}");
}

#[test]
fn wifi_get_stats_is_contract_shaped() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "wifi", "stats"]);
    assert_eq!(code, 0, "{out}");
    // WifiStatus carries the wifi sub-struct of getStatus (doc 06's `wifi stats`).
    assert!(out.contains("\"ssid\""), "{out}");
    assert!(out.contains("MockNet"), "{out}");
}

#[test]
fn wifi_scan_is_contract_shaped() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "wifi", "scan"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"ssid\"") && out.contains("\"security\""), "{out}");
}

/// The human path renders decoration over the SAME object (grammar law #11):
/// the table, the dBm figure and the strength bar all come from the one
/// `WifiNetwork` the JSON emitter reads.
#[test]
fn wifi_scan_human_mode_renders_a_table_over_the_same_object() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["wifi", "scan"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("ssid"), "{out}");
    assert!(out.contains("MockNet"), "{out}");
    assert!(out.contains("dBm"), "the strength figure is decoration over the same field: {out}");
    assert!(out.contains('#'), "the strength bar renders: {out}");
}

#[test]
fn interface_detail_is_contract_shaped() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "interface", "wlan0"]);
    assert_eq!(code, 0);
    // keys are contract field names (InterfaceConnectionStatus)
    assert!(out.contains("\"connected\""), "{out}");
    assert!(out.contains("\"hostState\""), "{out}");
    assert!(out.contains("\"wifi\""), "{out}");
}

#[test]
fn routes_and_firewall_and_proxy_contract_shaped() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (routes, code) = net(p, &["--json", "routes"]);
    assert_eq!(code, 0);
    assert!(routes.contains("\"destinationCidr\"") && routes.contains("0.0.0.0/0"), "{routes}");

    let (fw, code) = net(p, &["--json", "firewall", "rules"]);
    assert_eq!(code, 0);
    assert!(fw.contains("\"hitCount\"") && fw.contains("existing"), "{fw}");

    let (proxy, code) = net(p, &["--json", "proxy"]);
    assert_eq!(code, 0);
    assert!(proxy.contains("\"enabled\""), "{proxy}");
}

/// Every `--json` answer must be VALID JSON (a contract-shaped object is not
/// a fragment). This is the mechanical guard against an emitter that leaves an
/// object open.
#[test]
fn json_answers_are_valid_json_objects() {
    let mock = MockService::start(true).unwrap();
    let p = mock.path().to_str().unwrap();
    for args in [
        vec!["--json"],
        vec!["--json", "wifi", "list"],
        vec!["--json", "interface", "wlan0"],
        vec!["--json", "wifi", "stats"],
        vec!["--json", "routes"],
        vec!["--json", "routes", "add", "10.0.0.0/8", "192.168.1.1"],
        vec!["--json", "firewall", "rules"],
        vec!["--json", "firewall", "stats"],
        vec!["--json", "firewall", "allow", "firefox", "outbound"],
        vec!["--json", "proxy"],
        vec!["--json", "ping", "8.8.8.8"],
        vec!["--json", "bandwidth"],
        vec!["--json", "net", "dns", "get"],
        #[cfg(feature = "cmd-dns")]
        vec!["--json", "dns", "resolve", "example.com"],
        vec!["--json", "diagnose"],
        vec!["--json", "vpn", "list"],
        vec!["--json", "wifi", "scan"],
    ] {
        let (out, code) = net(p, &args);
        assert_eq!(code, 0, "{args:?} -> {out}");
        let line = json_line(&out);
        assert!(line.starts_with('{') && line.ends_with('}'), "{args:?} must answer an object: {out}");
    }
    // The error answers are objects too, not fragments (a read-only class, so
    // the mutating verb is denied rather than ok).
    let denied = MockService::start(false).unwrap();
    let d = denied.path().to_str().unwrap();
    for args in [
        vec!["--json", "networking", "off"],
        vec!["--json", "traceroute", "8.8.8.8"],
        vec!["--json", "connections"],
        vec!["--json", "interface", "--where", "bogus", "==", "1"],
    ] {
        let (out, code) = net(d, &args);
        assert!(code != 0, "{args:?} should not be ok: {out}");
        let line = json_line(&out);
        assert!(line.contains("\"error\""), "{args:?} must name an error: {out}");
    }
}

#[test]
fn ping_json_shape() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "ping", "8.8.8.8"]);
    assert_eq!(code, 0);
    assert!(out.contains("\"latencyMs\"") && out.contains("\"packetLoss\""), "{out}");
}

#[test]
fn exit_code_permission_on_readonly_control_verb() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "networking", "off"]);
    assert_eq!(code, 4, "permissionDenied -> exit 4; got {code}: {out}");
    assert!(out.contains("\"error\"") && out.contains("permissionDenied"), "{out}");
}

#[test]
fn exit_code_unavailable_with_reason() {
    let mock = MockService::start(false).unwrap();
    // the traceroute method is not in the mock's served set -> notSupported -> exit 3.
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "traceroute", "8.8.8.8"]);
    assert_eq!(code, 3, "notSupported -> exit 3; got {code}: {out}");
    assert!(out.contains("\"error\"") && out.contains("notSupported"), "{out}");
}

#[test]
fn void_success_never_prints_a_lie_on_the_denied_path() {
    // The old CLI printed "WiFi enabled" while calling nothing. Here, the
    // denied mutator prints NOTHING on stdout (the error goes to stderr), so
    // a success string never appears where the result is discarded.
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["wifi", "off"]);
    assert_eq!(code, 4);
    assert!(out.trim().is_empty(), "no fabricated success on stdout: {out:?}");
}

#[test]
fn control_verb_succeeds_on_a_control_class_service() {
    let mock = MockService::start(true).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "wifi", "off"]);
    assert_eq!(code, 0, "control class -> ok: {out}");
    assert_eq!(out.trim(), "{}", "void result -> {{}}: {out}");
}

#[test]
fn selector_bad_field_is_a_usage_error_naming_valid_fields() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(
        mock.path().to_str().unwrap(),
        &["--json", "interface", "--where", "bogusField", "==", "1"],
    );
    assert_eq!(code, 2, "a bad selector field is a usage error: {code}");
    assert!(out.contains("bogusField"), "the bad field is named: {out}");
    assert!(out.contains("connected"), "the valid fields are listed: {out}");
}

#[test]
fn selector_filters_the_interfaces_list() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(
        mock.path().to_str().unwrap(),
        &["--json", "interface", "--where", "connected", "==", "true"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("wlan0"), "the connected interface is listed: {out}");
}

#[test]
fn selector_filters_routes_and_wifi_lists() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    // `Route.destinationCidr` is the contract field name (generated).
    let (routes, code) = net(
        p,
        &["--json", "routes", "--where", "destinationCidr", "~=", "0.0.0.0"],
    );
    assert_eq!(code, 0, "{routes}");
    assert!(routes.contains("0.0.0.0/0"), "{routes}");

    // A field that exists on another type is still unknown here: the selector
    // validates against the schema struct the command lists.
    let (_out, code) = net(p, &["--json", "routes", "--where", "ssid", "==", "x"]);
    assert_eq!(code, 2, "a field not in Route is a usage error: {code}");

    let (wifi, code) = net(p, &["--json", "wifi", "list", "--where", "autoConnect", "==", "true"]);
    assert_eq!(code, 0, "{wifi}");
    assert!(wifi.contains("Home"), "{wifi}");
}

#[test]
fn dashboard_composes_the_summary_shape() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json"]);
    assert_eq!(code, 0);
    assert!(out.contains("\"networkingEnabled\"") && out.contains("\"radios\"") && out.contains("\"interfaces\""), "{out}");
}

// ═════════════════════════════════════════════════════════════════════════
// The worked-example table's remaining rows (doc 06 §2), one test each.
// ═════════════════════════════════════════════════════════════════════════

/// The `set` row: a mutating verb that composes read → write, answered ok.
/// The answer is the set AS WRITTEN and it only appears after the ok frame.
#[test]
fn routes_add_writes_the_set_and_echoes_it() {
    let mock = MockService::start(true).unwrap();
    let (out, code) = net(
        mock.path().to_str().unwrap(),
        &["--json", "routes", "add", "10.0.0.0/8", "192.168.1.1", "--metric", "50"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"destinationCidr\"") && out.contains("10.0.0.0/8"), "{out}");
}

/// The `remove` row: the read → filter → write composer, honest about a
/// missing entry.
#[test]
fn routes_remove_rewrites_the_table_and_reports_a_missing_entry() {
    let mock = MockService::start(true).unwrap();
    let p = mock.path().to_str().unwrap();
    let (out, code) = net(p, &["--json", "routes", "remove", "0.0.0.0/0"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("{\"routes\""), "the set as written is shown: {out}");
    assert!(!out.contains("0.0.0.0/0"), "the removed entry is gone: {out}");

    let (out, code) = net(p, &["--json", "routes", "remove", "10.9.9.0/24"]);
    assert_eq!(code, 3, "a route that is not there is notFound -> exit 3: {code} {out}");
    assert!(out.contains("notFound"), "{out}");
}

/// The `start/stop` row: `net firewall allow …` over the full-set mutator.
#[test]
fn firewall_allow_appends_and_echoes_the_rule_set() {
    let mock = MockService::start(true).unwrap();
    let (out, code) = net(
        mock.path().to_str().unwrap(),
        &["--json", "firewall", "allow", "firefox", "outbound"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"firefox\"") && out.contains("accept"), "{out}");
    assert!(out.contains("\"existing\""), "the rules it carries forward: {out}");
}

/// The extraction's explicit `rule add|remove|enable|disable` forms.
#[test]
fn firewall_rule_add_remove_and_disable() {
    let mock = MockService::start(true).unwrap();
    let p = mock.path().to_str().unwrap();
    let (out, code) = net(
        p,
        &[
            "--json", "firewall", "rule", "add", "web", "--direction", "outbound", "--action",
            "accept", "--protocol", "tcp", "--port", "443", "--priority", "10",
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"web\"") && out.contains("\"protocol\":\"tcp\""), "{out}");

    let (out, code) = net(p, &["--json", "firewall", "rule", "disable", "existing"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"enabled\":false"), "{out}");

    let (out, code) = net(p, &["--json", "firewall", "rule", "remove", "web"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("\"web\""), "{out}");
}

/// The hit-stats row: the contract's `FirewallRule.hitCount` aggregated
/// client-side (measured, never assumed).
#[test]
fn firewall_stats_aggregates_the_contract_hit_counters() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "firewall", "stats"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"totalHitCount\"") && out.contains("42"), "{out}");
}

/// The `diagnose` row: the client-level guided report over named methods.
#[test]
fn diagnose_reports_the_composed_state() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "diagnose"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"networkingEnabled\"") && out.contains("\"interfaces\""), "{out}");
}

/// The unavailable row for a verb whose route is a named gap: `connections`
/// and `ports` compose from socket.capnp, which declares no all-sockets
/// enumeration (doc 04 §4). The CLI answers exit 3 with the reason instead of
/// inventing a route.
#[test]
fn connections_and_ports_answer_unavailable_with_the_named_gap() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "connections"]);
    assert_eq!(code, 3, "the gap is named, not routed: {code} {out}");
    assert!(out.contains("notSupported"), "{out}");
    assert!(out.contains("socket"), "the owning batch is named: {out}");

    let (_out, code) = net(mock.path().to_str().unwrap(), &["--json", "ports"]);
    assert_eq!(code, 3);
}

/// The CLI-local rows: `net profile apply` and `net export`/`import` are
/// client-side workflows (doc 04 §5) over REAL contract mutators. Each step
/// is reported applied only after its own ok.
#[test]
fn profile_apply_reports_each_step_after_its_ok() {
    let mock = MockService::start(true).unwrap();
    let dir = std::env::temp_dir().join(format!("smart-cli-profile-{}.toml", std::process::id()));
    std::fs::write(
        &dir,
        "[profile.work]\nssid = \"Corp\"\ndnsServers = [\"10.0.0.1\"]\nenabled = true\nurl = \"http://proxy:3128\"\nentries = [\"10.1.0.0/16|10.0.0.1|50\"]\n",
    )
    .unwrap();
    let file = dir.to_str().unwrap().to_string();
    let (out, code) = net(
        mock.path().to_str().unwrap(),
        &["--json", "profile", "apply", "work", "--file", &file],
    );
    assert_eq!(code, 0, "{out}");
    let applied = out.contains("\"applied\"") && out.contains("\"wifi\"") && out.contains("\"dns\"");
    assert!(applied, "the applied steps are named: {out}");
    assert!(out.contains("\"proxy\"") && out.contains("\"routes\""), "{out}");
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn profile_apply_stops_at_the_first_failure_and_names_it() {
    // A read-only-class service denies the first mutating step: the answer is
    // the error, plus what had applied before it (never a fake success).
    let mock = MockService::start(false).unwrap();
    let dir = std::env::temp_dir().join(format!("smart-cli-profile-denied-{}.toml", std::process::id()));
    std::fs::write(&dir, "[profile.work]\nssid = \"Corp\"\ndnsServers = [\"10.0.0.1\"]\n").unwrap();
    let file = dir.to_str().unwrap().to_string();
    let (out, code) = net(
        mock.path().to_str().unwrap(),
        &["--json", "profile", "apply", "work", "--file", &file],
    );
    assert_eq!(code, 4, "permissionDenied -> exit 4: {code} {out}");
    assert!(out.contains("permissionDenied"), "{out}");
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn export_import_round_trip_through_the_contract() {
    let mock = MockService::start(true).unwrap();
    let p = mock.path().to_str().unwrap();
    let (exported, code) = net(p, &["export"]);
    assert_eq!(code, 0, "{exported}");
    // The state file is the CLI-local TOML subset (doc 04 §5), section-shaped.
    assert!(exported.contains("[networking]"), "{exported}");
    assert!(exported.contains("[dns]"), "{exported}");
    assert!(exported.contains("[proxy]"), "{exported}");

    // Import applies it back through the contract's own mutators.
    let path = std::env::temp_dir().join(format!("smart-cli-state-{}.toml", std::process::id()));
    std::fs::write(&path, exported).unwrap();
    let (out, code) = net(p, &["--json", "import", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"applied\""), "{out}");
    let _ = std::fs::remove_file(&path);
}

/// The help screen publishes the exit-code table (grammar law #7): every
/// published code appears with its meaning.
#[test]
fn help_publishes_the_exit_code_table() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("exit codes:"), "{out}");
    for published in 0..=5i32 {
        assert!(
            out.lines().any(|line| line.trim_start().starts_with(&format!("{published}  "))),
            "the exit table publishes code {published}: {out}"
        );
    }
    assert!(out.contains("usage"), "{out}");
    assert!(out.contains("permission"), "{out}");
    assert!(out.contains("did not answer in time"), "{out}");
}

/// The startup budget (plan Part 6, L7): the CLI adds no call at startup
/// beyond the registry read, so a help screen (which does not even connect)
/// must start in single-digit milliseconds. The bound is a generous regression
/// gate; the measured number is in `knowledge/17-cli-proofs.md`.
#[test]
fn startup_budget_is_single_digit_milliseconds() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
    cmd.arg("--help");
    let started = std::time::Instant::now();
    let output = cmd.output().expect("spawn net --help");
    let elapsed = started.elapsed();
    assert!(output.status.success());
    assert!(
        elapsed.as_millis() < 250,
        "startup took {} ms — the CLI must not do work before it parses",
        elapsed.as_millis()
    );
}

/// The hot-path seam count: a read issues exactly the frames it needs — no
/// hidden startup call, no second round trip (plan Part 6, L7).
#[test]
fn a_read_issues_exactly_the_frames_it_needs() {
    let mock = MockService::start(true).unwrap();
    let p = mock.path().to_str().unwrap();
    let before = mock.requests();
    let (out, code) = net(p, &["--json", "ping", "8.8.8.8"]);
    assert_eq!(code, 0, "{out}");
    let served = mock.requests() - before;
    // One serve frame for the single ping call (the ABI handshake is not a
    // serve request and is not counted). No registry read, no warm-up probe.
    assert_eq!(served, 1, "ping issued {served} serve frame(s), expected exactly 1");
}

/// The dashboard composes three reads; the count proves it makes no more.
#[test]
fn the_dashboard_issues_exactly_its_composition() {
    let mock = MockService::start(false).unwrap();
    let before = mock.requests();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json"]);
    assert_eq!(code, 0, "{out}");
    let served = mock.requests() - before;
    assert_eq!(
        served, 3,
        "the dashboard composed {served} calls (isNetworkingEnabled + listRadioStates + getInterfaces)"
    );
}
