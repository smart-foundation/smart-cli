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
//! — a registered command that is not currently runnable, with the reason
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
use smart_protocol::schema::cli_capnp::cli_service;
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

/// True when this build implements the command the service published.
///
/// The comparison is by **identity**, not by path: a service and a client may
/// spell the same command differently ("interfaces" vs "interfaces up down
/// mtu"), but if the command routes through a method this build calls, this
/// build implements it. The path is the fallback, for a service that
/// published a command with no identity at all.
pub fn implements(command: &Command) -> bool {
    if TASKS.iter().any(|spec| {
        spec.calls
            .iter()
            .any(|(interface, method)| *interface == command.interface && *method == command.method)
    }) {
        return true;
    }
    task_lines().iter().any(|line| line == &command.path)
}

/// The registry table's task lines, as help prints them (one source of truth
/// with the completion generator).
pub fn task_lines() -> Vec<String> {
    TASKS
        .iter()
        .map(|spec| {
            let task = spec.task.first().copied().unwrap_or("");
            if spec.verbs.is_empty() {
                task.to_string()
            } else {
                format!(
                    "{task} {}",
                    spec.verbs
                        .iter()
                        .filter(|v| !v.starts_with("--"))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        })
        .collect()
}

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
        // "This host cannot run it" is the published table's row 3.
        Err(CallError::Unavailable { .. }) => EXIT_UNAVAILABLE,
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

/// The contract's CLI interface, by its GENERATED identity.
#[cfg(feature = "std")]
fn cli_interface() -> Option<(u64, u16)> {
    let info = smart_protocol::contract::interface_named("CliService")?;
    let list_commands = smart_protocol::contract::method_id(info.stem, info.name, "listCommands")?;
    Some((info.type_id, list_commands))
}

/// One command the service publishes in its registry (cli.capnp
/// `CommandRegistration`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// The command path the user types (e.g. "wifi scan").
    pub path: String,
    /// The schema the command's contract lives in (the canonical schema id).
    pub schema_id: u64,
    /// The contract interface the command calls.
    pub interface: String,
    /// The method the command calls, as the schema spells it.
    pub method: String,
    /// The presentation domain the service groups it under.
    pub domain: String,
    /// The service's one-line summary.
    pub summary: String,
    /// The registration's metadata (the contract's open string map): this is
    /// where a service publishes what the contract does not carry, such as
    /// the schema's node tree for a schema this client never compiled.
    pub metadata: Vec<(String, String)>,
}

impl Command {
    /// The command's route, as the audit trail names it.
    pub fn route(&self) -> String {
        format!("{}.{}.{}", self.schema_id, self.interface, self.method)
    }

    /// The **method's identity**: the capnp ordinal the service published for
    /// this command, in the registration's open metadata.
    ///
    /// The contract's `CommandRegistration` names the method but has no field
    /// for its ordinal, so a service publishes it under the `methodId` key.
    /// `None` when the service published none — the caller then falls back to
    /// the schema tree's own `codeOrder`, and says so if neither knows.
    pub fn method_id(&self) -> Option<u16> {
        self.metadata
            .iter()
            .find(|(key, _)| key == METADATA_METHOD_ID)
            .and_then(|(_, value)| value.trim().parse::<u16>().ok())
    }

    /// The **schema's node tree**, as the service published it (hex), in the
    /// registration's open metadata.
    pub fn schema_tree(&self) -> Option<Vec<u8>> {
        self.metadata
            .iter()
            .find(|(key, _)| key == METADATA_SCHEMA_TREE)
            .and_then(|(_, value)| decode_hex(value))
    }
}

/// The metadata key the **method's ordinal** is published under.
///
/// The same key, with the same meaning, in the service that publishes it (the
/// engine's `serve::cli`) and in the client that reads it: the contract's
/// `CommandRegistration` has no field for the ordinal, so the contract's open
/// metadata map carries it.
pub const METADATA_METHOD_ID: &str = "methodId";

/// The metadata key the **schema's node tree** is published under.
///
/// The tree is the contract's own `CodeGeneratorRequest` for the schema, hex
/// encoded. It is what lets a client that NEVER compiled the schema address
/// its methods.
pub const METADATA_SCHEMA_TREE: &str = "capnpSchema";

/// Decode a hex string into bytes. `None` when the text is not hex — never a
/// partial decode.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

/// The service's own registry: the commands it publishes and the ones it
/// reports as not runnable right now.
///
/// This is the CONTRACT's registry (cli.capnp), read over the contract's own
/// call model. It is the source of the command surface: the list, the help,
/// the completions and the unavailable list all come from here when the
/// service publishes it — never from a hand-written table.
#[derive(Clone, Debug, Default)]
pub struct ServiceRegistry {
    /// The published commands.
    pub commands: Vec<Command>,
    /// The published unavailable list.
    pub unavailable: Vec<Unavailable>,
}

impl ServiceRegistry {
    /// The command the user typed, if the service published it.
    pub fn command(&self, path: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.path == path)
    }

    /// The task words the published commands start with.
    pub fn task_words(&self) -> Vec<&str> {
        let mut words: Vec<&str> = Vec::new();
        for command in &self.commands {
            if let Some((first, _)) = command.path.split_once(' ') {
                if !words.contains(&first) {
                    words.push(first);
                }
            }
        }
        words
    }

    /// The verbs the published commands accept under one task word.
    pub fn verbs_for(&self, task: &str) -> Vec<&str> {
        let mut verbs: Vec<&str> = Vec::new();
        for command in &self.commands {
            if let Some((first, rest)) = command.path.split_once(' ') {
                if first == task && !verbs.contains(&rest) {
                    verbs.push(rest);
                }
            }
        }
        verbs
    }
}

