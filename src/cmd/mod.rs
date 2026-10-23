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

use crate::completions;
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
// The registry-driven dispatch the six contract-served families share.
#[cfg(any(
    feature = "cmd-files",
    feature = "cmd-process",
    feature = "cmd-settings",
    feature = "cmd-logging",
    feature = "cmd-clock",
    feature = "cmd-time",
    feature = "cmd-clipboard",
    feature = "cmd-audio",
    feature = "cmd-media",
    feature = "cmd-power",
    feature = "cmd-database",
    feature = "cmd-queue",
    feature = "cmd-appindex",
    feature = "cmd-manifest",
    feature = "cmd-vcard",
    feature = "cmd-content"
))]
pub mod domain;
#[cfg(feature = "cmd-files")]
pub mod files;
#[cfg(feature = "cmd-process")]
pub mod process;
#[cfg(feature = "cmd-settings")]
pub mod settings;
#[cfg(feature = "cmd-logging")]
pub mod logging;
#[cfg(feature = "cmd-clock")]
pub mod clock;
#[cfg(feature = "cmd-time")]
pub mod time;
#[cfg(feature = "cmd-clipboard")]
pub mod clipboard;
#[cfg(feature = "cmd-audio")]
pub mod audio;
#[cfg(feature = "cmd-media")]
pub mod media;
#[cfg(feature = "cmd-power")]
pub mod power;
#[cfg(feature = "cmd-database")]
pub mod database;
#[cfg(feature = "cmd-queue")]
pub mod queue;
#[cfg(feature = "cmd-appindex")]
pub mod appindex;
#[cfg(feature = "cmd-manifest")]
pub mod manifest;
#[cfg(feature = "cmd-vcard")]
pub mod vcard;
#[cfg(feature = "cmd-content")]
pub mod content;

