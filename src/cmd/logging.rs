//! `logging` — the log domains, the retained entries, the live stream, the
//! export: the coupled shell's presentation, re-keyed from the engine's mirror
//! types (`LogEntry`, `LogDomain`, `LogFilter`) to the CONTRACT'S RESULT
//! READERS (`logging.capnp`'s `getEntries` / `listDomains` / `log` / `export`).
//!
//! ## What the re-keying changed
//!
//! * the engine's `LogDomain` mirror became the contract's `LogDomain`
//!   reader — the same four fields (`name`, `level`, `description`,
//!   `samplingRate`), read from the wire;
//! * the shell's `Value` records became the contract's own JSON (the field
//!   names are the schema's, verbatim);
//! * the shell's **host-sink** view (a path, a readable flag, a measured
//!   reason) has NO contract counterpart: the contract's `listDomains` answers
//!   the DOMAINS — the same columns the shell's domain listing used (domain,
//!   level, sampling, what it is). The registry publishes `logging sinks` and
//!   `logging domains` against that one method, and this CLI presents it once;
//! * the shell's `caps` view has no contract method either (the capability
//!   tokens live in discovery's descriptor metadata), so it is not on this
//!   surface.
//!
//! The registry publishes eleven paths: `logging sinks`, `logging domains`,
//! `logging tail`, `logging entries`, `logging follow`, `logging emit`,
//! `logging level`, `logging set-level`, `logging sampling`, `logging clear`,
//! `logging export`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json;
use crate::present;
use crate::schema::logging_capnp as wire;
use crate::schema::types_capnp::LogLevel;
use crate::transport as tp;

use super::domain::{self, Presenter};
use super::App;

/// The schema stem the family presents.
const STEM: &str = "logging";
/// The contract interface the family presents.
const INTERFACE: &str = "Logging";

/// The family's presenter lookup: keyed by **(schema id, method name)**.
pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != crate::contract::schema_id(STEM)? {
        return None;
    }
    Some(match method {
        "listDomains" => sinks,
        "getEntries" => entries,
        "subscribe" => follow,
        "log" => emit,
        "getLevel" => level,
        "setLevel" => set_level,
        "setSamplingRate" => sampling,
        "clear" => clear,
        "export" => export,
        _ => return None,
    })
}

/// Build one params message.
fn msg<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut capnp::message::Builder<capnp::message::HeapAllocator>),
{
    let mut message = capnp::message::Builder::new_default();
    f(&mut message);
    tp::encode_message(&message)
}

// ═════════════════════════════════════════════════════════════════════════
// `logging sinks` / `logging domains` — the domains this host serves, each
// with its level and its sampling rate.
// ═════════════════════════════════════════════════════════════════════════

fn sinks(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::list_domains_params::Builder<'_>>();
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listDomains", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::logging::list_domains_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::log_domain::Reader<'_>> = results
                .get_domains()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                domains_json(rows.iter().copied(), total),
                domains_human(app, &rows),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The domain table: the shell's own columns — domain, level, sampling, and
/// what the domain is.
fn domains_human(app: &App, domains: &[wire::log_domain::Reader<'_>]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} — the domains this backend knows\n",
        domain::cyan(app, "🗂"),
        domain::bold(app, "Log domains"),
        domain::dim(app, &format!("({})", domains.len()))
    ));
    if domains.is_empty() {
        out.push_str(&format!(
            "  └─ {} the backend knows no domain yet (an honest empty)\n",
            domain::dim(app, "○")
        ));
        return out.trim_end().to_string();
    }
    let mut table = present::Table::new(vec![
        String::from("DOMAIN"),
        String::from("LEVEL"),
        String::from("SAMPLING"),
        String::from("WHAT IT IS"),
    ])
    .cap(0, 30)
    .cap(3, 52);
    for entry in domains {
        let level = level_token(entry.get_level().unwrap_or(LogLevel::Info));
        table = table.row(vec![
            present::truncate_to(&text(entry.get_name()), 30).into(),
            level_cell(level),
            present::Cell::plain(format!("{:.2}", entry.get_sampling_rate())),
            present::truncate_to(&text(entry.get_description()), 52).into(),
        ]);
    }
    out.push_str(&table.render(app.style()));
    out.trim_end().to_string()
}