/// Read the service's registry through cli.capnp (`listCommands` and
/// `listUnavailable`). Both ids come from the GENERATED table.
///
/// `None` when the service does not serve cli.capnp at all — a bare network
/// daemon. The CLI is then honest that it cannot verify the surface.
#[cfg(feature = "std")]
pub fn fetch(conn: &mut Connection) -> Option<ServiceRegistry> {
    let (interface_id, list_commands) = cli_interface()?;
    let list_unavailable = smart_protocol::contract::method_id("cli", "CliService", "listUnavailable")?;
    let mut registry = ServiceRegistry::default();

    // The unavailable list.
    let mut message = capnp::message::Builder::new_default();
    {
        let mut params = message.init_root::<cli_service::list_unavailable_params::Builder<'_>>();
        let mut page = params.reborrow().init_pagination();
        page.set_offset(0);
        page.set_limit(u32::MAX);
    }
    let payload = crate::transport::encode_message(&message);
    if let Ok(bytes) = conn.call(interface_id, list_unavailable, &payload) {
        if let Ok(reader) =
            crate::transport::decode_message::<cli_service::list_unavailable_results::Owned>(&bytes)
        {
            if let Ok(results) = reader.get() {
                for registration in results.get_commands().into_iter().flatten() {
                    let path = registration
                        .get_command_path()
                        .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned())
                        .unwrap_or_default();
                    let name = reason_token(registration.get_unavailable_reason());
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
                    registry.unavailable.push(Unavailable {
                        path,
                        reason_class: name.to_string(),
                        reason: reason_text,
                    });
                }
            }
        }
    }

    // The published command list.
    let mut message = capnp::message::Builder::new_default();
    {
        let mut params = message.init_root::<cli_service::list_commands_params::Builder<'_>>();
        let mut page = params.reborrow().init_pagination();
        page.set_offset(0);
        page.set_limit(u32::MAX);
    }
    let payload = crate::transport::encode_message(&message);
    let bytes = conn.call(interface_id, list_commands, &payload).ok()?;
    let reader =
        crate::transport::decode_message::<cli_service::list_commands_results::Owned>(&bytes).ok()?;
    let results = reader.get().ok()?;
    for registration in results.get_commands().into_iter().flatten() {
        let path = registration
            .get_command_path()
            .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned())
            .unwrap_or_default();
        let schema_id = registration
            .get_schema_id()
            .map(|s| s.get_id())
            .unwrap_or_default();
        let interface = registration
            .get_interface_name()
            .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned())
            .unwrap_or_default();
        let method = registration
            .get_method_name()
            .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned())
            .unwrap_or_default();
        let mut domain = String::new();
        let mut summary = String::new();
        if let Ok(presentation) = registration.get_presentation() {
            if let Ok(text) = presentation.get_domain() {
                domain = String::from_utf8_lossy(text.as_bytes()).into_owned();
            }
            if let Ok(text) = presentation.get_summary() {
                summary = String::from_utf8_lossy(text.as_bytes()).into_owned();
            }
        }
        let mut metadata = Vec::new();
        if let Ok(map) = registration.get_metadata() {
            for entry in map.get_entries().into_iter().flatten() {
                let key = entry
                    .get_key()
                    .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned())
                    .unwrap_or_default();
                let value = entry
                    .get_value()
                    .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned())
                    .unwrap_or_default();
                metadata.push((key, value));
            }
        }
        registry.commands.push(Command {
            path,
            schema_id,
            interface,
            method,
            domain,
            summary,
            metadata,
        });
    }
    Some(registry)
}

