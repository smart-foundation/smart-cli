//! The command surface: parsing + rendering over the contract, adding NO
//! capability. Every command is `route -> call -> render/json`. Success is
//! printed only after an ok result (grammar law #8): a void command prints
//! nothing on the human path and `{}` on the JSON path, so a successful call is
//! the exit code and never a fabricated "WiFi enabled".
//!
//! ## Exit codes (the published table; see [`crate::registry`])
//! 0 ok · 1 call failed · 2 usage · 3 unavailable-with-reason · 4 permission · 5 timeout.

use alloc::string::String;
use alloc::vec::Vec;
use std::io::Write;

use crate::contract;
use crate::json;
use crate::registry::{self, Availability};
use crate::render::Style;
use crate::remote::{self, ConnectOverrides, Spawn};
use crate::transport::{CallError, Connection};

// ── sibling command families (each compiled behind its cmd-* feature) ──────
#[cfg(feature = "cmd-net")]
pub mod net;
#[cfg(feature = "cmd-dns")]
pub mod dns;
#[cfg(feature = "cmd-bt")]
pub mod bluetooth;
#[cfg(feature = "cmd-tunnel")]
pub mod tunnel;
#[cfg(feature = "cmd-web")]
pub mod web;
#[cfg(feature = "cmd-socket")]
pub mod socket;
#[cfg(feature = "cmd-discovery")]
pub mod discovery;

/// The CLI's own name, in the architecture-law sense (no "smart-lib" here).
pub const SHELL: &str = "net";

// ── The mode one invocation runs in ───────────────────────────────────────

/// The global flags one invocation carries.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    /// `--json`: contract-shaped machine output.
    pub json: bool,
    /// `--quiet`: only the result object / exit code.
    pub quiet: bool,
    /// `--verbose`: the schema identity of every call + the transport.
    pub verbose: bool,
    /// `--no-color`: force plain output (also NO_COLOR / non-TTY).
    pub no_color: bool,
}

impl Flags {
    fn present(&mut self, arg: &str) -> bool {
        match arg {
            "--json" => {
                self.json = true;
                true
            }
            "--quiet" | "-q" => {
                self.quiet = true;
                true
            }
            "--verbose" | "-v" => {
                self.verbose = true;
                true
            }
            "--no-color" | "--no-colour" => {
                self.no_color = true;
                true
            }
            _ => false,
        }
    }
}

/// Split the global flags from the command arguments (flags may appear anywere).
pub fn split_flags(argv: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags::default();
    let mut rest = Vec::new();
    for arg in argv {
        if !flags.present(arg) {
            rest.push(arg.clone());
        }
    }
    (flags, rest)
}

/// The invocation context every command runs in.
pub struct App<'a> {
    conn: &'a mut Connection,
    mode: Flags,
    style: Style,
    spawn: Spawn,
}

impl<'a> App<'a> {
    /// The JSON flag.
    pub fn json(&self) -> bool {
        self.mode.json
    }
    /// The quiet flag.
    pub fn quiet(&self) -> bool {
        self.mode.quiet
    }
    /// The verbose flag.
    pub fn verbose(&self) -> bool {
        self.mode.verbose
    }
    /// The render style (colour policy).
    pub fn style(&self) -> &Style {
        &self.style
    }
    /// The `--verbose` audit line naming one call's schema identity.
    pub fn trace(&self, interface: &str, method: &str, ordinal: u16) {
        if self.mode.verbose {
            eprintln!(
                "verbose: {}.capnp:Network.{method} @{ordinal} via {}",
                if interface == "Network" { "network" } else { interface },
                self.spawn.label()
            );
        }
    }
    /// The remote DEGRADED note, printed in `--verbose` for a TCP session.
    pub fn trace_transport(&self) {
        if self.mode.verbose {
            eprintln!("verbose: transport {}", self.spawn.label());
            if self.spawn.remote {
                eprintln!(
                    "verbose: remote session @{} — capability claims over a remote transport are weaker (DEGRADED)",
                    self.spawn.host_token.as_deref().unwrap_or("host")
                );
            }
        }
    }
    /// Raw call access (for commands that build inline helpers).
    pub fn call(&mut self, interface_id: u64, ordinal: u16, params: &[u8]) -> Result<Vec<u8>, CallError> {
        self.conn.call(interface_id, ordinal, params)
    }
    /// Raw subscription access (watch) — the connection, for pull/cancel.
    pub fn conn(&mut self) -> &mut Connection {
        self.conn
    }
}