/// The contract-shaped JSON of the domain rows.
fn domains_json<'a>(domains: impl Iterator<Item = wire::log_domain::Reader<'a>>, total: u64) -> String {
    let mut out = String::from("{\"domains\":[");
    let mut first = true;
    for entry in domains {
        json::sep(&mut out, first);
        first = false;
        let mut o = json::Obj::new(&mut out);
        o.key("name").str(&text(entry.get_name()));
        o.key("level").str(level_token(entry.get_level().unwrap_or(LogLevel::Info)));
        o.key("description").str(&text(entry.get_description()));
        o.key("samplingRate").float(entry.get_sampling_rate() as f64);
        o.end();
    }
    out.push_str("],\"total\":");
    out.push_str(&total.to_string());
    out.push('}');
    out
}

// ═════════════════════════════════════════════════════════════════════════
// `logging tail` / `logging entries` — the retained lines.
// ═════════════════════════════════════════════════════════════════════════

fn entries(app: &mut App, args: &[String], path: &str) -> i32 {
    let domain = domain::flag(args, "--domain").unwrap_or_default();
    let min_level = level_of(&domain::flag(args, "--level").unwrap_or_default());
    let pattern = domain::flag(args, "--pattern").unwrap_or_default();
    let limit = domain::number::<u32>(args, "--lines", 64);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::get_entries_params::Builder<'_>>();
        {
            let mut filter = params.reborrow().init_filter();
            filter.set_domain(&domain);
            filter.set_min_level(min_level);
            filter.set_pattern(&pattern);
        }
        params.reborrow().init_page().set_limit(limit);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getEntries", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::logging::get_entries_results::Owned, _, _>(&bytes, |results| {
            let total = results.get_total();
            let rows: Vec<wire::log_entry::Reader<'_>> = results
                .get_entries()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            (
                entries_json(&rows, total),
                // The service publishes two paths against this one method:
                // `logging tail` (one domain's own lines) and `logging
                // entries` (the retained store). Each keeps its own view.
                if path.ends_with("tail") {
                    tail_human(app, &rows, &domain)
                } else {
                    entries_human(app, &rows, min_level)
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The shell's `Tail` view: one domain's own lines.
fn tail_human(app: &App, rows: &[wire::log_entry::Reader<'_>], domain_name: &str) -> String {
    let scope = if domain_name.is_empty() {
        "every domain"
    } else {
        domain_name
    };
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} {} — the host's own lines, read natively\n",
        domain::cyan(app, "📜"),
        domain::bold(app, "Tail"),
        domain::bold(app, scope),
        domain::dim(app, &format!("({})", rows.len()))
    ));
    if rows.is_empty() {
        out.push_str(&format!(
            "  └─ {} {} holds no line (a measured empty, never an invented one)\n",
            domain::dim(app, "○"),
            scope
        ));
        return out.trim_end().to_string();
    }
    for entry in rows {
        out.push_str(&format!(
            "  {} {:<7} {:<24} {}\n",
            domain::glyph(app, &level_glyph(level_token(entry.get_level().unwrap_or(LogLevel::Info)))),
            level_token(entry.get_level().unwrap_or(LogLevel::Info)),
            present::truncate_to(&text(entry.get_domain()), 24),
            text(entry.get_message())
        ));
    }
    out.trim_end().to_string()
}

/// The shell's `Entries` view: this process's own retained store.
fn entries_human(app: &App, rows: &[wire::log_entry::Reader<'_>], min_level: LogLevel) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} {} {}\n",
        domain::cyan(app, "🧾"),
        domain::bold(app, "Entries"),
        domain::dim(app, &format!("({})", rows.len())),
        domain::dim(app, "— this process's own retained store"),
        domain::dim(app, &format!("(min level {})", level_token(min_level)))
    ));
    if rows.is_empty() {
        out.push_str(&format!(
            "  └─ {} this process retained no entry (an honest empty — the store was read)\n",
            domain::dim(app, "○")
        ));
        return out.trim_end().to_string();
    }
    for entry in rows {
        out.push_str(&format!(
            "  {} {:<7} {:<18} {}\n",
            domain::glyph(app, &level_glyph(level_token(entry.get_level().unwrap_or(LogLevel::Info)))),
            level_token(entry.get_level().unwrap_or(LogLevel::Info)),
            present::truncate_to(&text(entry.get_domain()), 18),
            text(entry.get_message())
        ));
    }
    out.trim_end().to_string()
}

