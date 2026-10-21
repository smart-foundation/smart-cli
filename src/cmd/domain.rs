//! The registry-driven dispatch for the contract-served command families.
//!
//! # The rule
//!
//! A family's command PATH, its one-line summary and its availability come
//! from the SERVICE's registry (cli.capnp's `listCommands` / `listUnavailable`)
//! — never from a table written here. The **presenter** is keyed by the
//! contract's own identity for the call: the schema id and the method name
//! (`(schemaId, methodName)`), which is what the registry publishes and what
//! the contract's `CommandRegistration` binds.
//!
//! A verb this build has no presenter for runs through the GENERIC path (the
//! schema's node tree, which the service publishes in the registration's open
//! metadata), so the surface is never half-cut: every command the service
//! publishes is reachable, and the ones this build presents carry the
//! founder's own columns, glyphs and wording.
//!
//! # The write verbs
//!
//! A mutating verb is **gate-routed**: the CLI builds the contract's params
//! and issues the call; the SERVICE decides (the engine's serve layer refuses
//! a mutating verb on a read-only connection, with its own reason). The CLI
//! has no second authority model and no `--force`: the server's verdict is the
//! answer, and the CLI prints it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::present;
use crate::registry::{self, ServiceRegistry};

use super::App;

/// One presenter: the caller's arguments in, the contract's results rendered.
///
/// The last argument is the SERVICE'S OWN published path ("clock uptime"),
/// which the family may use to pick between the views the contract's one
/// method can answer (the service publishes three paths against `info`).
pub type Presenter = fn(&mut App, &[String], &str) -> i32;

/// The lookup: `(schema id, method name)` → the presenter that presents it.
pub type Lookup = fn(u64, &str) -> Option<Presenter>;

/// Run one command of a registry-served family.
///
/// The path is `"{family} {verb}"` — the product's own words, as the service
/// publishes them.
pub fn dispatch(family: &str, sub: &[String], app: &mut App, lookup: Lookup) -> i32 {
    if sub.is_empty() {
        return usage(app, family, None, "no command named", &empty_registry());
    }
    let Some(service) = registry::fetch(app.conn()) else {
        // The service publishes no registry: the CLI cannot know what this
        // family offers, and it says so rather than guessing a route.
        super::usage_error_app(
            app,
            &format!(
                "the service publishes no registry, so the `{family}` command surface cannot be \
                 verified; `{family} {}` is not run",
                sub.join(" ")
            ),
        );
        return registry::EXIT_UNAVAILABLE;
    };
    // The verb may be more than one word ("clock timer create"): the SERVICE's
    // published path says how many. The longest published path that matches
    // the caller's words wins, and the rest are the presenter's arguments.
    let Some((command, rest)) = match_published(&service, family, sub) else {
        let attempted = format!("{family} {}", sub.join(" "));
        return usage(
            app,
            family,
            Some(&attempted),
            "the service publishes no command named",
            &service,
        );
    };
    let path = command.path.clone();
    // The honesty filter: a command the service reports not runnable is
    // refused with the SERVICE'S OWN reason (the published table's row 3).
    if let Some(reason) = service.unavailable.iter().find(|u| u.path == path) {
        if app.json() {
            let mut out = String::new();
            crate::json::write_error(
                &mut out,
                crate::schema::types_capnp::ErrorCode::Unavailable,
                &reason.line(),
            );
            println!("{out}");
        } else {
            eprintln!("unavailable: {path} is unavailable: {}", reason.line());
        }
        return registry::EXIT_UNAVAILABLE;
    }
    // The presenter is keyed by (schema id, method name) — the contract's own
    // identity for the call.
    match lookup(command.schema_id, &command.method) {
        Some(presenter) => presenter(app, rest, &path),
        None => match super::generic_with(app, &service, &path, rest) {
            Some(code) => code,
            None => {
                super::usage_error_app(
                    app,
                    &format!(
                        "`{path}` is published by the service, but this build cannot address it \
                         (the service published no schema tree for it)"
                    ),
                );
                registry::EXIT_USAGE
            }
        },
    }
}

/// The longest published path under this family that matches the caller's
/// words, with the rest of the words as the presenter's arguments.
fn match_published<'a>(
    service: &'a registry::ServiceRegistry,
    family: &str,
    sub: &'a [String],
) -> Option<(registry::Command, &'a [String])> {
    for take in (1..=sub.len()).rev() {
        let verb = sub[..take].join(" ");
        let path = format!("{family} {verb}");
        if let Some(command) = service.command(&path) {
            return Some((command.clone(), &sub[take..]));
        }
    }
    None
}

/// An empty registry (the honest "the service published nothing" view).
fn empty_registry() -> ServiceRegistry {
    ServiceRegistry::default()
}