/// A command's two renderers over the SAME result bytes.
pub struct Handler {
    /// Contract-shaped JSON from the result struct bytes.
    pub json: fn(&[u8], &mut String),
    /// Human decoration from the result struct bytes.
    pub human: fn(&[u8]) -> String,
}

/// Issue one call with the given params and render its ok result (JSON or
/// human), or emit the error and return its exit code. This is the single seam
/// where "success only after ok" is enforced: the renderers run ONLY on Ok.
pub fn go(app: &mut App, interface: &str, method: &str, ordinal: u16, params: &[u8], handler: Handler) -> i32 {
    app.trace(interface, method, ordinal);
    let interface_id = schema_id_for(interface);
    let result = app.conn.call(interface_id, ordinal, params);
    emit_result(app, result, &handler)
}

/// Issue one call and hand the ok payload to a closure renderer (the composers
/// — `routes add`, `firewall allow`, `profile apply` — need the data they just
/// wrote, which is larger than a result message). Same seam as [`go`]: the
/// closure runs ONLY on Ok, so nothing is printed without a real success.
pub fn go_with<F>(
    app: &mut App,
    interface: &str,
    method: &str,
    ordinal: u16,
    params: &[u8],
    render: F,
) -> i32
where
    F: FnOnce(&mut App, &[u8]),
{
    app.trace(interface, method, ordinal);
    let interface_id = schema_id_for(interface);
    let result = app.conn.call(interface_id, ordinal, params);
    match result {
        Ok(bytes) => {
            render(app, &bytes);
            registry::EXIT_OK
        }
        Err(error) => emit_error(app, &error),
    }
}