/// The contract-shaped JSON of the entry rows.
fn entries_json(rows: &[wire::log_entry::Reader<'_>], total: u64) -> String {
    let mut out = String::from("{\"entries\":[");
    for (index, entry) in rows.iter().enumerate() {
        json::sep(&mut out, index == 0);
        let mut o = json::Obj::new(&mut out);
        o.key("id").int(entry.get_id().map(|id| id.get_id()).unwrap_or(0));
        o.key("sequence").int(entry.get_sequence());
        o.key("timestamp").sub(|out| {
            let mut t = json::Obj::new(out);
            t.key("nanoseconds").int(entry.get_timestamp().map(|t| t.get_nanoseconds()).unwrap_or(0));
            t.end();
        });
        o.key("level").str(level_token(entry.get_level().unwrap_or(LogLevel::Info)));
        o.key("domain").str(&text(entry.get_domain()));
        o.key("message").str(&text(entry.get_message()));
        o.key("subsystem").int(entry.get_subsystem().map(|s| s.get_id()).unwrap_or(0));
        o.key("correlationId").str(&text(entry.get_correlation_id()));
        o.key("traceId").str(&text(entry.get_trace_id()));
        o.key("spanId").str(&text(entry.get_span_id()));
        o.key("parentSpanId").str(&text(entry.get_parent_span_id()));
        o.end();
    }
    out.push_str("],\"total\":");
    out.push_str(&total.to_string());
    out.push('}');
    out
}

// ═════════════════════════════════════════════════════════════════════════
// `logging follow` — the contract's own subscription: a minted token, a
// bounded pull, a cancel.
// ═════════════════════════════════════════════════════════════════════════

fn follow(app: &mut App, args: &[String], _path: &str) -> i32 {
    let domain_name = domain::flag(args, "--domain").unwrap_or_default();
    let min_level = level_of(&domain::flag(args, "--level").unwrap_or_default());
    let count = domain::number::<u32>(args, "--count", 3);
    let interval = domain::number::<u64>(args, "--interval", 400);
    let max = count.max(1);
    // The subscribe is a TYPED call: the contract's results carry the
    // subscription capability, and a capability does not survive
    // serialization — so this is the one path that keeps the answer alive.
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::subscribe_params::Builder<'_>>();
        let mut filter = params.reborrow().init_filter();
        filter.set_domain(&domain_name);
        filter.set_min_level(min_level);
    });
    let (interface_id, method_id) = match super::ids_in(STEM, INTERFACE, "subscribe") {
        Some(ids) => ids,
        None => {
            super::usage_error_app(app, "the contract has no `subscribe` method in this build");
            return crate::registry::EXIT_USAGE;
        }
    };
    let reader = match tp::decode_message::<wire::logging::subscribe_params::Owned>(&params) {
        Ok(reader) => reader,
        Err(error) => {
            super::usage_error_app(app, &format!("{error}"));
            return crate::registry::EXIT_USAGE;
        }
    };
    let call_params = match reader.get() {
        Ok(params) => params,
        Err(error) => {
            super::usage_error_app(app, &format!("{error}"));
            return crate::registry::EXIT_USAGE;
        }
    };
    let response = match app.conn().call_typed::<
        wire::logging::subscribe_params::Owned,
        wire::logging::subscribe_results::Owned,
    >(interface_id, method_id, call_params) {
        Ok(response) => response,
        Err(error) => return super::emit_error(app, &error),
    };
    let results = match response.get() {
        Ok(results) => results,
        Err(error) => return super::emit_error(app, &crate::transport::CallError::Transport(format!("{error}"))),
    };
    let capability = match results.get_subscription() {
        Ok(capability) => capability,
        Err(error) => {
            return super::emit_error(app, &crate::transport::CallError::Transport(format!("{error}")))
        }
    };
    let subscription = crate::transport::Subscription::new(capability.client, "logging", "LogSubscription");
    let json_mode = app.json();
    let quiet = app.quiet();
    let mut delivered = 0u32;
    let mut idle = 0u32;
    // A bounded pull loop: the contract's own `pull(maxEntries)`, then the
    // cancel. Never a busy poll, never an unbounded wait.
    while delivered < max && idle < 3 {
        let payload = match pull_entries(app, &subscription, max - delivered) {
            Ok(payload) => payload,
            Err(error) => {
                let _ = subscription.cancel(app.conn());
                return super::emit_error(app, &error);
            }
        };
        // The pull's answer IS the contract's own results struct; the rows
        // are painted inside the read, because a reader borrows its message.
        let batch = tp::with_root::<wire::log_subscription::pull_results::Owned, _, _>(
            &payload,
            |r| {
                let mut count = 0u32;
                if let Ok(list) = r.get_entries() {
                    for entry in list.iter() {
                        if json_mode {
                            let mut out = String::new();
                            let mut o = json::Obj::new(&mut out);
                            o.key("level")
                                .str(level_token(entry.get_level().unwrap_or(LogLevel::Info)));
                            o.key("domain").str(&text(entry.get_domain()));
                            o.key("message").str(&text(entry.get_message()));
                            o.key("sequence").int(entry.get_sequence());
                            o.end();
                            println!("{out}");
                        } else if !quiet {
                            println!(
                                "  {} {:<7} {:<18} {}",
                                domain::glyph(
                                    app,
                                    &level_glyph(level_token(entry.get_level().unwrap_or(LogLevel::Info)))
                                ),
                                level_token(entry.get_level().unwrap_or(LogLevel::Info)),
                                present::truncate_to(&text(entry.get_domain()), 18),
                                text(entry.get_message())
                            );
                        }
                        count += 1;
                    }
                }
                count
            },
        )
        .unwrap_or(0);
        if batch == 0 {
            idle += 1;
            crate::watch::sleep_ms(interval);
            continue;
        }
        delivered += batch;
        crate::watch::sleep_ms(interval);
    }
    let _ = subscription.cancel(app.conn());
    if json_mode {
        let mut out = String::from("{\"summary\":");
        let mut o = json::Obj::new(&mut out);
        o.key("delivered").int(delivered as u64);
        o.key("idle").int(idle as u64);
        o.end();
        out.push('}');
        println!("{out}");
    } else if !quiet {
        eprintln!(
            "watch: {} event(s) delivered, {} idle poll(s)",
            delivered, idle
        );
    }
    crate::registry::EXIT_OK
}

