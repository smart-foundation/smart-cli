#![cfg(feature = "cmd-dns")]
// Compiled only against a cmd-dns build (its schema + command surface).
//! The `dns` family's golden tests: doc 06's resolve / reverse / bench /
//! cache rows, driven through BOTH spellings — the `dns …` family command and
//! the `net dns …` alias that delegates to the same implementation (the
//! zero-duplication law: one route, one renderer, two namespaces).

use std::process::Command;

mod support;
use support::mock::MockService;

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

#[test]
fn dns_resolve_is_contract_shaped() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "dns", "resolve", "example.com"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"canonicalName\""), "{out}");
    assert!(out.contains("example.com"), "{out}");
    assert!(out.contains("\"answers\""), "{out}");
}

/// `net dns <verb>` (the net grammar's spelling of the resolver surface) works
/// when both families are compiled; in a cmd-dns-only build it is answered
/// honestly by the family-not-compiled path instead.
#[cfg(all(feature = "cmd-dns", feature = "cmd-net"))]
#[test]
fn net_dns_resolve_reaches_the_same_route() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (family, family_code) = net(p, &["--json", "dns", "resolve", "example.com"]);
    let (alias, alias_code) = net(p, &["--json", "net", "dns", "resolve", "example.com"]);
    assert_eq!(family_code, 0, "{family}");
    assert_eq!(alias_code, 0, "{alias}");
    assert_eq!(alias, family, "the `net dns …` alias is the same call, not a re-implementation");
}

#[test]
fn dns_reverse_reports_the_name() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "dns", "reverse", "1.1.1.1"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"name\"") && out.contains("one.one.one.one"), "{out}");
}

#[test]
fn dns_bench_is_the_batch_row() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "dns", "bench"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"results\""), "{out}");
}

#[test]
fn dns_cache_stats_are_contract_keys() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "dns", "cache"]);
    assert_eq!(code, 0, "{out}");
    for key in ["hits", "misses", "size", "evictions"] {
        assert!(out.contains(&format!("\"{key}\"")), "{key} is missing: {out}");
    }
}

#[test]
fn dns_config_reports_the_transport_token() {
    let mock = MockService::start(false).unwrap();
    let (out, code) = net(mock.path().to_str().unwrap(), &["--json", "dns", "config"]);
    assert_eq!(code, 0, "{out}");
    // DoH/DoT are VALUES in the contract's DnsConfig, not booleans (doc 04 §3).
    assert!(out.contains("\"defaultTransport\""), "{out}");
}

/// The network family's `dns get|set` rows (`getDnsServers`/`setDnsServers`),
/// which live in the net grammar and therefore need both families compiled.
#[cfg(all(feature = "cmd-dns", feature = "cmd-net"))]
#[test]
fn dns_get_and_set_are_the_network_family_rows() {
    let mock = MockService::start(true).unwrap();
    let p = mock.path().to_str().unwrap();
    let (servers, code) = net(p, &["--json", "net", "dns", "get"]);
    assert_eq!(code, 0, "{servers}");
    assert!(servers.contains("1.1.1.1"), "{servers}");

    let (out, code) = net(p, &["--json", "net", "dns", "set", "9.9.9.9"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "{}", "the void mutator answers {{}}: {out}");
}

/// The honesty law across feature configs: in a build WITHOUT the net family,
/// `net …` is not advertised (see `--help`) and typing it answers exit 2
/// naming the feature to enable — never a silent no-op.
#[test]
fn an_uncompiled_family_says_so_instead_of_doing_nothing() {
    let mock = MockService::start(false).unwrap();
    let p = mock.path().to_str().unwrap();
    let (out, err, code) = {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_net"));
        cmd.arg("--connect").arg(p).arg("--json").arg("wifi").arg("list");
        let output = cmd.output().expect("spawn net");
        (
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
            output.status.code().unwrap_or(-1),
        )
    };
    // Only asserted in a build where the net family is genuinely absent.
    if cfg!(feature = "cmd-net") {
        return;
    }
    assert_eq!(code, 2, "{out} {err}");
    assert!(out.contains("not compiled"), "the message says the family is absent: {out}");
}