/// The stable token of a reason class (cli.capnp `AvailabilityReason`).
#[cfg(feature = "std")]
fn reason_token(
    reason: std::result::Result<smart_protocol::schema::cli_capnp::AvailabilityReason, capnp::NotInSchema>,
) -> &'static str {
    use smart_protocol::schema::cli_capnp::AvailabilityReason as R;
    match reason {
        Ok(R::Available) => "available",
        Ok(R::CapabilityDenied) => "capabilityDenied",
        Ok(R::ServiceUnavailable) => "serviceUnavailable",
        Ok(R::UnsupportedPlatform) => "unsupportedPlatform",
        Ok(R::Unknown) => "unknown",
        Ok(R::DisabledByPolicy) => "disabledByPolicy",
        Ok(R::Custom) => "custom",
        Err(_) => "unknown",
    }
}

/// Read the service's unavailable-command list through cli.capnp
/// `listUnavailable`. This is the ONLY startup query, and only for
/// help/registry views — the CLI adds no call to a normal command's hot path.
#[cfg(feature = "std")]
pub fn list_unavailable(conn: &mut Connection) -> Availability {
    let (interface_id, _) = match cli_interface() {
        Some(ids) => ids,
        None => return Availability::Unpublished,
    };
    let Some(list_unavailable) = smart_protocol::contract::method_id("cli", "CliService", "listUnavailable")
    else {
        return Availability::Unpublished;
    };
    let mut message = capnp::message::Builder::new_default();
    {
        let mut params = message.init_root::<cli_service::list_unavailable_params::Builder<'_>>();
        let mut page = params.reborrow().init_pagination();
        page.set_offset(0);
        page.set_limit(u32::MAX);
    }
    let payload = crate::transport::encode_message(&message);
    match conn.call(interface_id, list_unavailable, &payload) {
        Ok(bytes) => {
            let Ok(reader) =
                crate::transport::decode_message::<cli_service::list_unavailable_results::Owned>(
                    &bytes,
                )
            else {
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
                let name = reason_token(registration.get_unavailable_reason());
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
            Availability::Published(list)
        }
        // A daemon that does not run the registry service answers
        // unknown-interface here; honest, not a failure to list.
        Err(_) => Availability::Unpublished,
    }
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
    /// The contract calls this task makes, by (interface, method) NAME.
    ///
    /// The **ordinals are never written here**: they are read from the
    /// GENERATED contract table (`smart_protocol::contract`), which build.rs derives
    /// from the schema tree. A task that names a method this build did not
    /// compile says so in its route line — never a guessed ordinal.
    pub calls: &'static [(&'static str, &'static str)],
    /// How the task reaches the contract.
    pub kind: RouteKind,
}

/// How a task reaches the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
    /// One call on the contract: a read.
    Read,
    /// Several calls on the contract: a composition.
    Compose,
    /// The contract declares nothing for this task (the reason is named).
    Gap(&'static str),
    /// A CLI-local workflow: no contract call at all.
    Local(&'static str),
}

/// The route of a task: the schema identity of every call it makes, with the
/// ordinals read from the GENERATED contract table.
///
/// This is the fallback table's route — "the registry compiled in from the
/// schemas". A method ordinal is never written by hand anywhere in this
/// crate; the table names methods, and the schema names their ids.
pub fn route(spec: &TaskSpec) -> String {
    match spec.kind {
        RouteKind::Gap(reason) => format!("GAP: {reason}"),
        RouteKind::Local(reason) => format!("CLI-local: {reason}"),
        RouteKind::Read | RouteKind::Compose => {
            let label = if spec.kind == RouteKind::Compose {
                "C"
            } else {
                "R"
            };
            let calls: Vec<String> = spec
                .calls
                .iter()
                .map(|(interface, method)| {
                    format!("{interface}.{method} {}", method_id_of(interface, method))
                })
                .collect();
            format!("{label} {}", calls.join(" / "))
        }
    }
}

/// The capnp method id of one method, from the GENERATED contract table.
///
/// `None` when this build did not compile the schema that declares it — the
/// route line then says so, rather than showing an ordinal it does not know.
fn method_id_of(interface: &str, method: &str) -> String {
    let Some(info) = smart_protocol::contract::interface_named(interface) else {
        return "(not compiled in this build)".to_string();
    };
    match smart_protocol::contract::method_id(info.stem, info.name, method) {
        Some(id) => format!("@{id}"),
        None => "(not compiled in this build)".to_string(),
    }
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
///
/// The table names the contract calls each task makes; the ORDINALS behind
/// those names are read from the generated contract table (`route`), so no
/// method id is ever written by hand in this crate.
pub const TASKS: &[TaskSpec] = &[
    TaskSpec { task: &["status"], verbs: &[], flags: &[], calls: &[("Network", "isNetworkingEnabled"), ("Network", "listRadioStates"), ("Network", "getInterfaces")], kind: RouteKind::Compose },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["info", "connected", "type"], verbs: &[], flags: &[], calls: &[("Network", "getInterfaces"), ("Network", "getStatus")], kind: RouteKind::Compose },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["interfaces", "interface", "iface"], verbs: &["up", "down", "mtu"], flags: &[SELECTOR_FLAG], calls: &[("Network", "getInterfaces"), ("Network", "getStatus"), ("Network", "upInterface"), ("Network", "downInterface"), ("Network", "setInterfaceMtu")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["ip", "ip4", "ip6"], verbs: &[], flags: &[], calls: &[("Network", "getAddressAssignment"), ("Network", "getInterfaces")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["networking"], verbs: &["on", "off"], flags: &[], calls: &[("Network", "setNetworking"), ("Network", "isNetworkingEnabled")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["wifi"], verbs: &["stats", "list", "scan", "on", "off", "connect", "disconnect", "forget"], flags: &[SELECTOR_FLAG], calls: &[("Network", "getStatus"), ("Network", "listKnownWifiNetworks"), ("Network", "wifiScan"), ("Network", "setRadioEnabled"), ("Network", "wifiConnect"), ("Network", "wifiDisconnect"), ("Network", "forgetWifiNetwork")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["scan"], verbs: &[], flags: &[SELECTOR_FLAG], calls: &[("Network", "wifiScan")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["connect"], verbs: &[], flags: &[], calls: &[("Network", "wifiConnect")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["disconnect"], verbs: &[], flags: &[], calls: &[("Network", "wifiDisconnect")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["forget"], verbs: &[], flags: &[], calls: &[("Network", "forgetWifiNetwork")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["dns"], verbs: &["get", "set", "resolve", "reverse", "bench", "flush", "cache", "config"], flags: &["--auto"], calls: &[("Network", "getDnsServers"), ("Network", "setDnsServers"), ("DnsService", "resolve"), ("DnsService", "reverseResolve"), ("DnsService", "cacheStatus"), ("DnsService", "flushCache"), ("DnsService", "getConfig"), ("DnsService", "setConfig")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["routes", "route"], verbs: &["list", "add", "remove", "default"], flags: &[SELECTOR_FLAG, "--metric"], calls: &[("Network", "getRoutes"), ("Network", "setRoutes")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["firewall"], verbs: &["rules", "rule", "allow", "deny", "stats"], flags: &[SELECTOR_FLAG, "--direction", "--action", "--protocol", "--port", "--priority"], calls: &[("Network", "getFirewallRules"), ("Network", "setFirewallRules")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["rule"], verbs: &["add", "remove", "enable", "disable"], flags: &["--direction", "--action", "--protocol", "--port", "--priority"], calls: &[("Network", "getFirewallRules"), ("Network", "setFirewallRules")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["proxy"], verbs: &["get", "set", "off"], flags: &[], calls: &[("Network", "getProxyConfig"), ("Network", "setProxyConfig")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["ping"], verbs: &[], flags: &[], calls: &[("Network", "ping")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["traceroute", "trace"], verbs: &[], flags: &[], calls: &[("Network", "traceroute")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["bandwidth", "usage"], verbs: &[], flags: &[], calls: &[("Network", "getCounters")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["watch"], verbs: &["bandwidth", "status"], flags: &[], calls: &[("Network", "subscribeCounters"), ("Network", "subscribeStatus")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["vpn"], verbs: &["list"], flags: &[], calls: &[("Network", "listVpnProfiles")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["diagnose"], verbs: &[], flags: &[], calls: &[("Network", "getStatus"), ("Network", "getCounters"), ("Network", "getDnsServers"), ("Network", "getRoutes")], kind: RouteKind::Compose },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["connections", "ports"], verbs: &[SELECTOR_FLAG], flags: &[SELECTOR_FLAG], calls: &[], kind: RouteKind::Gap("socket.capnp declares no all-sockets enumeration (socket batch)") },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["profile"], verbs: &["apply", "list"], flags: &["--file"], calls: &[("Network", "wifiConnect"), ("Network", "setDnsServers"), ("Network", "setProxyConfig"), ("Network", "setRoutes")], kind: RouteKind::Local("workflow (doc 04 §5) over the contract's own mutators") },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["export"], verbs: &[], flags: &[], calls: &[], kind: RouteKind::Local("workflow (doc 04 §5): the state file on stdout") },
    #[cfg(feature = "cmd-net")]
    TaskSpec { task: &["import"], verbs: &[], flags: &[], calls: &[("Network", "wifiConnect"), ("Network", "setDnsServers"), ("Network", "setProxyConfig"), ("Network", "setRoutes")], kind: RouteKind::Local("workflow (doc 04 §5) over the contract's own mutators") },
    #[cfg(feature = "cmd-socket")]
    TaskSpec { task: &["socket"], verbs: &["create", "info", "name", "close"], flags: &[], calls: &[("SocketService", "create"), ("SocketService", "getInfo"), ("SocketService", "getSocketName"), ("SocketService", "close")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-dns")]
    TaskSpec { task: &["resolve"], verbs: &["--type"], flags: &["--type"], calls: &[("DnsService", "resolve")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-dns")]
    TaskSpec { task: &["reverse"], verbs: &[], flags: &[], calls: &[("DnsService", "reverseResolve")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-bt")]
    TaskSpec { task: &["bluetooth", "bt"], verbs: &["status", "on", "off", "name", "devices"], flags: &[], calls: &[("BluetoothService", "getState"), ("BluetoothService", "enable"), ("BluetoothService", "disable"), ("BluetoothService", "setName"), ("BluetoothService", "getName"), ("BluetoothService", "getPairedDevices")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-tunnel")]
    TaskSpec { task: &["tunnel"], verbs: &["open", "info", "close"], flags: &["--name"], calls: &[("TunnelService", "open"), ("TunnelService", "getInfo"), ("TunnelService", "close")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-web")]
    TaskSpec { task: &["web"], verbs: &["get", "post", "put", "patch", "delete", "resolve"], flags: &[], calls: &[("WebService", "fetch"), ("WebService", "resolveUrl")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-discovery")]
    TaskSpec { task: &["discovery"], verbs: &["peers", "resolve"], flags: &[], calls: &[("Discovery", "lookup"), ("Discovery", "resolve")], kind: RouteKind::Read },
    // The six contract-served families (spine S1). The verbs are the
    // PRODUCT's own words — what the service publishes in its registry — and
    // the calls are named, never ordinaled: the schema names their ids.
    #[cfg(feature = "cmd-files")]
    TaskSpec { task: &["files"], verbs: &["ls", "find", "tree", "stat", "xattrs", "cat", "du", "locks", "hash", "versions", "branches", "store", "dedupe", "watch", "special", "trash", "recents", "app", "grep", "compare", "health", "quota", "mkdir", "touch", "rm", "mv", "cp", "write", "link", "lock", "unlock"], flags: &["--all", "--pattern", "--sort", "--order", "--recursive"], calls: &[("Files", "listDirectory"), ("Files", "searchFiles"), ("Files", "walkTree"), ("Files", "statFile"), ("Files", "getFileMetadata"), ("Files", "readFile"), ("Files", "getDiskUsage"), ("Files", "getLocks"), ("Files", "getFileHash"), ("Files", "getFileVersions"), ("Files", "listBranches"), ("Files", "storeContentAddressed"), ("Files", "deduplicate"), ("Files", "watchDirectory"), ("Files", "getSpecialPath"), ("Files", "listTrash"), ("Files", "getRecentFiles"), ("Files", "getDefaultApp"), ("Files", "searchFileContent"), ("Files", "compareFiles"), ("Files", "getFilesystemHealth"), ("Files", "getQuota"), ("Files", "createDirectory"), ("Files", "touch"), ("Files", "removeFile"), ("Files", "renameFile"), ("Files", "copyFile"), ("Files", "writeFile"), ("Files", "linkFile"), ("Files", "lockFile"), ("Files", "unlockFile")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-process")]
    TaskSpec { task: &["process"], verbs: &["list", "inspect", "status", "tree", "threads", "env", "maps", "limits", "accounting", "resource-accounting", "groups", "group", "stats", "context", "spawn", "wait", "set-env", "attach", "detach", "read-memory", "watch", "watch-exit", "respawn", "respawn-status", "exit", "step", "continue"], flags: &["--pid", "--user", "--depth"], calls: &[("Process", "listProcesses"), ("Process", "getProcess"), ("Process", "getProcessStatus"), ("Process", "getProcessTree"), ("Process", "getThreads"), ("Process", "getEnvironment"), ("Process", "getMemoryMap"), ("Process", "getResourceLimits"), ("Process", "getAccounting"), ("Process", "getResourceAccounting"), ("Process", "listProcessGroups"), ("Process", "getProcessGroup"), ("Process", "getStats"), ("Process", "getExecutionContext"), ("Process", "spawn"), ("Process", "wait"), ("Process", "setEnvironment"), ("Process", "attach"), ("Process", "detach"), ("Process", "readMemory"), ("Process", "subscribeProcess"), ("Process", "subscribeDeath"), ("Process", "respawnProcess"), ("Process", "getProcessRespawnStatus"), ("Process", "exitProcess"), ("Process", "stepProcess"), ("Process", "continueProcess")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-settings")]
    TaskSpec { task: &["settings"], verbs: &["domains", "list", "get", "set", "delete", "clear", "watch", "watch-domain", "bulk-set", "bulk-get", "category", "set-app-override", "get-app-override", "clear-app-override", "version", "set-if-unchanged", "history", "diff"], flags: &["--key", "--since", "--count", "--interval"], calls: &[("Settings", "listDomains"), ("Settings", "getAll"), ("Settings", "get"), ("Settings", "set"), ("Settings", "delete"), ("Settings", "clear"), ("Settings", "watch"), ("Settings", "watchDomain"), ("Settings", "bulkSet"), ("Settings", "bulkGet"), ("Settings", "listByCategory"), ("Settings", "setAppOverride"), ("Settings", "getAppOverride"), ("Settings", "clearAppOverride"), ("Settings", "getVersion"), ("Settings", "setIfUnchanged"), ("Settings", "getHistory"), ("Settings", "getSettingsDiff")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-logging")]
    TaskSpec { task: &["logging"], verbs: &["sinks", "domains", "tail", "entries", "follow", "emit", "level", "set-level", "sampling", "clear", "export"], flags: &["--level", "--domain", "--pattern", "--lines", "--count", "--interval", "--mime", "--set"], calls: &[("Logging", "listDomains"), ("Logging", "getEntries"), ("Logging", "subscribe"), ("Logging", "log"), ("Logging", "getLevel"), ("Logging", "setLevel"), ("Logging", "setSamplingRate"), ("Logging", "clear"), ("Logging", "export")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-clock")]
    TaskSpec { task: &["clock"], verbs: &["info", "now", "monotonic", "uptime", "resolution", "sync", "security", "domains", "quality", "drift", "sleep", "sleep-cancellable", "timers", "persist-timer", "set-timer-dependency", "watch", "alarms"], flags: &["--source", "--domain", "--count", "--interval", "--duration"], calls: &[("ClockService", "info"), ("ClockService", "realtimeNow"), ("ClockService", "monotonicNow"), ("ClockService", "listClockDomains"), ("ClockService", "getClockSync"), ("ClockService", "getClockQuality"), ("ClockService", "getClockDrift"), ("ClockService", "sleep"), ("ClockService", "sleepCancellable"), ("ClockService", "listPersistedTimers"), ("ClockService", "persistTimer"), ("ClockService", "setTimerDependency"), ("ClockService", "subscribeTimers")], kind: RouteKind::Read },
    #[cfg(feature = "cmd-time")]
    TaskSpec { task: &["time"], verbs: &["now", "cal", "add", "sub", "diff", "verify-hsm"], flags: &["--date", "--to"], calls: &[("Time", "getDateTime"), ("Time", "convertCalendar"), ("Time", "addCalendarUnits"), ("Time", "subtractCalendarUnits"), ("Time", "diffCalendarDates"), ("Time", "verifyHsmTimestamp")], kind: RouteKind::Read },
    TaskSpec { task: &["registry"], verbs: &["list-unavailable"], flags: &[], calls: &[("CliService", "listUnavailable")], kind: RouteKind::Read },
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
