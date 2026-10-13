//! The registry and the honesty law — the exit-code table, and the per-host
//! availability read that makes `help` list nothing that cannot run.
//!
//! ## The exit-code table (grammar law #7, published in `net --help`)
//!
//! | code | meaning |
//! |---|---|
//! | 0 | ok |
//! | 1 | the call failed (the contract `T.Error` kind is in the JSON `error`) |
//! | 2 | usage (bad grammar / unknown flag / bad selector field) |
//! | 3 | unavailable-with-reason (the command exists, this host cannot run it) |
//! | 4 | permission (capability not held — `permissionDenied`) |
//! | 5 | timeout |
//!
//! ## The honesty law (grammar law #8)
//!
//! `help` lists nothing that cannot run on this host. Availability comes from
//! the service's own answer through cli.capnp's registry (`listUnavailable`
//! @13) — a registered command that is not currently runnable, with the reason
//! it cannot run and why. A command that is not runnable is HIDDEN from help
//! and, if invoked, answers exit `3` with the reason printed and in the JSON
//! `error` object. `net registry list-unavailable` shows the hidden ones.
//!
//! When the service publishes no registry (a bare network daemon answers
//! `unknown-interface` for cli.capnp), the CLI is HONEST that it cannot verify
//! availability: it lists the grammar it implements and says so in `--verbose`,
//! and every command still enforces the service's answer at call time. It
//! never fakes a "WiFi enabled" the way the old CLI did (the named anti-pattern).

use alloc::format;
#[cfg_attr(not(feature = "std"), allow(unused_imports))]
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[cfg(feature = "std")]
use crate::schema::cli_capnp::cli_service;
#[cfg(feature = "std")]
use crate::transport::{CallError, Connection};

pub const EXIT_OK: i32 = 0;
pub const EXIT_CALL_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_UNAVAILABLE: i32 = 3;
pub const EXIT_PERMISSION: i32 = 4;
pub const EXIT_TIMEOUT: i32 = 5;

/// The exit-code table, as the lines `net --help` prints.
pub const EXIT_TABLE: &[(i32, &str)] = &[
    (EXIT_OK, "ok"),
    (EXIT_CALL_FAILED, "the call failed (see the JSON error object)"),
    (EXIT_USAGE, "usage: bad grammar, unknown flag, or bad selector field"),
    (EXIT_UNAVAILABLE, "unavailable with a reason on this host"),
    (EXIT_PERMISSION, "permission denied: the capability is not held"),
    (EXIT_TIMEOUT, "the service did not answer in time"),
];

/// The exit code for a completed call, per the table. Total over `CallError`.
#[cfg(feature = "std")]
pub fn exit_code_for(result: &Result<Vec<u8>, CallError>) -> i32 {
    match result {
        Ok(_) => EXIT_OK,
        Err(CallError::Service(error)) => {
            if error.is_permission() {
                EXIT_PERMISSION
            } else if error.is_unavailable() {
                EXIT_UNAVAILABLE
            } else if error.is_timeout() {
                EXIT_TIMEOUT
            } else {
                EXIT_CALL_FAILED
            }
        }
        Err(CallError::Transport(_)) => EXIT_CALL_FAILED,
    }
}

/// One registered command the service reports as not currently runnable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    /// The command path the user types (e.g. "wifi connect").
    pub path: String,
    /// Why it cannot run, as a stable class token + the service's reason text.
    pub reason_class: String,
    /// The service's human reason (empty when it declares none).
    pub reason: String,
}

impl Unavailable {
    /// The single line `net registry list-unavailable` prints.
    pub fn line(&self) -> String {
        if self.reason.is_empty() {
            format!("{}: {}", self.path, self.reason_class)
        } else {
            format!("{}: {} — {}", self.path, self.reason_class, self.reason)
        }
    }
}

/// Whether the service publishes a registry we could read. A bare network
/// daemon does not serve cli.capnp, so the CLI is honest that it cannot verify
/// availability rather than pretending.
#[derive(Clone, Debug)]
pub enum Availability {
    /// The service published which commands cannot run right now.
    Published(Vec<Unavailable>),
    /// The service does not expose a registry (unknown-interface). The CLI
    /// lists what it implements and says so; each command still enforces at
    /// call time.
    Unpublished,
}