/// The honest usage answer: what the SERVICE publishes for this family.
fn usage(
    app: &mut App,
    family: &str,
    path: Option<&str>,
    what: &str,
    service: &ServiceRegistry,
) -> i32 {
    let published = published_verbs(service, family);
    let list = if published.is_empty() {
        "no command for this family".to_string()
    } else {
        published.join(", ")
    };
    let message = match path {
        Some(path) => format!("{what} `{path}`; the service publishes: {list}"),
        None => format!("{what} for `{family}`; the service publishes: {list}"),
    };
    super::usage_error_app(app, &message);
    registry::EXIT_USAGE
}

/// The verbs the service publishes for one family (the registry's own words).
fn published_verbs(service: &ServiceRegistry, family: &str) -> Vec<String> {
    let prefix = format!("{family} ");
    service
        .commands
        .iter()
        .filter_map(|command| command.path.strip_prefix(&prefix).map(|v| v.to_string()))
        .collect()
}

// ═════════════════════════════════════════════════════════════════════════
// The shell's own colour vocabulary, carried across: the CLI paints the same
// words the same way, and the paint table is the CLI's own (`--no-color`,
// `NO_COLOR`, a non-TTY all strip it).
// ═════════════════════════════════════════════════════════════════════════

/// De-emphasis.
pub fn dim(app: &App, text: &str) -> String {
    app.style().paint(text, 2)
}
/// The "this is information" colour.
pub fn cyan(app: &App, text: &str) -> String {
    app.style().paint(text, 36)
}
/// The "this is fine" colour.
pub fn green(app: &App, text: &str) -> String {
    app.style().paint(text, 32)
}
/// The "this failed" colour.
pub fn red(app: &App, text: &str) -> String {
    app.style().paint(text, 31)
}
/// The "look at this" colour.
pub fn yellow(app: &App, text: &str) -> String {
    app.style().paint(text, 33)
}
/// Emphasis.
pub fn bold(app: &App, text: &str) -> String {
    app.style().bold(text)
}

/// The yes/no cell: one glyph per state, never a colour alone.
pub fn yes_no(yes: bool) -> present::Cell {
    if yes {
        present::Cell::styled(present::glyph::ACTIVE, present::Sgr::Green)
    } else {
        present::Cell::styled(present::glyph::UNKNOWN, present::Sgr::Dimmed)
    }
}

/// A cell painted for a HUMAN LINE (the table renderer paints its own cells).
pub fn glyph(app: &App, cell: &present::Cell) -> String {
    let style = cell.style();
    if style == present::Sgr::Plain {
        return cell.content().to_string();
    }
    app.style().paint(cell.content(), style.code())
}

// ═════════════════════════════════════════════════════════════════════════
// The argument helpers the presenters share — the shell's grammar carried
// across: a positional argument, a `--flag value`, a `--flag=value`, and a
// boolean `--flag`.
// ═════════════════════════════════════════════════════════════════════════

/// The positional arguments (everything that is not a flag or a flag's value).
pub fn positionals(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg.starts_with('-') {
            // A `--flag value` pair consumes the next word; `--flag=value`
            // does not.
            if !arg.contains('=') && takes_value(arg) {
                skip = true;
            }
            continue;
        }
        out.push(arg.as_str());
    }
    out
}

/// The flags that carry a value (`--sort name`, `--pattern *.rs`).
const VALUE_FLAGS: &[&str] = &[
    "--sort",
    "--order",
    "--pattern",
    "--name",
    "--level",
    "--domain",
    "--lines",
    "--count",
    "--interval",
    "--bytes",
    "--offset",
    "--depth",
    "--to",
    "--date",
    "--since",
    "--key",
    "--mime",
    "--kind",
    "--owner",
    "--set",
    "--app",
    "--source",
    "--type",
    "--category",
    "--filter",
    "--limit",
    "--query",
    "--content",
    "--after",
    "--before",
    "--every",
    "--label",
    "--clock",
    "--recurrence",
    "--value",
];

/// Whether a flag carries a value (the grammar's own list).
fn takes_value(flag: &str) -> bool {
    VALUE_FLAGS.contains(&flag)
}

/// The value of a flag: `--name value` or `--name=value`.
pub fn flag(args: &[String], name: &str) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == name {
            return args.get(index + 1).cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
        index += 1;
    }
    None
}

/// Whether a boolean flag is present.
pub fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

/// A numeric flag's value, with the grammar's own default.
pub fn number<T: core::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    flag(args, name)
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or(default)
}

/// A numeric POSITIONAL argument, with the grammar's own default.
pub fn positional_number<T: core::str::FromStr>(args: &[String], index: usize, default: T) -> T {
    positionals(args)
        .get(index)
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or(default)
}