/// Render an already-issued call's result (the seam used by `go`).
pub fn emit_result(app: &mut App, result: Result<Vec<u8>, CallError>, handler: &Handler) -> i32 {
    match result {
        Ok(bytes) => {
            if app.json() {
                let mut out = String::new();
                (handler.json)(&bytes, &mut out);
                println!("{out}");
            } else if !app.quiet() {
                let text = (handler.human)(&bytes);
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            registry::EXIT_OK
        }
        Err(error) => emit_error(app, &error),
    }
}

/// The schema id a command's interface routes through.
pub fn schema_id_for(interface: &str) -> u64 {
    let stem = match interface {
        "Network" => "network",
        "Dns" => "dns",
        "Socket" => "socket",
        "Bluetooth" => "bluetooth",
        "Tunnel" => "tunnel",
        "Web" => "web",
        "Discovery" => "discovery",
        "Cli" => "cli",
        other => other,
    };
    contract::schema_id(stem).unwrap_or(0)
}

/// Emit an error (JSON `error` object or human stderr line) and return the exit
/// code the published table names. The old CLI's fake `wifi on` is the
/// anti-pattern this replaces: nothing prints here without a real failing call.
pub fn emit_error(app: &App, error: &CallError) -> i32 {
    match error {
        CallError::Service(service_err) => {
            let token = json::error_code(service_err.code);
            if app.json() {
                let mut out = String::new();
                json::error_object(&mut out, service_err.code, &service_err.message);
                println!("{out}");
            } else {
                eprintln!("error: {}: {}", token, service_err.message);
            }
            registry::exit_code_for(&Err(error.clone()))
        }
        CallError::Transport(detail) => {
            if app.json() {
                let mut out = String::new();
                json::write_error(
                    &mut out,
                    smart_cli_schema_types_failed(),
                    detail,
                );
                println!("{out}");
            } else {
                eprintln!("transport: {detail}");
            }
            registry::EXIT_CALL_FAILED
        }
    }
}

/// The contract's `T.ErrorCode::Failed` without importing types in every arm.
fn smart_cli_schema_types_failed() -> crate::schema::types_capnp::ErrorCode {
    crate::schema::types_capnp::ErrorCode::Failed
}

// ── Standard result handlers ──────────────────────────────────────────────

/// The void-result handler: `{}` on JSON, silent on human (exit 0 is the ack).
pub const VOID: Handler = Handler {
    json: json_void,
    human: human_void,
};

fn json_void(_bytes: &[u8], out: &mut String) {
    out.push_str("{}");
}
fn human_void(_bytes: &[u8]) -> String {
    String::new()
}

// ── The entry point ───────────────────────────────────────────────────────

/// Run one invocation to an exit code (main's whole job).
pub fn run(overrides: &ConnectOverrides) -> i32 {
    let (flags, cmd_args) = split_flags(&overrides.rest);

    // An explicit help request → the usage / help screen. A bare `net` (no
    // command) is the dashboard, handled after the transport is connected.
    if matches!(cmd_args.first().map(String::as_str), Some("help") | Some("--help") | Some("-h")) {
        return help(overrides, &flags);
    }

    let spawn = match remote::resolve(overrides) {
        Ok(spawn) => spawn,
        Err(e) => {
            usage_error(&flags, &e);
            return registry::EXIT_USAGE;
        }
    };

    let mut conn = match Connection::connect(&spawn.target) {
        Ok(conn) => conn,
        Err(error) => {
            // "No service at all" -> unavailable with a reason (Part 5); never
            // a fabricated success.
            let detail = match &error {
                CallError::Transport(d) => d.clone(),
                CallError::Service(s) => s.message.clone(),
            };
            if flags.json {
                println!(
                    "{{\"error\":{{\"kind\":\"unavailable\",\"message\":\"the service is not reachable at {}: {detail}\"}}}}",
                    spawn.label()
                );
            } else {
                eprintln!("unavailable: the service is not reachable at {}: {detail}", spawn.label());
            }
            return registry::EXIT_UNAVAILABLE;
        }
    };

    let style = if flags.no_color {
        Style::plain()
    } else if flags.json {
        Style::plain()
    } else {
        Style::from_env()
    };
    let mut app = App {
        conn: &mut conn,
        mode: flags,
        style,
        spawn,
    };
    app.trace_transport();

    // A bare `net` (no command word) is the dashboard — the composed summary.
    if cmd_args.is_empty() {
        #[cfg(feature = "cmd-net")]
        return net::run(&[], &mut app);
        #[cfg(not(feature = "cmd-net"))]
        return family_not_compiled(&flags, "net");
    }

    // The first arg selects the family. The `net` shell (this binary) takes its
    // task names directly (`net wifi on` → `wifi` is a net task reached by the
    // wildcard arm); the sibling domains are their own families (`dns resolve`,
    // `bluetooth status`). `net <task>` is also accepted explicitly.
    let family = cmd_args[0].as_str();
    let sub = &cmd_args[1..];
    match family {
        "net" if is_registry_command(sub) => registry_command(sub, &mut app),
        #[cfg(feature = "cmd-net")]
        "net" => net::run(sub, &mut app),
        #[cfg(not(feature = "cmd-net"))]
        "net" => family_not_compiled(&flags, "net"),
        "registry" if is_registry_command(sub) => registry_command(sub, &mut app),
        #[cfg(feature = "cmd-dns")]
        "dns" => dns::run(sub, &mut app),
        #[cfg(not(feature = "cmd-dns"))]
        "dns" => family_not_compiled(&flags, "dns"),
        #[cfg(feature = "cmd-bt")]
        "bluetooth" | "bt" => bluetooth::run(sub, &mut app),
        #[cfg(not(feature = "cmd-bt"))]
        "bluetooth" | "bt" => family_not_compiled(&flags, "bluetooth"),
        #[cfg(feature = "cmd-tunnel")]
        "tunnel" => tunnel::run(sub, &mut app),
        #[cfg(not(feature = "cmd-tunnel"))]
        "tunnel" => family_not_compiled(&flags, "tunnel"),
        #[cfg(feature = "cmd-web")]
        "web" => web::run(sub, &mut app),
        #[cfg(not(feature = "cmd-web"))]
        "web" => family_not_compiled(&flags, "web"),
        #[cfg(feature = "cmd-socket")]
        "socket" => socket::run(sub, &mut app),
        #[cfg(not(feature = "cmd-socket"))]
        "socket" => family_not_compiled(&flags, "socket"),
        #[cfg(feature = "cmd-discovery")]
        "discovery" => discovery::run(sub, &mut app),
        #[cfg(not(feature = "cmd-discovery"))]
        "discovery" => family_not_compiled(&flags, "discovery"),
        // The default: a net-family task (`wifi`, `interface`, `routes`,
        // `ping`, `proxy`, ...). This is how `net wifi on` works — the shell
        // is net, so an unrecognised family word is a net task.
        #[cfg(feature = "cmd-net")]
        _ => net::run(&cmd_args, &mut app),
        #[cfg(not(feature = "cmd-net"))]
        _ => {
            usage_error(&flags, &format!("unknown command '{family}' (and the net family is not compiled)"));
            registry::EXIT_USAGE
        }
    }
}

/// The honest answer for a family whose command feature is not compiled here.
/// (`dead_code` is expected: with every family on, no call site remains.)
#[allow(dead_code)]
fn family_not_compiled(flags: &Flags, family: &str) -> i32 {
    usage_error(
        flags,
        &format!("the '{family}' command family is not compiled into this build (enable it with --features cmd-{family})"),
    );
    registry::EXIT_USAGE
}

/// Whether the `net` sub-args are the registry introspection command.
fn is_registry_command(sub: &[String]) -> bool {
    matches!(sub.first().map(String::as_str), Some("registry"))
}

// ── registry command: the honesty surface ─────────────────────────────────

/// `net registry list-unavailable` — the hidden commands, with their reasons.
fn registry_command(sub: &[String], app: &mut App) -> i32 {
    match sub.get(1).map(String::as_str) {
        Some("list-unavailable") => {
            let availability = registry::list_unavailable(app.conn());
            match availability {
                Availability::Published(list) => {
                    if app.json() {
                        let mut out = String::from("{\"unavailable\":[");
                        for (i, u) in list.iter().enumerate() {
                            if i > 0 {
                                out.push(',');
                            }
                            json::write_key(&mut out, "path");
                            json::write_str(&mut out, &u.path);
                        }
                        out.push_str("]}");
                        println!("{out}");
                    } else if list.is_empty() {
                        println!("every registered command is available on this host");
                    } else {
                        for u in &list {
                            println!("{}", u.line());
                        }
                    }
                    registry::EXIT_OK
                }
                Availability::Unpublished => {
                    if app.json() {
                        println!("{{\"unavailable\":null,\"reason\":\"the service publishes no registry\"}}");
                    } else {
                        eprintln!("registry: this service publishes no availability list (a bare network daemon); honesty is enforced per-call instead");
                    }
                    registry::EXIT_OK
                }
            }
        }
        _ => {
            usage_error_app(app, "usage: net registry list-unavailable");
            registry::EXIT_USAGE
        }
    }
}

// ── help & usage: the honesty law made visible ────────────────────────────

/// The families THIS build can run (grammar law #8: help lists nothing that
/// cannot run — a family whose feature is off is not listed here, and if it is
/// typed the CLI says so with exit 2).
pub fn compiled_families() -> Vec<&'static str> {
    let mut families = Vec::new();
    if cfg!(feature = "cmd-net") {
        families.push("net");
    }
    if cfg!(feature = "cmd-dns") {
        families.push("dns");
    }
    if cfg!(feature = "cmd-bt") {
        families.push("bluetooth");
    }
    if cfg!(feature = "cmd-tunnel") {
        families.push("tunnel");
    }
    if cfg!(feature = "cmd-web") {
        families.push("web");
    }
    if cfg!(feature = "cmd-socket") {
        families.push("socket");
    }
    if cfg!(feature = "cmd-discovery") {
        families.push("discovery");
    }
    families.push("registry");
    families
}