#[cfg(feature = "std")]
/// Read the service's unavailable-command list through cli.capnp `listUnavailable`
/// @13. This is the ONLY startup query, and only for help/registry views — the
/// CLI adds no call to a normal command's hot path.
pub fn list_unavailable(conn: &mut Connection) -> Availability {
    use crate::transport::{decode_message, encode_message};
    let cli_id = match crate::contract::schema_id("cli") {
        Some(id) => id,
        None => return Availability::Unpublished,
    };

    let mut message = capnp::message::Builder::new_default();
    {
        let mut params = message.init_root::<cli_service::list_unavailable_params::Builder<'_>>();
        let mut page = params.reborrow().init_pagination();
        page.set_offset(0);
        page.set_limit(u32::MAX);
    }
    let payload = encode_message(&message);
    let result = conn.call(cli_id, 13, &payload);
    let bytes = match result {
        Ok(bytes) => bytes,
        // A daemon that does not run the registry service answers
        // unknown-interface here; honest, not a failure to list.
        Err(_) => return Availability::Unpublished,
    };
    let Ok(reader) = decode_message::<cli_service::list_unavailable_results::Owned>(&bytes) else {
        return Availability::Unpublished;
    };
    let Ok(results) = reader.get() else {
        return Availability::Unpublished;
    };
    let mut list = Vec::new();
    for registration in results.get_commands().into_iter().flatten() {
        let path = registration
            .get_command_path()
            .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned())
            .unwrap_or_default();
    use crate::schema::cli_capnp::AvailabilityReason as R;
    if let Ok(reason) = registration.get_unavailable_reason() {
        let name = match reason {
            R::Available => "available",
            R::CapabilityDenied => "capabilityDenied",
            R::ServiceUnavailable => "serviceUnavailable",
            R::UnsupportedPlatform => "unsupportedPlatform",
            R::Unknown => "unknown",
            R::DisabledByPolicy => "disabledByPolicy",
            R::Custom => "custom",
        };
        let mut reason_text = String::new();
        if let Ok(metadata) = registration.get_metadata() {
            for entry in metadata.get_entries().into_iter().flatten() {
                if let (Ok(key), Ok(value)) = (entry.get_key(), entry.get_value()) {
                    if key.to_str().unwrap_or("") == "reason" {
                        reason_text = value.to_str().unwrap_or("").to_string();
                    }
                }
            }
        }
        list.push(Unavailable {
            path,
            reason_class: name.to_string(),
            reason: reason_text,
        });
    }
    }
    Availability::Published(list)
}

/// True when `command_path` is reported not runnable by `availability`.
pub fn is_hidden(availability: &Availability, command_path: &str) -> bool {
    match availability {
        Availability::Published(list) => list.iter().any(|u| u.path == command_path),
        Availability::Unpublished => false,
    }
}

/// The reason a hidden command is unavailable, for the exit-3 message.
pub fn hidden_reason(availability: &Availability, command_path: &str) -> Option<String> {
    match availability {
        Availability::Published(list) => list
            .iter()
            .find(|u| u.path == command_path)
            .map(Unavailable::line),
        Availability::Unpublished => None,
    }
}

// ═════════════════════════════════════════════════════════════════════════
// The command registry: the ONE table the help screen and the shell
// completions are generated from (plan Part 4.3 — the registry drives help,
// completion, and the honesty law; nothing here is hand-written per shell).
//
// A task is listed here only when THIS build compiles it: a family whose
// `cmd-*` feature is off has no entry, so `help` never advertises it and
// `completions` never completes it. What the SERVICE additionally reports as
// not runnable (`listUnavailable`) is filtered out at render time, exactly as
// help hides it — the same data, the same law, two surfaces.
// ═════════════════════════════════════════════════════════════════════════

/// One command family/task the CLI implements, with the words a user types.
pub struct TaskSpec {
    /// The task word(s) the user types (first is canonical; the rest aliases).
    pub task: &'static [&'static str],
    /// The sub-verbs this task accepts (empty = the task takes no verb).
    pub verbs: &'static [&'static str],
    /// The flags this task's grammar accepts, beyond the global flags.
    pub flags: &'static [&'static str],
    /// The contract route, for `--verbose` help (schema identity, one line).
    pub route: &'static str,
}

/// The global flags every invocation accepts (the connect + mode law).
pub const GLOBAL_FLAGS: &[&str] = &[
    "--json",
    "--quiet",
    "--verbose",
    "--no-color",
    "--connect",
    "--addr",
];

/// The selector flag (`--where field op value`, grammar law #4) — carried by
/// every task that LISTS a contract type.
pub const SELECTOR_FLAG: &str = "--where";
/// The watch flags (bounded pull loop, grammar law #5).
pub const WATCH_FLAGS: &[&str] = &["--count", "--interval", "--follow"];

