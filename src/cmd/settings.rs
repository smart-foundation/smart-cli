//! `settings` — the one settings store: the domains, the values, the change
//! trail: the coupled shell's presentation, re-keyed from the engine's mirror
//! types (`SettingResult`, `SettingValue`, `SchemaInfo`, `SettingChange`) to
//! the CONTRACT'S RESULT READERS (`settings.capnp`'s `listDomains` / `getAll` /
//! `get` / `getHistory` / `getSettingsDiff` / `bulkGet` / `listByCategory`).
//!
//! ## What the re-keying changed
//!
//! * the engine's `SettingValue` mirror became the contract's `SettingValue`
//!   reader — the SAME union (string, integer, real, boolean, data, list,
//!   dict, unknown), read from the wire. The shell's GVariant tokens
//!   (`(mb)`, `(as)`, …) become the contract's own arm names, which are
//!   already human words;
//! * `notSet` is still an ANSWER, never a sentinel: the contract's
//!   `SettingResult` union says so, and this CLI paints it as the shell did;
//! * the shell's **schema browser** (`settings schema <domain>` — the types,
//!   the defaults, the ranges, the summaries) has NO contract method: the
//!   contract declares no schema-description call, so the service publishes
//!   no such command and this CLI does not invent one;
//! * the shell's per-key SCHEMA DEFAULT column has no contract counterpart
//!   either — the contract's `getAll` carries the key and the value only, so
//!   the table keeps KEY, VALUE and TYPE (the type being the union's own
//!   arm), and the default column is gone rather than faked;
//! * the shell's `caps` view has no contract method (the capability tokens
//!   live in discovery's descriptor metadata), so it is not on this surface.
//!
//! The registry publishes nineteen paths: `settings domains`, `settings list`,
//! `settings get`, `settings set`, `settings delete`, `settings clear`,
//! `settings watch`, `settings watch-domain`, `settings bulk-set`,
//! `settings bulk-get`, `settings category`, `settings set-app-override`,
//! `settings get-app-override`, `settings clear-app-override`,
//! `settings version`, `settings set-if-unchanged`, `settings history`,
//! `settings diff` (and the subscription's own `settings pull` /
//! `settings cancel`).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json;
use crate::present;
use smart_protocol::schema::settings_capnp as wire;
use crate::transport as tp;

use super::domain::{self, Presenter};
use super::App;

/// The schema stem the family presents.
const STEM: &str = "settings";
/// The contract interface the family presents.
const INTERFACE: &str = "Settings";

/// The family's presenter lookup: keyed by **(schema id, method name)**.
pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != smart_protocol::contract::schema_id(STEM)? {
        return None;
    }
    Some(match method {
        "listDomains" => domains,
        "getAll" => list,
        "get" => get,
        "set" => set,
        "delete" => delete,
        "clear" => clear,
        "watch" => watch,
        "watchDomain" => watch_domain,
        "bulkSet" => bulk_set,
        "bulkGet" => bulk_get,
        "listByCategory" => category,
        "setAppOverride" => set_app_override,
        "getAppOverride" => get_app_override,
        "clearAppOverride" => clear_app_override,
        "getVersion" => version,
        "setIfUnchanged" => set_if_unchanged,
        "getHistory" => history,
        "getSettingsDiff" => diff,
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

/// The domain the caller named (a positional, or `--domain`).
fn domain_name(args: &[String]) -> String {
    domain::flag(args, "--domain")
        .or_else(|| domain::positionals(args).first().map(|d| (*d).to_string()))
        .unwrap_or_default()
}

/// The key the caller named (a positional after the domain, or `--key`).
fn key_name(args: &[String]) -> String {
    domain::flag(args, "--key")
        .or_else(|| domain::positionals(args).get(1).map(|k| (*k).to_string()))
        .unwrap_or_default()
}

// ═════════════════════════════════════════════════════════════════════════
// `settings domains` — the schema source the host publishes.
// ═════════════════════════════════════════════════════════════════════════

fn domains(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::list_domains_params::Builder<'_>>();
        params.reborrow().init_pagination().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listDomains", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::settings::list_domains_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<String> = results
                .get_domains()
                .map(|list| list.iter().map(|d| text(d)).collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"domains\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut o = json::Obj::new(&mut out);
                        o.key("domain").str(row);
                        o.end();
                    }
                    out.push_str("],\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                domains_human(app, &rows, total),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The domain listing: the shell's own table, with the columns the contract
/// can actually fill (the schema's key COUNT and the schema's PATH are the
/// host's own metadata, and the contract does not carry them).
fn domains_human(app: &App, rows: &[String], total: u64) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} {}\n",
        domain::cyan(app, "⚙"),
        domain::bold(app, "Settings"),
        domain::dim(app, &format!("({total})")),
        domain::dim(app, "— the host's own schema source, read natively")
    ));
    if rows.is_empty() {
        out.push_str(&format!(
            "  └─ {} the host publishes no schema (an honest empty — the source was read)\n",
            domain::dim(app, "○")
        ));
        return out.trim_end().to_string();
    }
    let mut table = present::Table::new(vec![String::new(), String::from("DOMAIN")]).cap(1, 52);
    for row in rows {
        table = table.row(vec![
            domain::yes_no(true),
            present::truncate_to(row, 52).into(),
        ]);
    }
    out.push_str(&table.render(app.style()));
    out.trim_end().to_string()
}