/// The subscription's own bounded pull (`pull(maxEntries)`).
fn pull_entries(
    app: &mut App,
    subscription: &crate::transport::Subscription,
    max: u32,
) -> Result<Vec<u8>, crate::transport::CallError> {
    let info = crate::contract::interface_by_name("logging", "LogSubscription").ok_or_else(|| {
        crate::transport::CallError::Transport(
            "the contract interface `LogSubscription` is not compiled into this build".to_string(),
        )
    })?;
    let method_id = crate::contract::method_id("logging", "LogSubscription", "pull").ok_or_else(|| {
        crate::transport::CallError::Transport(
            "the contract interface `LogSubscription` has no `pull` method in this build".to_string(),
        )
    })?;
    let params = msg(|m| {
        let mut params = m.init_root::<wire::log_subscription::pull_params::Builder<'_>>();
        params.set_max_entries(max);
    });
    app.conn().call_capability(
        subscription.client(),
        info.type_id,
        method_id,
        &params,
    )
}

// ═════════════════════════════════════════════════════════════════════════
// `logging emit` — one line, handed to the service; the SERVICE decides.
// ═════════════════════════════════════════════════════════════════════════

fn emit(app: &mut App, args: &[String], _path: &str) -> i32 {
    let level = level_of(&domain::flag(args, "--level").unwrap_or_else(|| "info".to_string()));
    let domain_name = domain::flag(args, "--domain").unwrap_or_default();
    let message = domain::positionals(args)
        .first()
        .map(|m| (*m).to_string())
        .or_else(|| domain::flag(args, "--message"))
        .unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::log_params::Builder<'_>>();
        params.set_severity(level);
        params.set_domain(&domain_name);
        params.set_message(&message);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "log", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::logging::log_results::Owned, _, _>(&bytes, |results| {
        let Ok(outcome) = results.get_outcome() else {
            return (
                String::from("{\"outcome\":null}"),
                format!("  {} Emit — the service answered no outcome\n", domain::cyan(app, "✎")),
            );
        };
        let (token, reason) = match outcome.which() {
            Ok(wire::log_outcome::Which::Accepted(_)) => ("accepted", String::new()),
            Ok(wire::log_outcome::Which::Sampled(_)) => ("sampled", String::new()),
            Ok(wire::log_outcome::Which::Dropped(_)) => ("dropped", String::new()),
            Ok(wire::log_outcome::Which::Error(error)) => {
                let reason = error
                    .map(|e| text(e.get_message()))
                    .unwrap_or_default();
                ("error", reason)
            }
            _ => ("unknown", String::new()),
        };
        (
            format!("{{\"outcome\":\"{token}\"}}"),
            format!(
                "  {} {} {} {} {}",
                domain::green(app, "✎"),
                domain::bold(app, "Emit"),
                domain::glyph(app, &level_cell(level_token(level))),
                domain::dim(app, &format!("({token})")),
                if reason.is_empty() { message } else { format!("{message} — {reason}") }
            ),
        )
    })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `logging level` / `logging set-level` / `logging sampling` / `logging clear`
// — the read and the three gated writes.
// ═════════════════════════════════════════════════════════════════════════

fn level(app: &mut App, args: &[String], _path: &str) -> i32 {
    let domain_name = domain::flag(args, "--domain").unwrap_or_default();
    if domain_name.is_empty() {
        super::usage_error_app(app, "usage: logging level <domain>");
        return crate::registry::EXIT_USAGE;
    }
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::get_level_params::Builder<'_>>();
        params.set_domain(&domain_name);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getLevel", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::logging::get_level_results::Owned, _, _>(
        &bytes,
        |results| {
            let level = results.get_level().unwrap_or(LogLevel::Info);
            (
                format!("{{\"level\":\"{}\"}}", level_token(level)),
                format!(
                    "  {} {} {} {} {}",
                    domain::cyan(app, "📶"),
                    domain::bold(app, "Level"),
                    domain::bold(app, &domain_name),
                    domain::dim(app, "→"),
                    domain::bold(app, level_token(level))
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// A gated write: the CLI issues the call; the SERVICE decides.
fn set_level(app: &mut App, args: &[String], _path: &str) -> i32 {
    let domain_name = domain::flag(args, "--domain").unwrap_or_default();
    let level = level_of(&domain::flag(args, "--level").unwrap_or_default());
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::set_level_params::Builder<'_>>();
        params.set_domain(&domain_name);
        params.set_level(level);
    });
    void_verb(app, "setLevel", "set-level", &domain_name, &params)
}

fn sampling(app: &mut App, args: &[String], _path: &str) -> i32 {
    let domain_name = domain::flag(args, "--domain").unwrap_or_default();
    let rate = domain::number::<f32>(args, "--rate", 1.0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::set_sampling_rate_params::Builder<'_>>();
        params.set_domain(&domain_name);
        params.set_rate(rate);
    });
    void_verb(app, "setSamplingRate", "sampling", &domain_name, &params)
}

fn clear(app: &mut App, args: &[String], _path: &str) -> i32 {
    let domain_name = domain::flag(args, "--domain").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::clear_params::Builder<'_>>();
        params.set_domain(&domain_name);
    });
    void_verb(app, "clear", "clear", &domain_name, &params)
}