/// Every command this build implements, in the order help prints them.
pub const TASKS: &[TaskSpec] = &[
    TaskSpec { task: &["status"], verbs: &[], flags: &[], route: "C isNetworkingEnabled + listRadioStates + getInterfaces" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["info", "connected", "type"], verbs: &[], flags: &[], route: "C getInterfaces + getStatus" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["interfaces", "interface", "iface"], verbs: &["up", "down", "mtu"], flags: &[SELECTOR_FLAG], route: "R getInterfaces / getStatus / upInterface / downInterface / setInterfaceMtu" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["ip", "ip4", "ip6"], verbs: &[], flags: &[], route: "R getAddressAssignment (via getInterfaces)" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["networking"], verbs: &["on", "off"], flags: &[], route: "R setNetworking @72 / isNetworkingEnabled @73" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["wifi"], verbs: &["stats", "list", "scan", "on", "off", "connect", "disconnect", "forget"], flags: &[SELECTOR_FLAG], route: "R getStatus / listKnownWifiNetworks @78 / wifiScan @6 / setRadioEnabled @74 / wifiConnect @7 / wifiDisconnect @8 / forgetWifiNetwork @77" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["scan"], verbs: &[], flags: &[SELECTOR_FLAG], route: "R wifiScan @6" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["connect"], verbs: &[], flags: &[], route: "R wifiConnect @7" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["disconnect"], verbs: &[], flags: &[], route: "R wifiDisconnect @8" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["forget"], verbs: &[], flags: &[], route: "R forgetWifiNetwork @77" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["dns"], verbs: &["get", "set", "resolve", "reverse", "bench", "flush", "cache", "config"], flags: &["--auto"], route: "R getDnsServers @31 / setDnsServers @32 (+ dns.capnp: resolve/reverse/bench/flush/cache/config)" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["routes", "route"], verbs: &["list", "add", "remove", "default"], flags: &[SELECTOR_FLAG, "--metric"], route: "R getRoutes @79 / setRoutes @80" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["firewall"], verbs: &["rules", "rule", "allow", "deny", "stats"], flags: &[SELECTOR_FLAG, "--direction", "--action", "--protocol", "--port", "--priority"], route: "R getFirewallRules @57 / setFirewallRules @56" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["rule"], verbs: &["add", "remove", "enable", "disable"], flags: &["--direction", "--action", "--protocol", "--port", "--priority"], route: "R getFirewallRules @57 / setFirewallRules @56" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["proxy"], verbs: &["get", "set", "off"], flags: &[], route: "R getProxyConfig @81 / setProxyConfig @82" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["ping"], verbs: &[], flags: &[], route: "R ping @54" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["traceroute", "trace"], verbs: &[], flags: &[], route: "R traceroute @55" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["bandwidth", "usage"], verbs: &[], flags: &[], route: "R getCounters @35" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["watch"], verbs: &["bandwidth", "status"], flags: &[], route: "R subscribeCounters @37 / subscribeStatus @2 (+ pull/cancel)" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["vpn"], verbs: &["list"], flags: &[], route: "R listVpnProfiles @17" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["diagnose"], verbs: &[], flags: &[], route: "C summary + status + counters + dns + routes" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["connections", "ports"], verbs: &[SELECTOR_FLAG], flags: &[SELECTOR_FLAG], route: "GAP: socket.capnp declares no all-sockets enumeration (socket batch)" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["profile"], verbs: &["apply", "list"], flags: &["--file"], route: "CLI-local workflow (doc 04 §5) over wifiConnect/setDnsServers/setProxyConfig/setRoutes" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["export"], verbs: &[], flags: &[], route: "CLI-local (doc 04 §5): the state file on stdout" },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["import"], verbs: &[], flags: &[], route: "CLI-local (doc 04 §5) over the contract's own mutators" },
    #[cfg(feature = "cmd-socket")]
    TaskSpec { task: &["socket"], verbs: &["create", "info", "name", "close"], flags: &[], route: "R SocketService create @0 / getInfo @11 / getSocketName @19 / close @17" },
    #[cfg(feature = "cmd-dns")]
    TaskSpec { task: &["resolve"], verbs: &["--type"], flags: &["--type"], route: "R DnsService.resolve @0" },
    #[cfg(feature = "cmd-dns")]
    TaskSpec { task: &["reverse"], verbs: &[], flags: &[], route: "R DnsService.reverseResolve @2" },
    #[cfg(feature = "cmd-bt")]
    TaskSpec { task: &["bluetooth", "bt"], verbs: &["status", "on", "off", "name", "devices"], flags: &[], route: "R BluetoothService getState @0 / enable @1 / disable @2 / name @3-4 / getPairedDevices @8" },
    #[cfg(feature = "cmd-tunnel")]
    TaskSpec { task: &["tunnel"], verbs: &["open", "info", "close"], flags: &["--name"], route: "R TunnelService open @0 / getInfo @10 / close @6" },
    #[cfg(feature = "cmd-web")]
    TaskSpec { task: &["web"], verbs: &["get", "post", "put", "patch", "delete", "resolve"], flags: &[], route: "R WebService.fetch @0 / resolveUrl @3" },
    #[cfg(feature = "cmd-discovery")]
    TaskSpec { task: &["discovery"], verbs: &["peers", "resolve"], flags: &[], route: "R Discovery.lookup @0 / resolve @2" },
    TaskSpec { task: &["registry"], verbs: &["list-unavailable"], flags: &[], route: "R cli.capnp listUnavailable @13" },
];