/// The CLI's own name, in the architecture-law sense (no "an implementation crate" here).
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
    ///
    /// The schema stem, the interface, the method and the method id are all
    /// GENERATED (see [`crate::contract`]) — the audit trail cannot drift
    /// from the contract.
    pub fn trace(&self, target: &Target, method: &str) {
        if self.mode.verbose {
            eprintln!(
                "verbose: {}.capnp:{}.{method} @{} via {}",
                target.interface.stem,
                target.interface.name,
                target.method_id,
                self.spawn.label()
            );
        }
    }
    /// The audit line for one call named by the contract (the ids come from
    /// the GENERATED table).
    pub fn trace_named(&self, interface: &str, method: &str) {
        if let Some(target) = target(interface, method) {
            self.trace(&target, method);
        }
    }
    /// The audit line for a call the CLI made through the GENERIC path (a
    /// schema this build never compiled): the identity is the service's.
    pub fn trace_generic(&self, domain: &str, interface: &str, method: &str, method_id: u16) {
        if self.mode.verbose {
            eprintln!(
                "verbose: {domain}.capnp:{interface}.{method} @{method_id} via {} (generic path)",
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
    pub fn call(&mut self, interface_id: u64, method_id: u16, params: &[u8]) -> Result<Vec<u8>, CallError> {
        self.conn.call(interface_id, method_id, params)
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

/// A call target: the contract interface and the method, resolved from the
/// GENERATED tables.
///
/// The interface's **type id** and the method's **id** both come from the
/// contract: the type id from the generated bindings, the method id from the
/// ordinal the schema declares. No ordinal is written by hand anywhere in
/// this crate.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    /// The contract interface the call names.
    pub interface: contract::InterfaceInfo,
    /// The method's capnp id.
    pub method_id: u16,
}

/// Resolve a command's call target by the names the schema gives them.
///
/// `None` means the contract has no such method in this build — an honest
/// "this build cannot", never a fabricated call.
pub fn target(interface: &str, method: &str) -> Option<Target> {
    let info = contract::interface_named(interface)?;
    let method_id = contract::method_id(info.stem, info.name, method)?;
    Some(Target {
        interface: info,
        method_id,
    })
}

/// Resolve a call target by the SCHEMA STEM and the names the schema gives
/// them — the explicit form, for a family whose interface name another schema
/// could also declare. The ids still come from the GENERATED table.
pub fn target_in(stem: &str, interface: &str, method: &str) -> Option<Target> {
    let info = contract::interface_by_name(stem, interface)?;
    let method_id = contract::method_id(info.stem, info.name, method)?;
    Some(Target {
        interface: info,
        method_id,
    })
}

/// Issue one call by NAME in a NAMED schema, without a renderer (the
/// composers that need the bytes they just wrote).
pub fn call_in(
    app: &mut App,
    stem: &str,
    interface: &str,
    method: &str,
    params: &[u8],
) -> Result<Vec<u8>, CallError> {
    let target = target_in(stem, interface, method).ok_or_else(|| {
        CallError::Transport(format!(
            "the contract has no method `{stem}.{interface}.{method}` in this build"
        ))
    })?;
    app.trace(&target, method);
    app.conn
        .call(target.interface.type_id, target.method_id, params)
}

/// Issue one call by NAME (the ids come from the GENERATED tables) and render
/// its ok result (JSON or human), or emit the error and return its exit code.
/// This is the single seam where "success only after ok" is enforced: the
/// renderers run ONLY on Ok.
pub fn go(app: &mut App, interface: &str, method: &str, params: &[u8], handler: Handler) -> i32 {
    let Some(target) = target(interface, method) else {
        usage_error_app(
            app,
            &format!("the contract has no method `{interface}.{method}` in this build"),
        );
        return registry::EXIT_USAGE;
    };
    app.trace(&target, method);
    let result = app
        .conn
        .call(target.interface.type_id, target.method_id, params);
    emit_result(app, result, &handler)
}

/// The interface id and the method id of one contract method (both GENERATED).
pub fn ids(interface: &str, method: &str) -> Option<(u64, u16)> {
    let target = target(interface, method)?;
    Some((target.interface.type_id, target.method_id))
}

/// The ids of one call in a NAMED schema — the explicit form, for a family
/// whose interface name another schema could also declare.
pub fn ids_in(stem: &str, interface: &str, method: &str) -> Option<(u64, u16)> {
    let target = target_in(stem, interface, method)?;
    Some((target.interface.type_id, target.method_id))
}

/// Issue one call by NAME, without a renderer (for the composers that need
/// the bytes they just wrote).
pub fn call(app: &mut App, interface: &str, method: &str, params: &[u8]) -> Result<Vec<u8>, CallError> {
    let target = target(interface, method).ok_or_else(|| {
        CallError::Transport(format!(
            "the contract has no method `{interface}.{method}` in this build"
        ))
    })?;
    app.conn
        .call(target.interface.type_id, target.method_id, params)
}

/// Issue one call and hand the ok payload to a closure renderer (the composers
/// — `routes add`, `firewall allow`, `profile apply` — need the data they just
/// wrote, which is larger than a result message). Same seam as [`go`]: the
/// closure runs ONLY on Ok, so nothing is printed without a real success.
pub fn go_with<F>(
    app: &mut App,
    interface: &str,
    method: &str,
    params: &[u8],
    render: F,
) -> i32
where
    F: FnOnce(&mut App, &[u8]),
{
    let Some(target) = target(interface, method) else {
        usage_error_app(
            app,
            &format!("the contract has no method `{interface}.{method}` in this build"),
        );
        return registry::EXIT_USAGE;
    };
    app.trace(&target, method);
    let result = app
        .conn
        .call(target.interface.type_id, target.method_id, params);
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
        CallError::Unavailable { what, reason } => {
            // "This host cannot run it" — the published table's row 3, with
            // the reason the caller was given.
            if app.json() {
                let mut out = String::new();
                json::write_error(&mut out, smart_cli_schema_types_unavailable(), reason);
                println!("{out}");
            } else {
                eprintln!("unavailable: {what} is unavailable: {reason}");
            }
            registry::EXIT_UNAVAILABLE
        }
    }
}

/// The contract's `T.ErrorCode::Failed` without importing types in every arm.
fn smart_cli_schema_types_failed() -> crate::schema::types_capnp::ErrorCode {
    crate::schema::types_capnp::ErrorCode::Failed
}

/// The contract `T.ErrorCode::Unavailable` without importing types in every arm.
fn smart_cli_schema_types_unavailable() -> crate::schema::types_capnp::ErrorCode {
    crate::schema::types_capnp::ErrorCode::Unavailable
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

    // Completions and their installation are LOCAL views: the registry this
    // build implements is the source, so they work with no service at all.
    // When a service IS reachable, they consult its availability answer as the
    // honesty filter (exactly what `--help` does); when it is not, they say
    // availability is UNVERIFIED rather than pretending everything runs.
    if matches!(cmd_args.first().map(String::as_str), Some("completions") | Some("install")) {
        let family = cmd_args[0].clone();
        let sub = &cmd_args[1..];
        // The SERVICE's registry: what the service publishes (the truth about
        // the host) and what it reports unavailable (the honesty filter).
        let mut service: Option<registry::ServiceRegistry> = None;
        let availability = remote::resolve(overrides)
            .ok()
            .and_then(|spawn| Connection::connect(&spawn.endpoint).ok())
            .map(|mut conn| {
                service = registry::fetch(&mut conn);
                registry::list_unavailable(&mut conn)
            })
            .unwrap_or(registry::Availability::Unpublished);
        return match family.as_str() {
            "completions" => completions_command(sub, &flags, &availability, service.as_ref()),
            _ => install_command(sub, &flags, &availability, service.as_ref()),
        };
    }

    let spawn = match remote::resolve(overrides) {
        Ok(spawn) => spawn,
        Err(e) => {
            usage_error(&flags, &e);
            return registry::EXIT_USAGE;
        }
    };

    let mut conn = match Connection::connect(&spawn.endpoint) {
        Ok(conn) => conn,
        Err(error) => {
            // "No service at all" -> unavailable with a reason (Part 5); never
            // a fabricated success.
            let detail = match &error {
                CallError::Transport(d) => d.clone(),
                CallError::Service(s) => s.message.clone(),
                CallError::Unavailable { what, reason } => format!("{what} is unavailable: {reason}"),
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
        // The six contract-served families (spine S1): the path and the
        // summary come from the service's registry, the presenter is keyed by
        // (schema id, method name). A verb this build has no presenter for
        // runs through the generic path — the surface is never half-cut.
        #[cfg(feature = "cmd-files")]
        "files" => domain::dispatch("files", sub, &mut app, files::presenter),
        #[cfg(not(feature = "cmd-files"))]
        "files" => family_not_compiled(&flags, "files"),
        #[cfg(feature = "cmd-process")]
        "process" => domain::dispatch("process", sub, &mut app, process::presenter),
        #[cfg(not(feature = "cmd-process"))]
        "process" => family_not_compiled(&flags, "process"),
        #[cfg(feature = "cmd-settings")]
        "settings" => domain::dispatch("settings", sub, &mut app, settings::presenter),
        #[cfg(not(feature = "cmd-settings"))]
        "settings" => family_not_compiled(&flags, "settings"),
        #[cfg(feature = "cmd-logging")]
        "logging" => domain::dispatch("logging", sub, &mut app, logging::presenter),
        #[cfg(not(feature = "cmd-logging"))]
        "logging" => family_not_compiled(&flags, "logging"),
        #[cfg(feature = "cmd-clock")]
        "clock" => domain::dispatch("clock", sub, &mut app, clock::presenter),
        #[cfg(not(feature = "cmd-clock"))]
        "clock" => family_not_compiled(&flags, "clock"),
        #[cfg(feature = "cmd-time")]
        "time" => domain::dispatch("time", sub, &mut app, time::presenter),
        #[cfg(not(feature = "cmd-time"))]
        "time" => family_not_compiled(&flags, "time"),
        #[cfg(feature = "cmd-clipboard")]
        "clipboard" => domain::dispatch("clipboard", sub, &mut app, clipboard::presenter),
        #[cfg(not(feature = "cmd-clipboard"))]
        "clipboard" => family_not_compiled(&flags, "clipboard"),
        #[cfg(feature = "cmd-audio")]
        "audio" => domain::dispatch("audio", sub, &mut app, audio::presenter),
        #[cfg(not(feature = "cmd-audio"))]
        "audio" => family_not_compiled(&flags, "audio"),
        #[cfg(feature = "cmd-media")]
        "media" => domain::dispatch("media", sub, &mut app, media::presenter),
        #[cfg(not(feature = "cmd-media"))]
        "media" => family_not_compiled(&flags, "media"),
        #[cfg(feature = "cmd-power")]
        "power" => domain::dispatch("power", sub, &mut app, power::presenter),
        #[cfg(not(feature = "cmd-power"))]
        "power" => family_not_compiled(&flags, "power"),
        #[cfg(feature = "cmd-database")]
        "database" => domain::dispatch("database", sub, &mut app, database::presenter),
        #[cfg(not(feature = "cmd-database"))]
        "database" => family_not_compiled(&flags, "database"),
        #[cfg(feature = "cmd-queue")]
        "queue" => domain::dispatch("queue", sub, &mut app, queue::presenter),
        #[cfg(not(feature = "cmd-queue"))]
        "queue" => family_not_compiled(&flags, "queue"),
        #[cfg(feature = "cmd-appindex")]
        "appindex" => domain::dispatch("appindex", sub, &mut app, appindex::presenter),
        #[cfg(not(feature = "cmd-appindex"))]
        "appindex" => family_not_compiled(&flags, "appindex"),
        #[cfg(feature = "cmd-manifest")]
        "manifest" => domain::dispatch("manifest", sub, &mut app, manifest::presenter),
        #[cfg(not(feature = "cmd-manifest"))]
        "manifest" => family_not_compiled(&flags, "manifest"),
        #[cfg(feature = "cmd-vcard")]
        "vcard" => domain::dispatch("vcard", sub, &mut app, vcard::presenter),
        #[cfg(not(feature = "cmd-vcard"))]
        "vcard" => family_not_compiled(&flags, "vcard"),
        #[cfg(feature = "cmd-content")]
        "content" => domain::dispatch("content", sub, &mut app, content::presenter),
        #[cfg(not(feature = "cmd-content"))]
        "content" => family_not_compiled(&flags, "content"),
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

/// The **generic path**: a command the SERVICE published and this build never
/// compiled.
///
/// The registry names the method (schema id + interface + method) and
/// publishes the schema's node tree; the CLI walks that tree to address the
/// call — no compiled binding, no hand-written ordinal, no per-domain call
/// site. A service that ships a contract this CLI has never seen therefore
/// works with no CLI change at all.
///
/// The method's **identity** comes from the registration's metadata
/// (`methodId`): the contract's `CommandRegistration` names the method but
/// carries no ordinal, so the service publishes the one it resolved from the
/// schema tree. When the service published none, the schema tree's own
/// `codeOrder` is the fallback — and when neither knows, the CLI refuses
/// honestly rather than calling a guessed ordinal.
///
/// `None` means the service published no such command: the caller then
/// refuses honestly (the CLI never invents a route).
pub fn generic_command(app: &mut App, path: &str, args: &[String]) -> Option<i32> {
    let service = registry::fetch(app.conn())?;
    generic_with(app, &service, path, args)
}

/// The generic path over an ALREADY-FETCHED registry: the same call, without
/// the second registry read.
pub fn generic_with(
    app: &mut App,
    service: &registry::ServiceRegistry,
    path: &str,
    args: &[String],
) -> Option<i32> {
    let command = service.command(path)?.clone();
    let schema = command
        .metadata
        .iter()
        .find(|(key, _)| key == crate::dynamic::SCHEMA_METADATA_KEY)
        .map(|(_, value)| value.clone())?;
    let tree = crate::dynamic::SchemaTree::decode(&hex_decode(&schema)?)?;
    let interface = tree.interface(&command.interface)?.clone();
    let method = tree
        .method(&command.interface, &command.method)?
        .clone();
    // The ordinal: the service's published identity first, the schema tree's
    // own `codeOrder` as the fallback.
    let method_id = match command.method_id() {
        Some(published) => published,
        None => method.id,
    };
    let params = match crate::dynamic::build_params(&tree, &method, &key_value_args(args)) {
        Ok(params) => params,
        Err(error) => {
            usage_error_app(app, &error.message);
            return Some(registry::EXIT_USAGE);
        }
    };
    app.trace_generic(
        &command.domain,
        &command.interface,
        &command.method,
        method_id,
    );
    match app.conn.call(interface.type_id, method_id, &params) {
        Ok(bytes) => {
            let values = crate::dynamic::read_results(&tree, method.results_type_id, &bytes)
                .unwrap_or_default();
            if app.json() {
                let mut out = String::from("{");
                for (index, (name, value)) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    json::write_key(&mut out, name);
                    crate::dynamic::write_json(value, &mut out);
                }
                out.push('}');
                println!("{out}");
            } else {
                crate::dynamic::render_fields(&values);
            }
            Some(registry::EXIT_OK)
        }
        Err(error) => Some(emit_error(app, &error)),
    }
}

/// The `key=value` arguments the generic path understands.
fn key_value_args(args: &[String]) -> Vec<(String, String)> {
    args.iter()
        .filter_map(|arg| arg.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// Decode a hex string into bytes (the metadata carries the schema's node
/// tree as hex, because the contract's metadata is a string map).
fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
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

/// `net completions <shell>` — the generated completer for one shell, printed
/// to stdout (the user redirects, or uses `net install completions`). With no
/// shell, the shell-agnostic registry corpus is printed instead; with
/// `--json`, that corpus is printed as JSON.
fn completions_command(
    sub: &[String],
    flags: &Flags,
    availability: &registry::Availability,
    service: Option<&registry::ServiceRegistry>,
) -> i32 {
    let corpus = completions::corpus(availability, service);
    let shell = sub.first().map(String::as_str).unwrap_or("");
    match shell {
        "" => {
            // The registry corpus itself: the shell-agnostic form.
            if flags.json {
                let mut out = String::from("{\"completions\":[");
                for (i, (task, verbs, task_flags)) in corpus.tasks.iter().enumerate() {
                    json::sep(&mut out, i == 0);
                    let mut o = json::Obj::new(&mut out);
                    o.key("task").str(task);
                    o.key("verbs").sub(|out| {
                        let mut first = true;
                        json::array(out, |out| {
                            for verb in verbs {
                                json::arr_sep(out, first);
                                first = false;
                                json::write_str(out, verb);
                            }
                        });
                    });
                    o.key("flags").sub(|out| {
                        let mut first = true;
                        json::array(out, |out| {
                            let global: Vec<String> = registry::GLOBAL_FLAGS
                    .iter()
                    .map(|flag| flag.to_string())
                    .collect();
                for flag in task_flags.iter().chain(global.iter()) {
                                json::arr_sep(out, first);
                                first = false;
                                json::write_str(out, flag);
                            }
                        });
                    });
                    o.end();
                }
                out.push_str(&format!(
                    "],\"availabilityVerified\":{}}}",
                    corpus.availability_verified
                ));
                println!("{out}");
            } else {
                print_completion_notes(&corpus);
                for (task, verbs, _) in &corpus.tasks {
                    if verbs.is_empty() {
                        println!("{task}");
                    } else {
                        println!(
                            "{task} {}",
                            verbs
                                .iter()
                                .filter(|v| !v.starts_with("--"))
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                    }
                }
            }
            registry::EXIT_OK
        }
        _ => match completions::render(shell, &corpus) {
            Ok(text) => {
                print!("{text}");
                print_completion_notes(&corpus);
                registry::EXIT_OK
            }
            Err(reason) => {
                usage_error(flags, &reason);
                registry::EXIT_USAGE
            }
        },
    }
}

/// The honesty note every completion view carries: what was withheld (and
/// why), or that availability could not be verified.
fn print_completion_notes(corpus: &completions::Corpus) {
    if !corpus.withheld.is_empty() {
        eprintln!(
            "completions: {} command(s) withheld — the service reports them not runnable here: {}",
            corpus.withheld.len(),
            corpus.withheld.join(", ")
        );
        eprintln!("completions: run `net registry list-unavailable` for each reason");
    } else if !corpus.availability_verified {
        eprintln!(
            "completions: the service published no registry, so availability is UNVERIFIED — \
             a command is completed because this build implements it, not because the host can run it"
        );
    }
}

/// `net install completions [--shell S] [--path P] [--dry-run]` — write the
/// generated completer ONLY where the user points. The CLI never writes to a
/// system path silently; `--dry-run` prints what it would write.
fn install_command(
    sub: &[String],
    flags: &Flags,
    availability: &registry::Availability,
    service: Option<&registry::ServiceRegistry>,
) -> i32 {
    if sub.first().map(String::as_str) != Some("completions") {
        usage_error(flags, "usage: net install completions [--shell bash|zsh|fish|elvish] [--path P] [--dry-run]");
        return registry::EXIT_USAGE;
    }
    let shell = sub
        .iter()
        .skip(1)
        .position(|a| a == "--shell")
        .and_then(|i| sub.get(i + 2).cloned())
        .or_else(|| {
            sub.iter()
                .skip(1)
                .find_map(|a| a.strip_prefix("--shell=").map(String::from))
        })
        .unwrap_or_else(|| "bash".to_string());
    let path = sub
        .iter()
        .skip(1)
        .position(|a| a == "--path")
        .and_then(|i| sub.get(i + 2).cloned())
        .or_else(|| {
            sub.iter()
                .skip(1)
                .find_map(|a| a.strip_prefix("--path=").map(String::from))
        });
    let dry_run = sub.iter().skip(1).any(|a| a == "--dry-run");

    let corpus = completions::corpus(availability, service);
    let text = match completions::render(&shell, &corpus) {
        Ok(text) => text,
        Err(reason) => {
            usage_error(flags, &reason);
            return registry::EXIT_USAGE;
        }
    };
    let Some(path) = path else {
        usage_error(flags, "install needs --path: the CLI writes only where you point (no system path is written silently)");
        return registry::EXIT_USAGE;
    };
    if dry_run {
        // A dry run prints what it WOULD write and writes nothing.
        println!("# would write {} bytes to {path}", text.len());
        print!("{text}");
        return registry::EXIT_OK;
    }
    match completions::write_to(&path, &text) {
        Ok(bytes) => {
            if !flags.quiet {
                println!("installed {shell} completion ({bytes} bytes) to {path}");
            }
            registry::EXIT_OK
        }
        Err(e) => {
            usage_error(flags, &format!("cannot write {path}: {e}"));
            registry::EXIT_USAGE
        }
    }
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
    if cfg!(feature = "cmd-files") {
        families.push("files");
    }
    if cfg!(feature = "cmd-process") {
        families.push("process");
    }
    if cfg!(feature = "cmd-settings") {
        families.push("settings");
    }
    if cfg!(feature = "cmd-logging") {
        families.push("logging");
    }
    if cfg!(feature = "cmd-clock") {
        families.push("clock");
    }
    if cfg!(feature = "cmd-time") {
        families.push("time");
    }
    if cfg!(feature = "cmd-clipboard") {
        families.push("clipboard");
    }
    if cfg!(feature = "cmd-audio") {
        families.push("audio");
    }
    if cfg!(feature = "cmd-media") {
        families.push("media");
    }
    if cfg!(feature = "cmd-power") {
        families.push("power");
    }
    if cfg!(feature = "cmd-database") {
        families.push("database");
    }
    if cfg!(feature = "cmd-queue") {
        families.push("queue");
    }
    if cfg!(feature = "cmd-appindex") {
        families.push("appindex");
    }
    if cfg!(feature = "cmd-manifest") {
        families.push("manifest");
    }
    if cfg!(feature = "cmd-vcard") {
        families.push("vcard");
    }
    if cfg!(feature = "cmd-content") {
        families.push("content");
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
    // The task vocabulary is the REGISTRY (the same table the completions are
    // generated from), so help and completion can never drift apart.
    println!("tasks (registry order):");
    for line in registry_task_lines().iter().take(40) {
        println!("  {line}");
    }
    println!("  (run `net completions` for the machine-readable form, or `net completions <shell>`)");
    println!("  (selectors: --where <field> <op> <value> with ==, !=, >, <, ~= and and/or)");

    // Honesty: fetch the SERVICE's registry and list what is hidden, when a
    // service is reachable. A CLI that cannot reach the service says so and
    // does not lie.
    if let Ok(spawn) = remote::resolve(overrides) {
        if let Ok(mut conn) = Connection::connect(&spawn.endpoint) {
            let mut app = App {
                conn: &mut conn,
                mode: *flags,
                style: Style::plain(),
                spawn,
            };
            let service = registry::fetch(app.conn());
            let availability = registry::list_unavailable(app.conn());
            print_published(service.as_ref());
            print_hidden(&availability);
        }
    }
    registry::EXIT_OK
}

/// Print the commands the SERVICE published that this build does not
/// implement — they run through the generic path, and the help says so.
///
/// When the service publishes no registry at all, the help says that the list
/// above is the CLI's own grammar and that availability is UNVERIFIED.
fn print_published(service: Option<&registry::ServiceRegistry>) {
    println!();
    match service {
        Some(service) => {
            let extra: Vec<&registry::Command> = service
                .commands
                .iter()
                .filter(|command| !registry::implements(command))
                .collect();
            if extra.is_empty() {
                println!("published by the service: this build implements them all.");
                return;
            }
            println!("published by the service (this build runs them through the generic path):");
            for command in extra {
                println!("  {} — {}", command.path, command.summary);
            }
        }
        None => println!(
            "the service publishes no registry: the list above is this build's own grammar \
             and its availability is UNVERIFIED"
        ),
    }
}

/// The registry's task lines as help prints them (one source of truth with
/// the completion generator).
fn registry_task_lines() -> Vec<String> {
    registry::task_lines()
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
