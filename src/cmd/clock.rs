//! `clock` — the measured state of this host's clocks, the timers, the
//! alarms, the virtual clocks: the coupled shell's presentation, re-keyed from
//! the engine's mirror types (`ClockInfo`, `ClockSyncInfo`, `TimerSpec`,
//! `Alarm`, `VirtualClockHandle`) to the CONTRACT'S RESULT READERS
//! (`clock.capnp`'s `info` / `realtimeNow` / `monotonicNow` /
//! `listClockDomains` / `getClockSync` / `getClockQuality` / `getClockDrift`).
//!
//! ## What the re-keying changed
//!
//! * the engine's `ClockInfo` mirror became the contract's `ClockInfo`
//!   reader — the same fields, read from the wire;
//! * the shell's `Value` records became the contract's own JSON (the field
//!   names are the schema's, verbatim);
//! * the shell's **session ledger** (the timers a session armed, with the
//!   measured OWNER column) has no contract counterpart: the contract's
//!   `listPersistedTimers` is the store's own list, so that is what is
//!   presented, and an empty store is an honest empty;
//! * the shell's `caps` view has no contract method (the capability tokens
//!   live in discovery's descriptor metadata), so it is not on this surface.
//!
//! The registry publishes thirty-two paths, including the alarm surface
//! (`clock alarms` acquires the capability; `clock alarm list` and its
//! siblings ride it) and the subscription's own `pull` / `cancel`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json;
use crate::present;
use crate::schema::clock_capnp as wire;
use crate::schema::clock_capnp::ClockDomain;
use crate::transport as tp;

use super::domain::{self, Presenter};
use super::App;

/// The schema stem the family presents.
const STEM: &str = "clock";
/// The contract interface the family presents.
const INTERFACE: &str = "ClockService";