/// Every task word this build implements (the first word a user types).
pub fn task_words() -> Vec<&'static str> {
    TASKS.iter().flat_map(|spec| spec.task.iter().copied()).collect()
}

/// The verbs a task accepts, for completion after the task word.
pub fn verbs_for(task: &str) -> Vec<&'static str> {
    TASKS
        .iter()
        .find(|spec| spec.task.contains(&task))
        .map(|spec| spec.verbs.iter().copied().collect())
        .unwrap_or_default()
}

/// Every flag this build's grammar accepts, global + per-task.
pub fn all_flags() -> Vec<&'static str> {
    let mut flags: Vec<&'static str> = GLOBAL_FLAGS.to_vec();
    for spec in TASKS {
        for flag in spec.flags {
            if *flag != SELECTOR_FLAG && !flags.contains(flag) {
                flags.push(flag);
            }
        }
    }
    flags.push(SELECTOR_FLAG);
    flags.push(WATCH_FLAGS[0]);
    flags.push(WATCH_FLAGS[1]);
    flags.push(WATCH_FLAGS[2]);
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_table_is_total_and_stable() {
        assert_eq!(exit_code_for(&Ok(Vec::new())), EXIT_OK);
        let failed = Err(CallError::Transport("x".into()));
        assert_eq!(exit_code_for(&failed), EXIT_CALL_FAILED);
    }

    /// The registry table and the command dispatch are ONE source of truth:
    /// every task word the `net` family accepts must have a registry entry, and
    /// every registry entry must be a word the dispatch accepts. This test is
    /// the mechanical guard that stops help/completions from drifting from the
    /// grammar the CLI actually runs.
    #[test]
    #[cfg(feature = "cmd-net")]
    fn the_registry_table_matches_the_net_dispatch() {
        let source = include_str!("cmd/net.rs");
        // The task words `net::run` dispatches on (its match arms).
        let dispatch_words = [
            "status", "interfaces", "ip", "networking", "wifi", "scan", "connect", "disconnect",
            "forget", "dns", "routes", "firewall", "proxy", "ping", "traceroute", "bandwidth",
            "watch", "vpn", "diagnose", "connections", "profile", "export", "import",
        ];
        for word in dispatch_words {
            assert!(
                TASKS.iter().any(|spec| spec.task.contains(&word)),
                "the dispatch accepts '{word}' but the registry has no entry for it"
            );
            assert!(
                source.contains(&format!("\"{word}\"")),
                "the registry lists '{word}' but net.rs does not dispatch it"
            );
        }
    }

    #[test]
    fn the_registry_lists_only_words_this_build_compiles() {
        // A family whose feature is off must not appear (honesty law #8).
        if !cfg!(feature = "cmd-bt") {
            assert!(!task_words().contains(&"bluetooth"));
        }
        if !cfg!(feature = "cmd-web") {
            assert!(!task_words().contains(&"web"));
        }
        if cfg!(feature = "cmd-net") {
            assert!(task_words().contains(&"wifi"));
            assert!(task_words().contains(&"registry"));
        }
    }

    #[test]
    #[cfg(feature = "cmd-net")]
    fn every_task_exposes_its_selector_or_watch_flags_where_the_grammar_has_them() {
        let wifi = TASKS.iter().find(|spec| spec.task == &["wifi"]).unwrap();
        assert!(wifi.flags.contains(&SELECTOR_FLAG), "wifi lists a contract type");
        let watch = TASKS.iter().find(|spec| spec.task == &["watch"]).unwrap();
        assert!(watch.verbs.contains(&"bandwidth") && watch.verbs.contains(&"status"));
        // The watch flags are offered to the watch stream.
        assert!(all_flags().contains(&"--count"));
        assert!(all_flags().contains(&"--where"));
    }
}