/// The `net --help` / no-argument screen: usage, the exit-code table, and the
/// command families that can run on THIS host (hidden: the unavailable ones,
/// per grammar law #8).
pub fn help(overrides: &ConnectOverrides, flags: &Flags) -> i32 {
    let _ = overrides;
    println!("{} — the smart-protocol network client", SHELL);
    println!();
    println!("usage: {} <family> <command> [selectors] [flags]", SHELL);
    println!("       @host {} <family> <command>   # remote parity", SHELL);
    println!();
    println!("flags:");
    println!("  --json      contract-shaped machine output (keys = field names)");
    println!("  --quiet     only the result object / exit code");
    println!("  --verbose   the schema identity of every call + the transport");
    println!("  --no-color  force plain output (also $NO_COLOR / non-TTY)");
    println!("  --connect <path>   connect to a unix socket");
    println!("  --addr host:port   connect to a TCP service");
    println!();
    println!("exit codes:");
    for (code, meaning) in registry::EXIT_TABLE {
        println!("  {code}  {meaning}");
    }
    println!();
    println!("families: {}", compiled_families().join(", "));
    println!("  (a family whose feature is not compiled is not listed)");
    println!();
    println!("net tasks: status, interface(s), ip, networking, wifi, scan, connect, disconnect,");
    println!("           forget, dns, routes, firewall, proxy, ping, traceroute, bandwidth,");
    println!("           watch, vpn, diagnose, connections, ports, socket, profile, export, import");
    println!("  (selectors: --where <field> <op> <value> with ==, !=, >, <, ~= and and/or)");

    // Honesty: fetch availability and list what is hidden, when a service is
    // reachable. A CLI that cannot reach the service says so and does not lie.
    if let Ok(spawn) = remote::resolve(overrides) {
        if let Ok(mut conn) = Connection::connect(&spawn.target) {
            let mut app = App {
                conn: &mut conn,
                mode: *flags,
                style: Style::plain(),
                spawn,
            };
            let availability = registry::list_unavailable(app.conn());
            print_hidden(&availability);
        }
    }
    registry::EXIT_OK
}

/// Print the hidden commands (grammar law #8): what cannot run here, and why.
fn print_hidden(availability: &Availability) {
    match availability {
        Availability::Published(list) if !list.is_empty() => {
            println!();
            println!("not available on this host (see `{} registry list-unavailable`):", SHELL);
            for u in list {
                println!("  {}", u.line());
            }
        }
        _ => {}
    }
}

/// A usage error line (stderr human / stdout JSON).
pub fn usage_error(flags: &Flags, message: &str) {
    if flags.json {
        println!("{{\"error\":{{\"kind\":\"usage\",\"message\":{}}}}}", escape(message));
    } else {
        eprintln!("usage: {message}");
    }
    eprintln!("run `{} --help` for the grammar", SHELL);
}

/// A usage error line attached to an app (human + JSON).
fn usage_error_app(app: &App, message: &str) {
    if app.json() {
        println!("{{\"error\":{{\"kind\":\"usage\",\"message\":{}}}}}", escape(message));
    } else {
        eprintln!("usage: {message}");
    }
}

/// A minimal JSON string escape for the error paths above.
fn escape(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