/// The family's presenter lookup: keyed by **(schema id, method name)**.
pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != crate::contract::schema_id(STEM)? {
        return None;
    }
    Some(match method {
        "info" => info,
        "realtimeNow" => now,
        "monotonicNow" => monotonic,
        "listClockDomains" => domains,
        "getClockSync" => sync,
        "getClockQuality" => quality,
        "getClockDrift" => drift,
        "sleep" => sleep,
        "sleepCancellable" => sleep_cancellable,
        "createTimer" => timer_create,
        "checkTimer" => timer_check,
        "resetTimer" => timer_reset,
        "cancelTimer" => timer_cancel,
        "listPersistedTimers" => timers,
        "persistTimer" => persist_timer,
        "setTimerDependency" => timer_dependency,
        "subscribeTimers" => watch,
        "pull" => subscription_pull,
        "pullPaginated" => subscription_pull,
        "cancel" => subscription_cancel,
        "alarms" => acquire_alarms,
        "listAlarms" => alarm_list,
        "getNextAlarm" => alarm_next,
        "getAlarm" => alarm_get,
        "createAlarm" => alarm_create,
        "updateAlarm" => alarm_update,
        "snoozeAlarm" => alarm_snooze,
        "dismissAlarm" => alarm_dismiss,
        "deleteAlarm" => alarm_delete,
        "createVirtualClock" => virtual_create,
        "advanceVirtualClock" => virtual_advance,
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
// `clock info` — the measured state of this host's clocks.
// ═════════════════════════════════════════════════════════════════════════

fn info(app: &mut App, args: &[String], path: &str) -> i32 {
    // The service publishes three paths against this one method ("clock
    // info", "clock uptime", "clock security"); the founder's shell had
    // three views for the same reading, so each path keeps its own view.
    if path.ends_with("uptime") {
        return uptime(app, args, path);
    }
    if path.ends_with("security") {
        return security(app, args, path);
    }
    if path.ends_with("resolution") {
        return resolution(app, args, path);
    }
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::info_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "info", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clock_service::info_results::Owned, _, _>(
        &bytes,
        |results| {
            let Ok(info) = results.get_info() else {
                return (
                    String::from("{\"error\":{\"kind\":\"failed\",\"message\":\"the service answered no clock state\"}}"),
                    format!("  {} the service answered no clock state\n", domain::red(app, "✗")),
                );
            };
            (info_json(&info), info_human(app, &info))
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The shell's `Clock` tree: the kernel's own clocks, measured.
fn info_human(app: &App, info: &wire::clock_info::Reader<'_>) -> String {
    let realtime = nanos(&info.get_realtime());
    let monotonic = nanos(&info.get_monotonic());
    let boot = nanos(&info.get_boot_time());
    let resolution = duration_nanos(&info.get_resolution());
    let uptime = info
        .get_realtime()
        .map(|t| t.get_nanoseconds())
        .unwrap_or(0)
        .saturating_sub(info.get_boot_time().map(|t| t.get_nanoseconds()).unwrap_or(0));
    let sync = info.get_sync().ok();
    let security = info.get_security().ok();
    let domains = info
        .get_domains()
        .map(|list| list.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} — the kernel's own clocks\n",
        domain::cyan(app, "🕰"),
        domain::bold(app, "Clock"),
        domain::dim(app, &format!("({})", text(info.get_source_name())))
    ));
    out.push_str(&format!("  ├─ Realtime    {realtime} (CLOCK_REALTIME)\n"));
    out.push_str(&format!(
        "  ├─ Monotonic   {monotonic} (CLOCK_MONOTONIC — immune to NTP and DST)\n"
    ));
    out.push_str(&format!(
        "  ├─ Uptime      {} since boot (btime {boot})\n",
        duration_label(uptime)
    ));
    out.push_str(&format!(
        "  ├─ Resolution  {resolution} ns (clock_getres — never a fabricated precision)\n"
    ));
    let (synced, source) = match &sync {
        Some(sync) => (sync.get_synced(), text(sync.get_source())),
        None => (false, String::new()),
    };
    out.push_str(&format!(
        "  ├─ Sync        {} {} {}\n",
        if synced {
            domain::green(app, "◉")
        } else {
            domain::dim(app, "○")
        },
        source,
        if synced {
            String::new()
        } else {
            "(measured — see `clock sync`)".to_string()
        }
    ));
    if let Some(security) = &security {
        out.push_str(&format!(
            "  ├─ Security    tampered {} · secure source {} · hardware rng {}\n",
            yes_no(app, security.get_tampered()),
            yes_no(app, security.get_secure_source()),
            yes_no(app, security.get_hardware_rng())
        ));
    }
    out.push_str(&format!("  └─ Domains     {} served", domains.len()));
    out
}

/// The contract-shaped JSON of the clock state.
fn info_json(info: &wire::clock_info::Reader<'_>) -> String {
    let mut out = String::from("{\"info\":");
    let mut o = json::Obj::new(&mut out);
    o.key("monotonic")
        .sub(|out| timestamp(out, &info.get_monotonic()));
    o.key("realtime").sub(|out| timestamp(out, &info.get_realtime()));
    o.key("resolution")
        .sub(|out| duration(out, &info.get_resolution()));
    o.key("sourceName").str(&text(info.get_source_name()));
    o.key("bootTime").sub(|out| timestamp(out, &info.get_boot_time()));
    o.key("sync").sub(|out| {
        let mut s = json::Obj::new(out);
        match info.get_sync() {
            Ok(sync) => {
                s.key("synced").boolean(sync.get_synced());
                s.key("source").str(&text(sync.get_source()));
                s.key("stratum").int(sync.get_stratum());
                s.key("lastSync").sub(|out| timestamp(out, &sync.get_last_sync()));
                s.key("offset").sub(|out| duration(out, &sync.get_offset()));
            }
            Err(_) => {
                s.key("synced").boolean(false);
            }
        }
        s.end();
    });
    o.key("security").sub(|out| {
        let mut s = json::Obj::new(out);
        match info.get_security() {
            Ok(security) => {
                s.key("tampered").boolean(security.get_tampered());
                s.key("secureSource").boolean(security.get_secure_source());
                s.key("hardwareRng").boolean(security.get_hardware_rng());
            }
            Err(_) => {
                s.key("tampered").boolean(false);
            }
        }
        s.end();
    });
    o.key("domains").sub(|out| {
        out.push('[');
        if let Ok(list) = info.get_domains() {
            for (index, entry) in list.iter().enumerate() {
                json::sep(out, index == 0);
                let mut d = json::Obj::new(out);
                d.key("domain").str(domain_token(entry.get_domain().unwrap_or(ClockDomain::Realtime)));
                d.key("name").str(&text(entry.get_name()));
                d.key("resolution").sub(|out| duration(out, &entry.get_resolution()));
                d.key("available").boolean(entry.get_available());
                d.end();
            }
        }
        out.push(']');
    });
    o.end();
    out
}

// ═════════════════════════════════════════════════════════════════════════
// `clock now` / `clock monotonic` — the two readings.
// ═════════════════════════════════════════════════════════════════════════

fn now(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::realtime_now_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "realtimeNow", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::clock_service::realtime_now_results::Owned, _, _>(&bytes, |results| {
            let stamp = results.get_time();
            let reading = nanos(&stamp);
            (
                {
                    let mut out = String::from("{\"time\":");
                    timestamp(&mut out, &stamp);
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "🕐"),
                    domain::bold(app, "Now"),
                    domain::bold(app, &reading),
                    domain::dim(app, "— the kernel's own CLOCK_REALTIME, no `date` fork")
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn monotonic(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::monotonic_now_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "monotonicNow", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::monotonic_now_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let stamp = results.get_time();
            let reading = nanos(&stamp);
            (
                {
                    let mut out = String::from("{\"time\":");
                    timestamp(&mut out, &stamp);
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "⏱"),
                    domain::bold(app, "Monotonic"),
                    domain::bold(app, &reading),
                    domain::dim(app, "— CLOCK_MONOTONIC, immune to NTP and DST")
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `clock uptime` / `clock security` — the two views the shell took from the
// same reading, presented from the same one.
// ═════════════════════════════════════════════════════════════════════════

fn uptime(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::info_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "info", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clock_service::info_results::Owned, _, _>(
        &bytes,
        |results| {
            let info = results.get_info().ok();
            let boot = info
                .as_ref()
                .map(|info| nanos(&info.get_boot_time()))
                .unwrap_or_default();
            let seconds = info
                .as_ref()
                .map(|info| {
                    info.get_realtime()
                        .map(|t| t.get_nanoseconds())
                        .unwrap_or(0)
                        .saturating_sub(info.get_boot_time().map(|t| t.get_nanoseconds()).unwrap_or(0))
                })
                .unwrap_or(0);
            (
                {
                    let mut out = String::from("{\"uptime\":");
                    json::Obj::new(&mut out)
                        .key("nanoseconds")
                        .int(seconds);
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {} — btime {} from /proc/stat",
                    domain::cyan(app, "⏳"),
                    domain::bold(app, "Uptime"),
                    domain::bold(app, &duration_label(seconds)),
                    domain::dim(app, ""),
                    boot
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// `clock resolution` — the host's own clock_getres answer. The service
/// publishes this path against the state read, so this is the resolution the
/// state carries (the per-domain table is `clock domains`).
fn resolution(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::info_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "info", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clock_service::info_results::Owned, _, _>(
        &bytes,
        |results| {
            let resolution = results
                .get_info()
                .map(|info| info.get_resolution().map(|d| d.get_nanoseconds()).unwrap_or(0))
                .unwrap_or(0);
            (
                format!("{{\"resolution\":{{\"nanoseconds\":{resolution}}}}}"),
                format!(
                    "  {} {} {} ns — clock_getres, measured\n  └─ Source     the host's own \
                     clock_getres(2) — never a fabricated precision",
                    domain::cyan(app, "📐"),
                    domain::bold(app, "Resolution"),
                    resolution
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn security(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::info_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "info", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clock_service::info_results::Owned, _, _>(
        &bytes,
        |results| {
            let security = results.get_info().ok().and_then(|info| info.get_security().ok());
            (
                {
                    let mut out = String::from("{\"security\":");
                    let mut o = json::Obj::new(&mut out);
                    match &security {
                        Some(security) => {
                            o.key("tampered").boolean(security.get_tampered());
                            o.key("secureSource").boolean(security.get_secure_source());
                            o.key("hardwareRng").boolean(security.get_hardware_rng());
                        }
                        None => {
                            o.key("tampered").boolean(false);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &security {
                    Some(security) => format!(
                        "  {} {} tampered {} · secure source {} · hardware rng {}",
                        domain::cyan(app, "🛡"),
                        domain::bold(app, "Clock security"),
                        yes_no(app, security.get_tampered()),
                        yes_no(app, security.get_secure_source()),
                        yes_no(app, security.get_hardware_rng())
                    ),
                    None => format!(
                        "  {} {} — the service answered no security state",
                        domain::cyan(app, "🛡"),
                        domain::bold(app, "Clock security")
                    ),
                },
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `clock resolution` / `clock domains` — the clock domains, one row each.
// ═════════════════════════════════════════════════════════════════════════

fn domains(app: &mut App, _args: &[String], path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::list_clock_domains_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listClockDomains", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::list_clock_domains_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let rows: Vec<wire::clock_domain_info::Reader<'_>> = results
                .get_domains()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"domains\":[");
                    for (index, entry) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut d = json::Obj::new(&mut out);
                        d.key("domain").str(domain_token(entry.get_domain().unwrap_or(ClockDomain::Realtime)));
                        d.key("name").str(&text(entry.get_name()));
                        d.key("resolution").sub(|out| duration(out, &entry.get_resolution()));
                        d.key("available").boolean(entry.get_available());
                        d.end();
                    }
                    out.push_str("]}");
                    out
                },
                if path.ends_with("resolution") {
                    domains_human(
                        app,
                        &rows,
                        "📐",
                        "Resolution",
                        "— clock_getres, measured",
                    )
                } else {
                    domains_human(app, &rows, "🗄", "Clock domains", "— what this host can serve")
                },
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The clock-domain table: DOMAIN, NAME, RESOLUTION, AVAILABLE.
fn domains_human(
    app: &App,
    rows: &[wire::clock_domain_info::Reader<'_>],
    glyph: &str,
    title: &str,
    note: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} {}\n",
        domain::cyan(app, glyph),
        domain::bold(app, title),
        domain::dim(app, &format!("({})", rows.len())),
        domain::dim(app, note)
    ));
    if rows.is_empty() {
        out.push_str(&format!(
            "  └─ {} this host serves no clock domain (an honest empty)\n",
            domain::dim(app, "○")
        ));
        return out.trim_end().to_string();
    }
    let mut table = present::Table::new(vec![
        String::from("DOMAIN"),
        String::from("NAME"),
        String::from("RESOLUTION"),
        String::from("AVAILABLE"),
    ]);
    for entry in rows {
        let resolution = entry
            .get_resolution()
            .map(|d| d.get_nanoseconds())
            .unwrap_or(0);
        table = table.row(vec![
            present::Cell::plain(domain_token(entry.get_domain().unwrap_or(ClockDomain::Realtime))),
            present::Cell::plain(text(entry.get_name())),
            present::Cell::plain(format!("{resolution} ns")),
            domain::yes_no(entry.get_available()),
        ]);
    }
    out.push_str(&table.render(app.style()));
    out.trim_end().to_string()
}

// ═════════════════════════════════════════════════════════════════════════
// `clock sync` / `clock quality` / `clock drift` — the measured verdicts.
// ═════════════════════════════════════════════════════════════════════════

fn sync(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::get_clock_sync_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getClockSync", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::clock_service::get_clock_sync_results::Owned, _, _>(&bytes, |results| {
            let sync = results.get_sync().ok();
            let synced = sync.as_ref().map(|s| s.get_synced()).unwrap_or(false);
            (
                {
                    let mut out = String::from("{\"sync\":");
                    let mut o = json::Obj::new(&mut out);
                    match &sync {
                        Some(sync) => {
                            o.key("synced").boolean(sync.get_synced());
                            o.key("source").str(&text(sync.get_source()));
                            o.key("stratum").int(sync.get_stratum());
                            o.key("lastSync").sub(|out| timestamp(out, &sync.get_last_sync()));
                            o.key("offset").sub(|out| duration(out, &sync.get_offset()));
                        }
                        None => {
                            o.key("synced").boolean(false);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {}\n",
                        domain::cyan(app, "📡"),
                        domain::bold(app, "Clock sync"),
                        if synced {
                            domain::green(app, "◉ synchronized")
                        } else {
                            domain::dim(app, "○ not synchronized")
                        },
                        domain::dim(app, "— measured, never assumed")
                    ));
                    if let Some(sync) = &sync {
                        let last = sync
                            .get_last_sync()
                            .map(|t| t.get_nanoseconds())
                            .unwrap_or(0);
                        let offset = sync.get_offset().map(|d| d.get_nanoseconds()).unwrap_or(0);
                        out.push_str(&format!(
                            "  ├─ Source      {} · stratum {}\n",
                            text(sync.get_source()),
                            sync.get_stratum()
                        ));
                        out.push_str(&format!(
                            "  └─ Reading     last sync {last} · offset {offset} ns"
                        ));
                    }
                    out
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn quality(app: &mut App, args: &[String], _path: &str) -> i32 {
    let source = domain::flag(args, "--source").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::get_clock_quality_params::Builder<'_>>();
        params.set_source_name(&source);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getClockQuality", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::get_clock_quality_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let quality = results.get_quality().ok();
            let accuracy = quality
                .as_ref()
                .map(|q| q.get_accuracy().map(|d| d.get_nanoseconds()).unwrap_or(0))
                .unwrap_or(0);
            let stability = quality
                .as_ref()
                .map(|q| q.get_stability().map(|d| d.get_nanoseconds()).unwrap_or(0))
                .unwrap_or(0);
            let reliability = quality.as_ref().map(|q| q.get_reliability()).unwrap_or(0.0);
            let samples = quality.as_ref().map(|q| q.get_sample_count()).unwrap_or(0);
            let name = quality
                .as_ref()
                .map(|q| text(q.get_source_name()))
                .unwrap_or_else(|| source.clone());
            (
                {
                    let mut out = String::from("{\"quality\":");
                    let mut o = json::Obj::new(&mut out);
                    match &quality {
                        Some(quality) => {
                            o.key("sourceName").str(&text(quality.get_source_name()));
                            o.key("accuracy").sub(|out| duration(out, &quality.get_accuracy()));
                            o.key("stability").sub(|out| duration(out, &quality.get_stability()));
                            o.key("reliability").float(reliability as f64);
                            o.key("measuredAt").sub(|out| timestamp(out, &quality.get_measured_at()));
                            o.key("sampleCount").int(samples);
                        }
                        None => {
                            o.key("sourceName").str(&source);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} — accuracy {accuracy} ns · stability {stability} ns · reliability \
                     {reliability} over {samples} sample(s)",
                    domain::cyan(app, "📊"),
                    domain::bold(app, "Clock quality"),
                    domain::bold(app, &name)
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn drift(app: &mut App, args: &[String], _path: &str) -> i32 {
    let positionals = domain::positionals(args);
    let first = positionals
        .first()
        .map(|d| (*d).to_string())
        .or_else(|| domain::flag(args, "--domain"))
        .unwrap_or_else(|| "realtime".to_string());
    let second = positionals
        .get(1)
        .map(|d| (*d).to_string())
        .or_else(|| domain::flag(args, "--to"))
        .unwrap_or_else(|| "monotonic".to_string());
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::get_clock_drift_params::Builder<'_>>();
        params.set_domain1(clock_domain_of(&first));
        params.set_domain2(clock_domain_of(&second));
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getClockDrift", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::get_clock_drift_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let drift = results.get_drift().ok();
            let measured = drift
                .as_ref()
                .map(|d| d.get_drift().map(|d| d.get_nanoseconds()).unwrap_or(0))
                .unwrap_or(0);
            let accuracy = drift
                .as_ref()
                .map(|d| d.get_accuracy().map(|d| d.get_nanoseconds()).unwrap_or(0))
                .unwrap_or(0);
            (
                {
                    let mut out = String::from("{\"drift\":");
                    let mut o = json::Obj::new(&mut out);
                    match &drift {
                        Some(drift) => {
                            o.key("drift").sub(|out| duration(out, &drift.get_drift()));
                            o.key("measuredAt").sub(|out| timestamp(out, &drift.get_measured_at()));
                            o.key("accuracy").sub(|out| duration(out, &drift.get_accuracy()));
                        }
                        None => {
                            o.key("drift").str("unmeasured");
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} → {} — drift {measured} ns · accuracy {accuracy} ns",
                    domain::cyan(app, "⇄"),
                    domain::bold(app, "Clock drift"),
                    first,
                    second
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `clock sleep` / `clock sleep-cancellable` — the host's own nanosleep.
// ═════════════════════════════════════════════════════════════════════════

fn sleep(app: &mut App, args: &[String], _path: &str) -> i32 {
    let millis = domain::number::<u64>(args, "--duration", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::sleep_params::Builder<'_>>();
        params.reborrow().init_duration().set_nanoseconds(millis * 1_000_000);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "sleep", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<crate::schema::types_capnp::void_result::Owned, _, _>(&bytes, |r| {
            let ok = matches!(
                r.which(),
                Ok(crate::schema::types_capnp::void_result::Which::Success(_))
            );
            (
                format!("{{\"slept\":{millis},\"accepted\":{ok}}}"),
                format!(
                    "  {} {} {} ms {} — nanosleep on the caller's own thread",
                    domain::cyan(app, "😴"),
                    domain::bold(app, "Sleep"),
                    millis,
                    if ok { "◉" } else { "✗" }
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn sleep_cancellable(app: &mut App, args: &[String], _path: &str) -> i32 {
    let millis = domain::number::<u64>(args, "--duration", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::sleep_cancellable_params::Builder<'_>>();
        params.reborrow().init_duration().set_nanoseconds(millis * 1_000_000);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "sleepCancellable", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::sleep_cancellable_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let handle = results.get_handle().ok();
            let id = handle.as_ref().map(|h| h.get_id()).unwrap_or(0);
            (
                format!("{{\"handle\":{{\"id\":{id}}}}}"),
                format!(
                    "  {} {} {} ms #{} — a cancellable sleep on the caller's own thread",
                    domain::cyan(app, "😴"),
                    domain::bold(app, "Sleep"),
                    millis,
                    id
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// The timer surface: create, check, reset, cancel, the persisted list.
// ═════════════════════════════════════════════════════════════════════════

fn timer_create(app: &mut App, args: &[String], _path: &str) -> i32 {
    let label = domain::flag(args, "--label").unwrap_or_default();
    let after = domain::number::<u64>(args, "--after", 0);
    let every = domain::number::<u64>(args, "--every", 0);
    let clock = domain::flag(args, "--clock").unwrap_or_else(|| "realtime".to_string());
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::create_timer_params::Builder<'_>>();
        let mut spec = params.reborrow().init_spec();
        spec.set_label(&label);
        spec.reborrow().init_anchor().set_nanoseconds(after * 1_000_000);
        {
            let mut anchor_type = spec.reborrow().init_anchor_type();
            anchor_type
                .reborrow()
                .init_relative()
                .set_nanoseconds(after * 1_000_000);
        }
        spec.reborrow().init_interval().set_nanoseconds(every * 1_000_000);
        spec.set_clock_domain(clock_domain_of(&clock));
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "createTimer", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::create_timer_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let timer = results.get_timer().ok();
            let id = timer
                .as_ref()
                .map(|t| t.get_id().map(|id| id.get_id()).unwrap_or(0))
                .unwrap_or(0);
            let label = timer.as_ref().map(|t| text(t.get_label())).unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"timer\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("id").int(id);
                    o.key("label").str(&label);
                    o.end();
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} #{} {} — a real timerfd this process owns\n",
                        domain::cyan(app, "⏲"),
                        domain::bold(app, "Timer"),
                        domain::bold(app, &id.to_string()),
                        domain::dim(app, &format!("({label})"))
                    ));
                    out.push_str(&format!(
                        "  ├─ Anchor      {after} ms from now ({clock})\n"
                    ));
                    out.push_str(&format!(
                        "  ├─ Interval    {every} ms {}\n",
                        if every > 0 { "(periodic)" } else { "(one-shot)" }
                    ));
                    out.push_str(&format!(
                        "  └─ Source      the contract's own timer service — the kernel's own \
                         timerfd, never a fabricated firing"
                    ));
                    out
                },
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn timer_check(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::check_timer_params::Builder<'_>>();
        params.reborrow().init_timer().reborrow().init_id().set_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "checkTimer", &params) {
        Ok(bytes) => bytes,
        Err(error) => {
            // A timerfd belongs to the process that armed it: the host's own
            // notFound with the reason beside it, never a fabricated fire.
            return super::emit_error(app, &error);
        }
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::clock_service::check_timer_results::Owned, _, _>(&bytes, |results| {
            let fired = results.get_fired();
            (
                format!("{{\"fired\":{fired}}}"),
                format!(
                    "  {} {} {} — the kernel's own expiration count",
                    domain::cyan(app, "⏲"),
                    domain::bold(app, &format!("Timer {id}")),
                    if fired {
                        domain::green(app, "◉ fired")
                    } else {
                        domain::dim(app, "○ not yet")
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

fn timer_reset(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let label = domain::flag(args, "--label").unwrap_or_default();
    let every = domain::number::<u64>(args, "--every", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::reset_timer_params::Builder<'_>>();
        {
            let mut timer = params.reborrow().init_timer();
            timer.reborrow().init_id().set_id(id);
            timer.set_label(&label);
        }
        let mut spec = params.reborrow().init_spec();
        spec.set_label(&label);
        spec.reborrow().init_interval().set_nanoseconds(every * 1_000_000);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "resetTimer", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::reset_timer_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let timer = results.get_timer().ok();
            let timer_id = timer
                .as_ref()
                .map(|t| t.get_id().map(|id| id.get_id()).unwrap_or(0))
                .unwrap_or(0);
            (
                format!("{{\"timer\":{{\"id\":{timer_id}}}}}"),
                format!(
                    "  {} {} #{} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, "Timer"),
                    domain::bold(app, &timer_id.to_string()),
                    domain::dim(app, "— the engine's own path, accepted")
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn timer_cancel(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::cancel_timer_params::Builder<'_>>();
        params.reborrow().init_timer().reborrow().init_id().set_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "cancelTimer", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        crate::schema::types_capnp::void_result::Owned,
        _,
        _,
    >(&bytes, |r| {
        let ok = matches!(
            r.which(),
            Ok(crate::schema::types_capnp::void_result::Which::Success(_))
        );
        (
            format!("{{\"cancelled\":{ok},\"id\":{id}}}"),
            format!(
                "  {} {} #{} {}",
                domain::green(app, "✎"),
                domain::bold(app, "Timer"),
                domain::bold(app, &format!("{id} disarmed")),
                domain::dim(app, "— the engine's own path, accepted")
            ),
        )
    }) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn timers(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::list_persisted_timers_params::Builder<'_>>();
        params.reborrow().init_pagination().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listPersistedTimers", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::list_persisted_timers_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let rows: Vec<wire::timer_handle::Reader<'_>> = results
                .get_timers()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total_count();
            (
                {
                    let mut out = String::from("{\"timers\":[");
                    for (index, timer) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut t = json::Obj::new(&mut out);
                        t.key("id")
                            .int(timer.get_id().map(|id| id.get_id()).unwrap_or(0));
                        t.key("label").str(&text(timer.get_label()));
                        t.end();
                    }
                    out.push_str("],\"totalCount\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — the store's own persisted timers\n",
                        domain::cyan(app, "⏱"),
                        domain::bold(app, "Timers"),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the store holds no persisted timer (an honest empty — the \
                             store was read)\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("ID"),
                            String::from("LABEL"),
                        ]);
                        for timer in &rows {
                            table = table.row(vec![
                                present::Cell::plain(
                                    timer.get_id().map(|id| id.get_id()).unwrap_or(0).to_string(),
                                ),
                                present::truncate_to(&text(timer.get_label()), 40).into(),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                    }
                    out.trim_end().to_string()
                },
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn persist_timer(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let name = domain::flag(args, "--label").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::persist_timer_params::Builder<'_>>();
        {
            let mut timer = params.reborrow().init_timer();
            timer.reborrow().init_id().set_id(id);
            timer.set_label(&name);
        }
        params.reborrow().init_persistence().set_name(&name);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "persistTimer", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    void_verb(app, "persistTimer", "persist-timer", &format!("{id} {name}"), &bytes)
}

fn timer_dependency(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let depends = domain::number::<u64>(args, "--depends-on", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::set_timer_dependency_params::Builder<'_>>();
        params.reborrow().init_timer().reborrow().init_id().set_id(id);
        params
            .reborrow()
            .init_depends_on()
            .reborrow()
            .init_id()
            .set_id(depends);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "setTimerDependency", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    void_verb(
        app,
        "setTimerDependency",
        "set-timer-dependency",
        &format!("{id} depends on {depends}"),
        &bytes,
    )
}

// ═════════════════════════════════════════════════════════════════════════
// The subscription: `clock timer pull` / `clock subscription cancel` — the
// contract's own TimerSubscription, pulled through a subscription this client
// mints (the service's path names the subscription's own methods).
// ═════════════════════════════════════════════════════════════════════════

fn subscription_pull(app: &mut App, args: &[String], _path: &str) -> i32 {
    let count = domain::number::<u32>(args, "--count", 1);
    let interval = domain::number::<u64>(args, "--interval", 400);
    let subscription = match subscribe_timers(app) {
        Ok(subscription) => subscription,
        Err(error) => return super::emit_error(app, &error),
    };
    let json_mode = app.json();
    let quiet = app.quiet();
    let mut delivered = 0u32;
    let mut idle = 0u32;
    while delivered < count.max(1) && idle < 3 {
        let payload = match pull_timer_events(app, &subscription, count.max(1) - delivered) {
            Ok(payload) => payload,
            Err(error) => {
                let _ = subscription.cancel(app.conn());
                return super::emit_error(app, &error);
            }
        };
        let batch = tp::with_root::<wire::timer_subscription::pull_results::Owned, _, _>(
            &payload,
            |r| {
                let mut count = 0u32;
                if let Ok(list) = r.get_events() {
                    for event in list.iter() {
                        if json_mode {
                            let mut out = String::new();
                            let mut o = json::Obj::new(&mut out);
                            o.key("timer")
                                .int(event.get_timer().map(|t| t.get_id().map(|id| id.get_id()).unwrap_or(0)).unwrap_or(0));
                            o.key("label")
                                .str(&event.get_timer().map(|t| text(t.get_label())).unwrap_or_default());
                            o.key("firedAt")
                                .int(event.get_fired_at().map(|t| t.get_nanoseconds()).unwrap_or(0));
                            o.key("count").int(event.get_count());
                            o.end();
                            println!("{out}");
                        } else if !quiet {
                            println!(
                                "  {} {:<20} {:<24} {}",
                                domain::green(app, "◉"),
                                present::truncate_to(
                                    &event.get_timer().map(|t| text(t.get_label())).unwrap_or_default(),
                                    20
                                ),
                                event.get_fired_at().map(|t| t.get_nanoseconds()).unwrap_or(0),
                                event.get_count()
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

fn subscription_cancel(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let subscription = match subscribe_timers(app) {
        Ok(subscription) => subscription,
        Err(error) => return super::emit_error(app, &error),
    };
    match subscription.cancel(app.conn()) {
        Ok(()) => {
            let human = format!(
                "  {} {} {}",
                domain::green(app, "✎"),
                domain::bold(app, "Subscription"),
                domain::dim(app, "cancelled — the contract's own cancel")
            );
            print_answer(app, String::from("{\"cancelled\":true}"), human)
        }
        Err(error) => super::emit_error(app, &error),
    }
}

/// The shell's `clock watch` — the timer event stream.
fn watch(app: &mut App, args: &[String], path: &str) -> i32 {
    subscription_pull(app, args, path)
}

/// Mint the contract's own timer subscription.
fn subscribe_timers(
    app: &mut App,
) -> Result<crate::transport::Subscription, crate::transport::CallError> {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::subscribe_timers_params::Builder<'_>>();
    });
    let (interface_id, method_id) = super::ids_in(STEM, INTERFACE, "subscribeTimers").ok_or_else(
        || {
            crate::transport::CallError::Transport(
                "the contract has no `subscribeTimers` method in this build".to_string(),
            )
        },
    )?;
    let reader = tp::decode_message::<wire::clock_service::subscribe_timers_params::Owned>(&params)
        .map_err(crate::transport::CallError::Transport)?;
    let call_params = reader
        .get()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    let response = app.conn().call_typed::<
        wire::clock_service::subscribe_timers_params::Owned,
        wire::clock_service::subscribe_timers_results::Owned,
    >(interface_id, method_id, call_params)?;
    let results = response
        .get()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    let capability = results
        .get_subscription()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    Ok(crate::transport::Subscription::new(
        capability.client,
        "clock",
        "TimerSubscription",
    ))
}

/// The subscription's own bounded pull.
fn pull_timer_events(
    app: &mut App,
    subscription: &crate::transport::Subscription,
    max: u32,
) -> Result<Vec<u8>, crate::transport::CallError> {
    let info = crate::contract::interface_by_name("clock", "TimerSubscription").ok_or_else(|| {
        crate::transport::CallError::Transport(
            "the contract interface `TimerSubscription` is not compiled into this build".to_string(),
        )
    })?;
    let method_id = crate::contract::method_id("clock", "TimerSubscription", "pull").ok_or_else(|| {
        crate::transport::CallError::Transport(
            "the contract interface `TimerSubscription` has no `pull` method in this build"
                .to_string(),
        )
    })?;
    let _ = max;
    app.conn()
        .call_capability(subscription.client(), info.type_id, method_id, &[])
}

// ═════════════════════════════════════════════════════════════════════════
// The alarm surface: the capability, and the verbs that ride it.
// ═════════════════════════════════════════════════════════════════════════

/// `clock alarms` — the alarm service is a CAPABILITY, and a capability does
/// not survive the byte path: this is a typed call, and the answer is the
/// capability itself (holding it IS the authority).
fn acquire_alarms(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::clock_service::alarms_params::Builder<'_>>();
    });
    let (interface_id, method_id) = match super::ids_in(STEM, INTERFACE, "alarms") {
        Some(ids) => ids,
        None => {
            super::usage_error_app(app, "the contract has no `alarms` method in this build");
            return crate::registry::EXIT_USAGE;
        }
    };
    let reader = match tp::decode_message::<wire::clock_service::alarms_params::Owned>(&params) {
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
        wire::clock_service::alarms_params::Owned,
        wire::clock_service::alarms_results::Owned,
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
    let acquired = results.get_service().is_ok();
    let human = format!(
        "  {} {} {}",
        domain::cyan(app, "⏰"),
        domain::bold(app, "Alarms"),
        if acquired {
            domain::green(app, "◉ the alarm service is acquired — holding it IS the authority")
        } else {
            domain::red(app, "✗ the service answered no alarm capability")
        }
    );
    print_answer(
        app,
        format!("{{\"alarms\":{{\"acquired\":{acquired}}}}}"),
        human,
    )
}

/// Call one method of the alarm service, through the capability the service
/// hands out.
fn alarm_call(
    app: &mut App,
    method: &str,
    params: &[u8],
) -> Result<Vec<u8>, crate::transport::CallError> {
    let (interface_id, method_id) = super::ids_in(STEM, INTERFACE, "alarms").ok_or_else(|| {
        crate::transport::CallError::Transport(
            "the contract has no `alarms` method in this build".to_string(),
        )
    })?;
    let reader = tp::decode_message::<wire::clock_service::alarms_params::Owned>(&params)
        .map_err(crate::transport::CallError::Transport)?;
    let call_params = reader
        .get()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    let response = app.conn().call_typed::<
        wire::clock_service::alarms_params::Owned,
        wire::clock_service::alarms_results::Owned,
    >(interface_id, method_id, call_params)?;
    let results = response
        .get()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    let capability = results
        .get_service()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    let info = crate::contract::interface_by_name("clock", "AlarmService").ok_or_else(|| {
        crate::transport::CallError::Transport(
            "the contract interface `AlarmService` is not compiled into this build".to_string(),
        )
    })?;
    let alarm_method = crate::contract::method_id("clock", "AlarmService", method).ok_or_else(|| {
        crate::transport::CallError::Transport(format!(
            "the contract interface `AlarmService` has no `{method}` method in this build"
        ))
    })?;
    app.conn()
        .call_capability(&capability.client, info.type_id, alarm_method, params)
}

/// The alarm list: the store's own alarms, through the acquired capability.
fn alarm_list(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let mut params = m.init_root::<wire::alarm_service::list_alarms_params::Builder<'_>>();
        params.reborrow().init_pagination().set_limit(u32::MAX);
    });
    let bytes = match alarm_call(app, "listAlarms", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::alarm_service::list_alarms_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::alarm::Reader<'_>> = results
                .get_alarms()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total_count();
            (
                {
                    let mut out = String::from("{\"alarms\":[");
                    for (index, alarm) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut a = json::Obj::new(&mut out);
                        a.key("id")
                            .int(alarm.get_id().map(|id| id.get_id()).unwrap_or(0));
                        a.key("label").str(&text(alarm.get_label()));
                        a.key("fireTime")
                            .int(alarm.get_fire_time().map(|t| t.get_nanoseconds()).unwrap_or(0));
                        a.key("enabled").boolean(alarm.get_enabled());
                        a.end();
                    }
                    out.push_str("],\"totalCount\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — this process's own store, through the acquired capability\n",
                        domain::cyan(app, "⏰"),
                        domain::bold(app, "Alarms"),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the store holds no alarm (an honest empty — the store was \
                             read)\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("ID"),
                            String::from("LABEL"),
                            String::from("FIRE TIME"),
                            String::from("ENABLED"),
                        ]);
                        for alarm in &rows {
                            let fire = alarm.get_fire_time().map(|t| t.get_nanoseconds()).unwrap_or(0);
                            table = table.row(vec![
                                present::Cell::plain(
                                    alarm.get_id().map(|id| id.get_id()).unwrap_or(0).to_string(),
                                ),
                                present::truncate_to(&text(alarm.get_label()), 28).into(),
                                present::Cell::plain(fire.to_string()),
                                domain::yes_no(alarm.get_enabled()),
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

/// The next alarm that will fire.
fn alarm_next(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::alarm_service::get_next_alarm_params::Builder<'_>>();
    });
    let bytes = match alarm_call(app, "getNextAlarm", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::alarm_service::get_next_alarm_results::Owned, _, _>(&bytes, |results| {
            let alarm = results.get_alarm().ok();
            let label = alarm.as_ref().map(|a| text(a.get_label())).unwrap_or_default();
            let fire = alarm
                .as_ref()
                .map(|a| a.get_fire_time().map(|t| t.get_nanoseconds()).unwrap_or(0))
                .unwrap_or(0);
            (
                {
                    let mut out = String::from("{\"alarm\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("label").str(&label);
                    o.key("fireTime").int(fire);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "⏰"),
                    domain::bold(app, "Next alarm"),
                    domain::bold(app, &label),
                    domain::dim(app, &format!("at {fire}"))
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// One alarm, by its id.
fn alarm_get(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::alarm_service::get_alarm_params::Builder<'_>>();
        params.reborrow().init_id().set_id(id);
    });
    let bytes = match alarm_call(app, "getAlarm", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::alarm_service::get_alarm_results::Owned, _, _>(&bytes, |results| {
            let alarm = results.get_alarm().ok();
            let label = alarm.as_ref().map(|a| text(a.get_label())).unwrap_or_default();
            let fire = alarm
                .as_ref()
                .map(|a| a.get_fire_time().map(|t| t.get_nanoseconds()).unwrap_or(0))
                .unwrap_or(0);
            (
                {
                    let mut out = String::from("{\"alarm\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("id").int(id);
                    o.key("label").str(&label);
                    o.key("fireTime").int(fire);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} #{} {} {}",
                    domain::cyan(app, "⏰"),
                    domain::bold(app, "Alarm"),
                    domain::bold(app, &id.to_string()),
                    domain::bold(app, &label),
                    domain::dim(app, &format!("fires at {fire}"))
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The gated alarm writes: the CLI issues the call; the SERVICE decides.
fn alarm_create(app: &mut App, args: &[String], _path: &str) -> i32 {
    let label = domain::flag(args, "--label").unwrap_or_default();
    let fire = domain::number::<u64>(args, "--fire-in", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::alarm_service::create_alarm_params::Builder<'_>>();
        let mut alarm = params.reborrow().init_alarm();
        alarm.set_label(&label);
        alarm.reborrow().init_fire_time().set_nanoseconds(fire * 1_000_000);
        alarm.set_enabled(true);
    });
    alarm_write(app, "createAlarm", "alarm create", &label, &params)
}

fn alarm_update(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let label = domain::flag(args, "--label").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::alarm_service::update_alarm_params::Builder<'_>>();
        let mut alarm = params.reborrow().init_alarm();
        alarm.reborrow().init_id().set_id(id);
        alarm.set_label(&label);
    });
    alarm_write(app, "updateAlarm", "alarm update", &format!("{id} {label}"), &params)
}

fn alarm_snooze(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let millis = domain::number::<u64>(args, "--duration", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::alarm_service::snooze_alarm_params::Builder<'_>>();
        params.reborrow().init_id().set_id(id);
        params.reborrow().init_duration().set_nanoseconds(millis * 1_000_000);
    });
    alarm_write(app, "snoozeAlarm", "alarm snooze", &format!("{id}"), &params)
}

fn alarm_dismiss(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::alarm_service::dismiss_alarm_params::Builder<'_>>();
        params.reborrow().init_id().set_id(id);
    });
    alarm_write(app, "dismissAlarm", "alarm dismiss", &format!("{id}"), &params)
}

fn alarm_delete(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::alarm_service::delete_alarm_params::Builder<'_>>();
        params.reborrow().init_id().set_id(id);
    });
    alarm_write(app, "deleteAlarm", "alarm delete", &format!("{id}"), &params)
}

/// A gated alarm write, in the shell's own "✎ verb target" line.
fn alarm_write(app: &mut App, method: &str, verb: &str, target: &str, params: &[u8]) -> i32 {
    let bytes = match alarm_call(app, method, params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<crate::schema::types_capnp::void_result::Owned, _, _>(&bytes, |r| {
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
// The virtual clocks.
// ═════════════════════════════════════════════════════════════════════════

fn virtual_create(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain::flag(args, "--label").unwrap_or_else(|| "virtual".to_string());
    let rate = domain::number::<f32>(args, "--rate", 1.0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::create_virtual_clock_params::Builder<'_>>();
        let mut config = params.reborrow().init_config();
        config.set_name(&name);
        config.set_rate(rate);
        config.set_domain(ClockDomain::Realtime);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "createVirtualClock", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::create_virtual_clock_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let handle = results.get_clock().ok();
            let id = handle.as_ref().map(|h| h.get_id()).unwrap_or(0);
            let name = handle.as_ref().map(|h| text(h.get_name())).unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"clock\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("id").int(id);
                    o.key("name").str(&name);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} #{} {} rate {} — a virtual clock IS software, so this is REAL",
                    domain::cyan(app, "🖥"),
                    domain::bold(app, "Virtual clock"),
                    domain::bold(app, &id.to_string()),
                    domain::dim(app, &name),
                    rate
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn virtual_advance(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let millis = domain::number::<u64>(args, "--duration", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::clock_service::advance_virtual_clock_params::Builder<'_>>();
        params.reborrow().init_clock().set_id(id);
        params
            .reborrow()
            .init_duration()
            .set_nanoseconds(millis * 1_000_000);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "advanceVirtualClock", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::clock_service::advance_virtual_clock_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let stamp = results.get_time();
            let reading = nanos(&stamp);
            (
                {
                    let mut out = String::from("{\"time\":");
                    timestamp(&mut out, &stamp);
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} advanced {} ms → {}",
                    domain::cyan(app, "🖥"),
                    domain::bold(app, &format!("Virtual clock {id}")),
                    domain::green(app, "◉"),
                    millis,
                    domain::bold(app, &reading)
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// The shared helpers.
// ═════════════════════════════════════════════════════════════════════════

/// A `T.VoidResult` answer, in the shell's own "✎ verb target" line.
fn void_verb(app: &mut App, _method: &str, verb: &str, target: &str, bytes: &[u8]) -> i32 {
    let Some((json_text, human_text)) =
        tp::with_root::<crate::schema::types_capnp::void_result::Owned, _, _>(bytes, |r| {
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

/// A timestamp, as the contract-shaped JSON.
fn timestamp(out: &mut String, stamp: &capnp::Result<crate::schema::types_capnp::timestamp::Reader<'_>>) {
    let mut o = json::Obj::new(out);
    o.key("nanoseconds")
        .int(stamp.as_ref().map(|s| s.get_nanoseconds()).unwrap_or(0));
    o.end();
}

/// A duration, as the contract-shaped JSON.
fn duration(out: &mut String, value: &capnp::Result<crate::schema::types_capnp::duration::Reader<'_>>) {
    let mut o = json::Obj::new(out);
    o.key("nanoseconds")
        .int(value.as_ref().map(|d| d.get_nanoseconds()).unwrap_or(0));
    o.end();
}

/// A timestamp reader, as its nanoseconds.
fn nanos(stamp: &capnp::Result<crate::schema::types_capnp::timestamp::Reader<'_>>) -> String {
    stamp.as_ref().map(|s| s.get_nanoseconds()).unwrap_or(0).to_string()
}

/// A duration, as its nanoseconds.
fn duration_nanos(value: &capnp::Result<crate::schema::types_capnp::duration::Reader<'_>>) -> String {
    value.as_ref().map(|d| d.get_nanoseconds()).unwrap_or(0).to_string()
}

/// The yes/no cell, painted for a human line.
fn yes_no(app: &App, yes: bool) -> String {
    domain::glyph(app, &domain::yes_no(yes))
}

/// A duration in the shell's own words: days, hours, minutes, seconds.
pub fn duration_label(nanos: u64) -> String {
    let secs = nanos / 1_000_000_000;
    let millis = (nanos % 1_000_000_000) / 1_000_000;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else if secs > 0 {
        format!("{seconds}s")
    } else if millis > 0 {
        format!("{millis} ms")
    } else {
        format!("{nanos} ns")
    }
}

/// A clock domain, in the contract's own vocabulary.
fn clock_domain_of(token: &str) -> ClockDomain {
    match token {
        "monotonic" => ClockDomain::Monotonic,
        "custom" => ClockDomain::Custom,
        _ => ClockDomain::Realtime,
    }
}

/// A clock domain's own token.
fn domain_token(domain: ClockDomain) -> &'static str {
    match domain {
        ClockDomain::Realtime => "realtime",
        ClockDomain::Monotonic => "monotonic",
        ClockDomain::Custom => "custom",
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