// ═════════════════════════════════════════════════════════════════════════
// `settings list` — one domain's live values.
// ═════════════════════════════════════════════════════════════════════════

fn list(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    if name.is_empty() {
        super::usage_error_app(app, "usage: settings list <domain>");
        return crate::registry::EXIT_USAGE;
    }
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::get_all_params::Builder<'_>>();
        params.set_domain(&name);
        params.reborrow().init_pagination().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getAll", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::settings::get_all_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::setting_map_entry::Reader<'_>> = results
                .get_settings()
                .map(|map| {
                    map.get_entries()
                        .map(|list| list.iter().collect())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = map_json("settings", &rows);
                    out.pop();
                    out.push_str(",\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                list_human(app, &rows, &name),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The value table: KEY, VALUE, TYPE — the contract's own union names the
/// type, and the contract carries no schema default.
fn list_human(app: &App, rows: &[wire::setting_map_entry::Reader<'_>], name: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} {} — the host's own values\n",
        domain::cyan(app, "🗂"),
        domain::bold(app, "Settings"),
        domain::bold(app, name),
        domain::dim(app, &format!("({})", rows.len()))
    ));
    if rows.is_empty() {
        out.push_str(&format!(
            "  └─ {} the schema publishes no key (an honest empty — the host was read)\n",
            domain::dim(app, "○")
        ));
        return out.trim_end().to_string();
    }
    let mut table = present::Table::new(vec![
        String::from("KEY"),
        String::from("VALUE"),
        String::from("TYPE"),
    ]);
    for row in rows {
        let value = row.get_value();
        table = table.row(vec![
            present::Cell::plain(text(row.get_key())),
            present::truncate_to(&value_word(&value), 40).into(),
            present::Cell::plain(value_type(&value)),
        ]);
    }
    out.push_str(&table.render(app.style()));
    out.trim_end().to_string()
}

// ═════════════════════════════════════════════════════════════════════════
// `settings get` — one key. `notSet` is an ANSWER.
// ═════════════════════════════════════════════════════════════════════════

fn get(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let key = key_name(args);
    if name.is_empty() || key.is_empty() {
        super::usage_error_app(app, "usage: settings get <domain> <key>");
        return crate::registry::EXIT_USAGE;
    }
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::get_params::Builder<'_>>();
        params.set_domain(&name);
        params.set_key(&key);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "get", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::settings::get_results::Owned, _, _>(
        &bytes,
        |results| {
            let Ok(result) = results.get_result() else {
                return (
                    String::from("{\"result\":null}"),
                    format!("  {} the service answered no result\n", domain::cyan(app, "🔎")),
                );
            };
            match result.which() {
                Ok(wire::setting_result::Which::Value(value)) => {
                    (
                        {
                            let mut out = String::from("{\"result\":");
                            let mut o = json::Obj::new(&mut out);
                            o.key("value").sub(|out| write_value(out, &value));
                            o.end();
                            out
                        },
                        format!(
                            "  {} {} {}\n  ├─ Value       {} {}\n  └─ Source     the contract's own store",
                            domain::cyan(app, "🔎"),
                            domain::bold(app, "Setting"),
                            domain::bold(app, &format!("{name} {key}")),
                            domain::bold(app, &value_word(&value)),
                            domain::dim(app, &format!("({})", value_type(&value)))
                        ),
                    )
                }
                Ok(wire::setting_result::Which::NotSet(_)) => (
                    String::from("{\"result\":{\"notSet\":null}}"),
                    format!(
                        "  {} {} {}\n  ├─ Value       {} the store carries no value for this key\n  └─ Source     the contract's own store",
                        domain::cyan(app, "🔎"),
                        domain::bold(app, "Setting"),
                        domain::bold(app, &format!("{name} {key}")),
                        domain::dim(app, "○")
                    ),
                ),
                Ok(wire::setting_result::Which::Error(error)) => {
                    let reason = error
                        .map(|e| text(e.get_message()))
                        .unwrap_or_default();
                    (
                        {
                            let mut out = String::from("{\"result\":{\"error\":");
                            let mut o = json::Obj::new(&mut out);
                            o.key("message").str(&reason);
                            o.end();
                            out.push('}');
                            out
                        },
                        format!(
                            "  {} {} {}\n  └─ {} {reason}",
                            domain::cyan(app, "🔎"),
                            domain::bold(app, "Setting"),
                            domain::bold(app, &format!("{name} {key}")),
                            domain::red(app, "✗")
                        ),
                    )
                }
                _ => (
                    String::from("{\"result\":null}"),
                    format!(
                        "  {} {} {}\n  └─ Source     the contract's own store",
                        domain::cyan(app, "🔎"),
                        domain::bold(app, "Setting"),
                        domain::bold(app, &format!("{name} {key}"))
                    ),
                ),
            }
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `settings category` — the contract's OPEN category token.
// ═════════════════════════════════════════════════════════════════════════

fn category(app: &mut App, args: &[String], _path: &str) -> i32 {
    let token = domain::flag(args, "--category")
        .or_else(|| domain::positionals(args).first().map(|c| (*c).to_string()))
        .unwrap_or_default();
    if token.is_empty() {
        super::usage_error_app(app, "usage: settings category <category>");
        return crate::registry::EXIT_USAGE;
    }
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::list_by_category_params::Builder<'_>>();
        params.set_category(&token);
        params.reborrow().init_pagination().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listByCategory", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::settings::list_by_category_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::setting_map_entry::Reader<'_>> = results
                .get_entries()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = map_json("entries", &rows);
                    out.pop();
                    out.push_str(",\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {}\n",
                        domain::cyan(app, "🏷"),
                        domain::bold(app, "Category"),
                        domain::dim(app, &format!("({})", rows.len())),
                        domain::bold(app, &token)
                    ));
                    out.push_str(&format!(
                        "  └─ {}\n",
                        domain::dim(
                            app,
                            "the contract's own OPEN category token — recorded beside each key, never guessed"
                        )
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  {} no key is recorded under {token} — the category is recorded beside \
                             each key, never guessed\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("KEY"),
                            String::from("VALUE"),
                        ]);
                        for row in &rows {
                            let value = row.get_value();
                            table = table.row(vec![
                                present::Cell::plain(text(row.get_key())),
                                present::truncate_to(&value_word(&value), 40).into(),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
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
// `settings version` / `settings history` / `settings diff` — the store's
// own version, its change trail, and the delta read.
// ═════════════════════════════════════════════════════════════════════════

fn version(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let key = key_name(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::get_version_params::Builder<'_>>();
        params.set_domain(&name);
        params.set_key(&key);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getVersion", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::settings::get_version_results::Owned, _, _>(&bytes, |results| {
            let version = results.get_version();
            (
                format!("{{\"version\":{version}}}"),
                format!(
                    "  {} {} {} {} → version {}",
                    domain::cyan(app, "🔢"),
                    domain::bold(app, "Version"),
                    name,
                    key,
                    domain::bold(app, &version.to_string())
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn history(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let key = key_name(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::get_history_params::Builder<'_>>();
        params.set_domain(&name);
        params.set_key(&key);
        params.reborrow().init_pagination().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getHistory", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::settings::get_history_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::setting_change::Reader<'_>> = results
                .get_changes()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = changes_json("changes", &rows);
                    out.pop();
                    out.push_str(",\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {} {}\n",
                        domain::cyan(app, "📜"),
                        domain::bold(app, "History"),
                        domain::bold(app, &format!("{name} {key}")),
                        domain::dim(app, &format!("({})", rows.len())),
                        domain::dim(app, "— the store's own monotonic trail")
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the store records no change for this key\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("VERSION"),
                            String::from("ACTOR"),
                            String::from("VALUE"),
                            String::from("WHEN"),
                        ]);
                        for change in &rows {
                            let when = change
                                .get_timestamp()
                                .map(|t| t.get_nanoseconds())
                                .unwrap_or(0);
                            table = table.row(vec![
                                present::Cell::plain(change.get_version().to_string()),
                                present::Cell::plain(text(change.get_actor_id())),
                                present::truncate_to(&value_word(&change.get_value()), 36).into(),
                                present::Cell::plain(when.to_string()),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
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

fn diff(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let since = domain::number::<u64>(args, "--since", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::get_settings_diff_params::Builder<'_>>();
        params.set_domain(&name);
        params.set_since_version(since);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getSettingsDiff", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::settings::get_settings_diff_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::setting_change::Reader<'_>> = results
                .get_changes()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let current = results.get_current_version();
            (
                {
                    let mut out = String::new();
                    let mut o = json::Obj::new(&mut out);
                    o.key("changes").sub(|out| {
                        out.push('[');
                        for (index, row) in rows.iter().enumerate() {
                            json::sep(out, index == 0);
                            let mut entry = json::Obj::new(out);
                            entry.key("domain").str(&text(row.get_domain()));
                            entry.key("key").str(&text(row.get_key()));
                            entry.key("value").sub(|out| write_value(out, &row.get_value()));
                            entry.key("version").int(row.get_version());
                            entry.end();
                        }
                        out.push(']');
                    });
                    o.key("currentVersion").int(current);
                    o.end();
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {} {}\n",
                        domain::cyan(app, "⇄"),
                        domain::bold(app, "Diff"),
                        domain::bold(app, &name),
                        domain::dim(app, &format!("({})", rows.len())),
                        domain::dim(app, &format!("since version {since} → {current}"))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} nothing changed after that version (a measured empty)\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("KEY"),
                            String::from("VALUE"),
                            String::from("VERSION"),
                        ]);
                        for change in &rows {
                            table = table.row(vec![
                                present::Cell::plain(text(change.get_key())),
                                present::truncate_to(&value_word(&change.get_value()), 40).into(),
                                present::Cell::plain(change.get_version().to_string()),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
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
// `settings bulk-get` — the contract's StringMap read.
// ═════════════════════════════════════════════════════════════════════════

fn bulk_get(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let keys: Vec<String> = domain::positionals(args)
        .iter()
        .skip(1)
        .map(|k| (*k).to_string())
        .collect();
    if name.is_empty() || keys.is_empty() {
        super::usage_error_app(app, "usage: settings bulk-get <domain> <key>…");
        return crate::registry::EXIT_USAGE;
    }
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::bulk_get_params::Builder<'_>>();
        params.set_domain(&name);
        let mut list = params.reborrow().init_keys(keys.len() as u32);
        for (index, key) in keys.iter().enumerate() {
            list.set(index as u32, key.as_str());
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "bulkGet", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::settings::bulk_get_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::setting_map_entry::Reader<'_>> = results
                .get_entries()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            (
                map_json("entries", &rows),
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {} — the contract's StringMap read\n",
                        domain::cyan(app, "📥"),
                        domain::bold(app, "Bulk read"),
                        domain::bold(app, &name),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    let mut table = present::Table::new(vec![
                        String::from("KEY"),
                        String::from("VALUE"),
                    ]);
                    for row in &rows {
                        let value = row.get_value();
                        table = table.row(vec![
                            present::Cell::plain(text(row.get_key())),
                            present::truncate_to(&value_word(&value), 48).into(),
                        ]);
                    }
                    out.push_str(&table.render(app.style()));
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
// `settings watch` / `settings watch-domain` — the store's own change
// signal, delivered live through the contract's subscription.
// ═════════════════════════════════════════════════════════════════════════

fn watch(app: &mut App, args: &[String], _path: &str) -> i32 {
    watch_verb(app, args, "watch", true)
}

fn watch_domain(app: &mut App, args: &[String], _path: &str) -> i32 {
    watch_verb(app, args, "watchDomain", false)
}

/// The subscription: a minted token, a bounded pull, a cancel.
fn watch_verb(app: &mut App, args: &[String], method: &str, with_key: bool) -> i32 {
    let name = domain_name(args);
    let key = key_name(args);
    let count = domain::number::<u32>(args, "--count", 3);
    let interval = domain::number::<u64>(args, "--interval", 400);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::watch_params::Builder<'_>>();
        params.set_domain(&name);
        params.set_key(&key);
    });
    let params = if with_key {
        params
    } else {
        msg(|m| {
            let mut params = m.init_root::<wire::settings::watch_domain_params::Builder<'_>>();
            params.set_domain(&name);
        })
    };
    let (interface_id, method_id) = match super::ids_in(STEM, INTERFACE, method) {
        Some(ids) => ids,
        None => {
            super::usage_error_app(app, "the contract has no subscription method in this build");
            return crate::registry::EXIT_USAGE;
        }
    };
    let reader = match tp::decode_message::<wire::settings::watch_params::Owned>(&params) {
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
        wire::settings::watch_params::Owned,
        wire::settings::watch_results::Owned,
    >(interface_id, method_id, call_params) {
        Ok(response) => response,
        Err(error) => return super::emit_error(app, &error),
    };
    let results = match response.get() {
        Ok(results) => results,
        Err(error) => {
            return super::emit_error(app, &crate::transport::CallError::Transport(format!("{error}")))
        }
    };
    let capability = match results.get_subscription() {
        Ok(capability) => capability,
        Err(error) => {
            return super::emit_error(app, &crate::transport::CallError::Transport(format!("{error}")))
        }
    };
    let subscription =
        crate::transport::Subscription::new(capability.client, "settings", "SettingsSubscription");
    let json_mode = app.json();
    let quiet = app.quiet();
    let mut delivered = 0u32;
    let mut idle = 0u32;
    while delivered < count.max(1) && idle < 3 {
        let payload = match pull_changes(app, &subscription, count.max(1) - delivered) {
            Ok(payload) => payload,
            Err(error) => {
                let _ = subscription.cancel(app.conn());
                return super::emit_error(app, &error);
            }
        };
        let batch = tp::with_root::<wire::settings_subscription::pull_results::Owned, _, _>(
            &payload,
            |r| {
                let mut count = 0u32;
                if let Ok(list) = r.get_changes() {
                    for change in list.iter() {
                        if json_mode {
                            let mut out = String::new();
                            let mut o = json::Obj::new(&mut out);
                            o.key("key").str(&text(change.get_key()));
                            o.key("version").int(change.get_version());
                            o.key("actorId").str(&text(change.get_actor_id()));
                            o.key("value").sub(|out| write_value(out, &change.get_value()));
                            o.end();
                            println!("{out}");
                        } else if !quiet {
                            println!(
                                "  {} {:<28} {:<40} {}",
                                domain::green(app, "∼"),
                                present::truncate_to(&text(change.get_key()), 28),
                                present::truncate_to(&value_word(&change.get_value()), 40),
                                change.get_version()
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
        println!("{out}");
    } else if !quiet {
        eprintln!("watch: {} event(s) delivered, {} idle poll(s)", delivered, idle);
    }
    crate::registry::EXIT_OK
}

/// The subscription's own bounded pull.
fn pull_changes(
    app: &mut App,
    subscription: &crate::transport::Subscription,
    max: u32,
) -> Result<Vec<u8>, crate::transport::CallError> {
    let info = smart_protocol::contract::interface_by_name("settings", "SettingsSubscription").ok_or_else(
        || {
            crate::transport::CallError::Transport(
                "the contract interface `SettingsSubscription` is not compiled into this build"
                    .to_string(),
            )
        },
    )?;
    let method_id =
        smart_protocol::contract::method_id("settings", "SettingsSubscription", "pull").ok_or_else(|| {
            crate::transport::CallError::Transport(
                "the contract interface `SettingsSubscription` has no `pull` method in this build"
                    .to_string(),
            )
        })?;
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings_subscription::pull_params::Builder<'_>>();
        params.set_max_events(max);
    });
    app.conn()
        .call_capability(subscription.client(), info.type_id, method_id, &params)
}

// ═════════════════════════════════════════════════════════════════════════
// The gated writes: the CLI issues the call; the SERVICE decides.
// ═════════════════════════════════════════════════════════════════════════

fn set(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let key = key_name(args);
    let raw = domain::flag(args, "--value")
        .or_else(|| domain::positionals(args).get(2).map(|v| (*v).to_string()))
        .unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::set_params::Builder<'_>>();
        params.set_domain(&name);
        params.set_key(&key);
        set_value(params.reborrow().init_value(), &raw);
    });
    void_verb(app, "set", "set", &format!("{name} {key}"), &params)
}

fn delete(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let key = key_name(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::delete_params::Builder<'_>>();
        params.set_domain(&name);
        params.set_key(&key);
    });
    void_verb(app, "delete", "delete", &format!("{name} {key}"), &params)
}

fn clear(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::clear_params::Builder<'_>>();
        params.set_domain(&name);
    });
    void_verb(app, "clear", "clear", &name, &params)
}

fn bulk_set(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let entries: Vec<(String, String)> = domain::positionals(args)
        .iter()
        .skip(1)
        .filter_map(|entry| entry.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();
    if name.is_empty() || entries.is_empty() {
        super::usage_error_app(app, "usage: settings bulk-set <domain> <key=value>…");
        return crate::registry::EXIT_USAGE;
    }
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::bulk_set_params::Builder<'_>>();
        params.set_domain(&name);
        let mut list = params.reborrow().init_entries(entries.len() as u32);
        for (index, (key, raw)) in entries.iter().enumerate() {
            let mut entry = list.reborrow().get(index as u32);
            entry.set_key(key);
            set_value(entry.reborrow().init_value(), raw);
        }
    });
    void_verb(
        app,
        "bulkSet",
        "bulk-set",
        &format!("{name} ({} entries)", entries.len()),
        &params,
    )
}

fn set_app_override(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain::flag(args, "--app").unwrap_or_default();
    let key = key_name(args);
    let raw = domain::flag(args, "--value")
        .or_else(|| domain::positionals(args).get(2).map(|v| (*v).to_string()))
        .unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::set_app_override_params::Builder<'_>>();
        params.reborrow().init_app_id().set_id(app_id(&name));
        params.set_key(&key);
        set_value(params.reborrow().init_value(), &raw);
    });
    void_verb(app, "setAppOverride", "set-app-override", &format!("{name} {key}"), &params)
}

fn clear_app_override(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain::flag(args, "--app").unwrap_or_default();
    let key = key_name(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::clear_app_override_params::Builder<'_>>();
        params.reborrow().init_app_id().set_id(app_id(&name));
        params.set_key(&key);
    });
    void_verb(
        app,
        "clearAppOverride",
        "clear-app-override",
        &format!("{name} {key}"),
        &params,
    )
}

fn get_app_override(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain::flag(args, "--app").unwrap_or_default();
    let key = key_name(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::get_app_override_params::Builder<'_>>();
        params.reborrow().init_app_id().set_id(app_id(&name));
        params.set_key(&key);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getAppOverride", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::settings::get_app_override_results::Owned, _, _>(&bytes, |results| {
            let exists = results.get_exists();
            let value = results.get_value();
            (
                {
                    let mut out = String::new();
                    let mut o = json::Obj::new(&mut out);
                    o.key("exists").boolean(exists);
                    o.key("value").sub(|out| write_value(out, &value));
                    o.end();
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "🔎"),
                    domain::bold(app, "Override"),
                    domain::bold(app, &format!("{name} {key}")),
                    if exists {
                        domain::green(app, "◉ set")
                    } else {
                        domain::dim(app, "○ not set")
                    }
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn set_if_unchanged(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain_name(args);
    let key = key_name(args);
    let raw = domain::flag(args, "--value")
        .or_else(|| domain::positionals(args).get(2).map(|v| (*v).to_string()))
        .unwrap_or_default();
    let expected = domain::number::<u64>(args, "--version", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::settings::set_if_unchanged_params::Builder<'_>>();
        params.set_domain(&name);
        params.set_key(&key);
        set_value(params.reborrow().init_value(), &raw);
        params.set_expected_version(expected);
    });
    void_verb(
        app,
        "setIfUnchanged",
        "set-if-unchanged",
        &format!("{name} {key}"),
        &params,
    )
}

/// A `T.VoidResult` write, in the shell's own "✎ verb target" line.
fn void_verb(app: &mut App, method: &str, verb: &str, target: &str, params: &[u8]) -> i32 {
    let bytes = match super::call_in(app, STEM, INTERFACE, method, params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<smart_protocol::schema::types_capnp::void_result::Owned, _, _>(&bytes, |r| {
            match r.which() {
                Ok(smart_protocol::schema::types_capnp::void_result::Which::Success(_)) => (
                    format!("{{\"verb\":\"{verb}\",\"target\":\"{target}\",\"changed\":true}}"),
                    format!(
                        "  {} {} {} {}",
                        domain::green(app, "✎"),
                        domain::bold(app, verb),
                        target,
                        domain::dim(app, "— the engine's own host-mutating path, accepted")
                    ),
                ),
                Ok(smart_protocol::schema::types_capnp::void_result::Which::Error(error)) => {
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
// The shared helpers.
// ═════════════════════════════════════════════════════════════════════════

/// An app id from the app's own name (the contract's opaque id).
fn app_id(name: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// A value's own word, for a human cell.
fn value_word(value: &capnp::Result<wire::setting_value::Reader<'_>>) -> String {
    let Ok(value) = value else {
        return String::from("▢ unreadable");
    };
    match value.which() {
        Ok(wire::setting_value::Which::String(inner)) => text(inner),
        Ok(wire::setting_value::Which::Integer(number)) => number.to_string(),
        Ok(wire::setting_value::Which::Real(number)) => number.to_string(),
        Ok(wire::setting_value::Which::Boolean(flag)) => flag.to_string(),
        Ok(wire::setting_value::Which::Data(handle)) => match handle {
            Ok(handle) => match handle.which() {
                Ok(smart_protocol::schema::types_capnp::data_handle::Which::Inline(data)) => {
                    let bytes = data.map(|d| d.len()).unwrap_or(0);
                    format!("◉ a data handle ({bytes} byte(s))")
                }
                _ => "◉ a data handle".to_string(),
            },
            Err(_) => "◉ a data handle".to_string(),
        },
        Ok(wire::setting_value::Which::List(list)) => {
            let count = list.map(|l| l.len()).unwrap_or(0);
            format!("▢ a list of {count}")
        }
        Ok(wire::setting_value::Which::Dict(map)) => {
            let count = map
                .ok()
                .and_then(|map| map.get_entries().ok())
                .map(|list| list.len())
                .unwrap_or(0);
            format!("▢ a map of {count}")
        }
        _ => "▢ unknown".to_string(),
    }
}

/// A value's own type, in the contract's own vocabulary.
fn value_type(value: &capnp::Result<wire::setting_value::Reader<'_>>) -> &'static str {
    let Ok(value) = value else {
        return "unreadable";
    };
    match value.which() {
        Ok(wire::setting_value::Which::String(_)) => "string",
        Ok(wire::setting_value::Which::Integer(_)) => "integer",
        Ok(wire::setting_value::Which::Real(_)) => "real",
        Ok(wire::setting_value::Which::Boolean(_)) => "boolean",
        Ok(wire::setting_value::Which::Data(_)) => "data",
        Ok(wire::setting_value::Which::List(_)) => "list",
        Ok(wire::setting_value::Which::Dict(_)) => "dict",
        _ => "unknown",
    }
}

/// The contract-shaped JSON of a value: the union's own arm name.
fn write_value(out: &mut String, value: &capnp::Result<wire::setting_value::Reader<'_>>) {
    let Ok(value) = value else {
        let mut o = json::Obj::new(out);
        o.key("unreadable").str("the value could not be read");
        o.end();
        return;
    };
    match value.which() {
        Ok(wire::setting_value::Which::String(inner)) => {
            let mut o = json::Obj::new(out);
            o.key("string").str(&text(inner));
            o.end();
        }
        Ok(wire::setting_value::Which::Integer(number)) => {
            let mut o = json::Obj::new(out);
            o.key("integer").int(number);
            o.end();
        }
        Ok(wire::setting_value::Which::Real(number)) => {
            let mut o = json::Obj::new(out);
            o.key("real").float(number);
            o.end();
        }
        Ok(wire::setting_value::Which::Boolean(flag)) => {
            let mut o = json::Obj::new(out);
            o.key("boolean").boolean(flag);
            o.end();
        }
        Ok(wire::setting_value::Which::Data(_)) => {
            let mut o = json::Obj::new(out);
            o.key("data").str("a data handle");
            o.end();
        }
        Ok(wire::setting_value::Which::List(_)) => {
            let mut o = json::Obj::new(out);
            o.key("list").str("a list");
            o.end();
        }
        Ok(wire::setting_value::Which::Dict(_)) => {
            let mut o = json::Obj::new(out);
            o.key("dict").str("a map");
            o.end();
        }
        _ => {
            let mut o = json::Obj::new(out);
            o.key("unknown").str("unknown");
            o.end();
        }
    }
}

/// The contract-shaped JSON of a StringMap's entries.
fn map_json(key: &str, rows: &[wire::setting_map_entry::Reader<'_>]) -> String {
    let mut out = String::new();
    let mut o = json::Obj::new(&mut out);
    o.key(key).sub(|out| {
        out.push('[');
        for (index, row) in rows.iter().enumerate() {
            json::sep(out, index == 0);
            let mut entry = json::Obj::new(out);
            entry.key("key").str(&text(row.get_key()));
            entry.key("value").sub(|out| write_value(out, &row.get_value()));
            entry.end();
        }
        out.push(']');
    });
    o.end();
    out
}

/// The contract-shaped JSON of a change list.
fn changes_json(key: &str, rows: &[wire::setting_change::Reader<'_>]) -> String {
    let mut out = String::new();
    let mut o = json::Obj::new(&mut out);
    o.key(key).sub(|out| {
        out.push('[');
        for (index, row) in rows.iter().enumerate() {
            json::sep(out, index == 0);
            let mut entry = json::Obj::new(out);
            entry.key("domain").str(&text(row.get_domain()));
            entry.key("key").str(&text(row.get_key()));
            entry.key("value").sub(|out| write_value(out, &row.get_value()));
            entry
                .key("timestamp")
                .sub(|out| {
                    let mut t = json::Obj::new(out);
                    t.key("nanoseconds").int(
                        row.get_timestamp().map(|stamp| stamp.get_nanoseconds()).unwrap_or(0),
                    );
                    t.end();
                });
            entry.key("actorId").str(&text(row.get_actor_id()));
            entry.key("version").int(row.get_version());
            entry.end();
        }
        out.push(']');
    });
    o.end();
    out
}

/// Put a raw value into the contract's union: a string unless it reads as a
/// number or a boolean — the contract's own arms, the caller's own spelling.
fn set_value(mut value: wire::setting_value::Builder<'_>, raw: &str) {
    if raw == "true" || raw == "false" {
        value.set_boolean(raw == "true");
        return;
    }
    if let Ok(number) = raw.parse::<i64>() {
        value.set_integer(number);
        return;
    }
    if let Ok(number) = raw.parse::<f64>() {
        value.set_real(number);
        return;
    }
    value.set_string(raw);
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