/// A `T.VoidResult` write, in the shell's own "✎ verb target" line.
fn void_verb(app: &mut App, method: &str, verb: &str, target: &str, params: &[u8]) -> i32 {
    let bytes = match super::call_in(app, STEM, INTERFACE, method, params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<crate::schema::types_capnp::void_result::Owned, _, _>(&bytes, |r| {
        match r.which() {
            Ok(crate::schema::types_capnp::void_result::Which::Success(_)) => (
                format!("{{\"verb\":\"{verb}\",\"target\":\"{target}\",\"changed\":true}}"),
                format!(
                    "  {} {} {} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, verb),
                    target,
                    domain::dim(app, "— the engine's own host-mutating path, accepted")
                ),
            ),
            Ok(crate::schema::types_capnp::void_result::Which::Error(error)) => {
                let reason = error.map(|e| text(e.get_message())).unwrap_or_default();
                (
                    format!("{{\"verb\":\"{verb}\",\"error\":\"{reason}\"}}"),
                    format!(
                        "  {} {} {} {}",
                        domain::green(app, "✎"),
                        domain::bold(app, verb),
                        target,
                        domain::red(app, &format!("— refused: {reason}"))
                    ),
                )
            }
            _ => (
                format!("{{\"verb\":\"{verb}\",\"target\":\"{target}\"}}"),
                format!(
                    "  {} {} {} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, verb),
                    target,
                    domain::dim(app, "— the service answered no verdict")
                ),
            ),
        }
    })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `logging export` — the OPEN content type, rendered by the backend.
// ═════════════════════════════════════════════════════════════════════════

fn export(app: &mut App, args: &[String], _path: &str) -> i32 {
    let domain_name = domain::flag(args, "--domain").unwrap_or_default();
    let mime = domain::flag(args, "--mime").unwrap_or_else(|| "text/plain".to_string());
    let (kind, subtype) = match mime.split_once('/') {
        Some((kind, subtype)) => (kind.to_string(), subtype.to_string()),
        None => (mime.clone(), "plain".to_string()),
    };
    let params = msg(|m| {
        let mut params = m.init_root::<wire::logging::export_params::Builder<'_>>();
        params.reborrow().init_filter().set_domain(&domain_name);
        {
            let mut format = params.reborrow().init_format();
            format.set_type(&kind);
            format.set_subtype(&subtype);
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "export", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::logging::export_results::Owned, _, _>(&bytes, |r| {
        let (payload, count) = match r.get_data() {
            Ok(handle) => match handle.which() {
                Ok(crate::schema::types_capnp::data_handle::Which::Inline(data)) => {
                    let bytes = data.map(|d| d.to_vec()).unwrap_or_default();
                    let count = bytes.len();
                    (String::from_utf8_lossy(&bytes).to_string(), count)
                }
                Ok(crate::schema::types_capnp::data_handle::Which::Buffer(buffer)) => {
                    let id = buffer
                        .map(|b| b.get_id().to_string())
                        .unwrap_or_default();
                    (
                        format!("a shared buffer ({id}) — this build does not read buffers"),
                        0,
                    )
                }
                _ => (String::new(), 0),
            },
            Err(_) => (String::new(), 0),
        };
        (
            {
                let mut out = String::from("{\"data\":");
                let mut o = json::Obj::new(&mut out);
                o.key("mimeType").str(&format!("{kind}/{subtype}"));
                o.key("bytes").int(count as u64);
                o.key("text").str(&payload);
                o.end();
                out
            },
            {
                let mut out = format!(
                    "  {} {} {} {} byte(s) — an OPEN content type, rendered by the backend\n",
                    domain::cyan(app, "📤"),
                    domain::bold(app, "Export"),
                    domain::bold(app, &format!("{kind}/{subtype}")),
                    count
                );
                if !payload.is_empty() {
                    out.push('\n');
                    out.push_str(&present::truncate_to(&payload, 2000));
                }
                out.trim_end().to_string()
            },
        )
    })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// The shared helpers.
// ═════════════════════════════════════════════════════════════════════════

/// The severity glyph, in the contract's own vocabulary.
fn level_glyph(level: &str) -> present::Cell {
    match level {
        "trace" | "debug" => present::Cell::styled("·", present::Sgr::Dimmed),
        "info" => present::Cell::styled("◉", present::Sgr::Green),
        "warn" | "warning" => present::Cell::styled("◐", present::Sgr::Yellow),
        "error" | "fatal" | "crash" => present::Cell::styled("✗", present::Sgr::Red),
        "security" | "audit" => present::Cell::styled("◆", present::Sgr::Cyan),
        _ => present::Cell::plain("▢"),
    }
}

/// The glyph AND the level name, in one styled cell (the shell's own
/// `glyph.with_text(...)`).
fn level_cell(level: &str) -> present::Cell {
    let (glyph, style) = match level {
        "trace" | "debug" => ("·", present::Sgr::Dimmed),
        "info" => ("◉", present::Sgr::Green),
        "warn" | "warning" => ("◐", present::Sgr::Yellow),
        "error" | "fatal" | "crash" => ("✗", present::Sgr::Red),
        "security" | "audit" => ("◆", present::Sgr::Cyan),
        _ => ("▢", present::Sgr::Plain),
    };
    present::Cell::styled(&format!("{glyph} {level}"), style)
}

/// A level token, from the contract's own enum.
fn level_token(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
        LogLevel::Fatal => "fatal",
        LogLevel::Silent => "silent",
        LogLevel::Crash => "crash",
        LogLevel::Security => "security",
        LogLevel::Audit => "audit",
        _ => "custom",
    }
}

/// A level token, as the contract's own enum.
fn level_of(token: &str) -> LogLevel {
    match token {
        "trace" => LogLevel::Trace,
        "debug" => LogLevel::Debug,
        "info" => LogLevel::Info,
        "warn" | "warning" => LogLevel::Warn,
        "error" => LogLevel::Error,
        "fatal" => LogLevel::Fatal,
        "silent" => LogLevel::Silent,
        "crash" => LogLevel::Crash,
        "security" => LogLevel::Security,
        "audit" => LogLevel::Audit,
        _ => LogLevel::Info,
    }
}

/// A text reader, as a string (never a guess).
fn text(reader: capnp::Result<capnp::text::Reader<'_>>) -> String {
    reader
        .map(|t| t.to_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

/// Print the answer: the JSON object, or the human view.
fn print_answer(app: &App, json_text: String, human_text: String) -> i32 {
    if app.json() {
        println!("{json_text}");
    } else if !app.quiet() {
        // The human view ends with a newline (a shell prompt deserves one).
        if human_text.ends_with('\n') {
            print!("{human_text}");
        } else {
            println!("{human_text}");
        }
    }
    crate::registry::EXIT_OK
}
