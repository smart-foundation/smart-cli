//! `time` — the wall clock, the arithmetic calendars, the HSM attestation:
//! the coupled shell's presentation, re-keyed from the engine's mirror types
//! (`DateTime`, `HistoricalDate`, `CalendarDiff`) to the CONTRACT'S RESULT
//! READERS (`time.capnp`'s `getDateTime` / `convertCalendar` /
//! `addCalendarUnits` / `subtractCalendarUnits` / `diffCalendarDates`).
//!
//! ## What the re-keying changed
//!
//! * the engine's `DateTime` mirror became the contract's `dateTime` reader —
//!   the same fields, read from the wire;
//! * the shell's `Value` records became the contract's own JSON (the field
//!   names are the schema's, verbatim);
//! * the shell's `capabilities` view has NO contract method (the capability
//!   tokens live in discovery's descriptor metadata, not in a method), and
//!   the shell's `tai` view has none either (the TAI converter moved to
//!   timezone.capnp). Both are therefore not on this surface: the service
//!   publishes no `time tai` and no `time caps` command, and this CLI
//!   presents only what the contract declares.
//!
//! The registry publishes six paths: `time now`, `time cal`, `time add`,
//! `time sub`, `time diff`, `time verify-hsm`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json;
use crate::present;
use crate::schema::time_capnp as wire;
use crate::schema::time_capnp::{CalendarSystem, CalendarUnitKind};
use crate::transport as tp;

use super::domain::{self, Presenter};
use super::App;

/// The schema stem the family presents.
const STEM: &str = "time";
/// The contract interface the family presents.
const INTERFACE: &str = "Time";

/// The family's presenter lookup: keyed by **(schema id, method name)** — the
/// contract's own identity for the call, which is what the service's registry
/// publishes.
pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != crate::contract::schema_id(STEM)? {
        return None;
    }
    Some(match method {
        "getDateTime" => now,
        "convertCalendar" => cal,
        "addCalendarUnits" => add,
        "subtractCalendarUnits" => sub,
        "diffCalendarDates" => diff,
        "verifyHsmTimestamp" => verify_hsm,
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
// `time now` — the wall-clock reading in the host's zone.
// ═════════════════════════════════════════════════════════════════════════

fn now(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::time::get_date_time_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getDateTime", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::time::get_date_time_results::Owned, _, _>(
        &bytes,
        |results| {
            let Ok(dt) = results.get_date_time() else {
                return (
                    String::from("{\"error\":{\"kind\":\"failed\",\"message\":\"the service answered no date-time\"}}"),
                    format!("  {} the service answered no date-time\n", domain::red(app, "✗")),
                );
            };
            (date_time_json(&dt), date_time_human(app, &dt))
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    emit(app, json_text, human_text)
}

/// The contract's `dateTime`, as the contract-shaped JSON.
fn date_time_json(dt: &wire::date_time::Reader<'_>) -> String {
    let mut out = String::from("{\"dateTime\":");
    let mut o = json::Obj::new(&mut out);
    o.key("year").int(dt.get_year());
    o.key("month").int(dt.get_month());
    o.key("day").int(dt.get_day());
    o.key("hour").int(dt.get_hour());
    o.key("minute").int(dt.get_minute());
    o.key("second").int(dt.get_second());
    o.key("millisecond").int(dt.get_millisecond());
    o.key("weekday").int(dt.get_weekday());
    o.key("yearday").int(dt.get_yearday());
    o.key("timezone").str(&text(dt.get_timezone()));
    o.key("utcOffsetSeconds").int(dt.get_utc_offset_seconds());
    o.end();
    out
}

/// The shell's `Now` view: the wall reading, the weekday, the zone, the
/// source — the same tree, the same words.
fn date_time_human(app: &App, dt: &wire::date_time::Reader<'_>) -> String {
    let wall = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        dt.get_year(),
        dt.get_month(),
        dt.get_day(),
        dt.get_hour(),
        dt.get_minute(),
        dt.get_second(),
        dt.get_millisecond()
    );
    let zone = text(dt.get_timezone());
    let zone_display = if zone.is_empty() {
        "— the host publishes no zone".to_string()
    } else {
        zone
    };
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {}\n",
        domain::cyan(app, "🕐"),
        domain::bold(app, "Now")
    ));
    out.push_str(&format!("  ├─ Wall       {}\n", domain::bold(app, &wall)));
    out.push_str(&format!(
        "  ├─ Weekday    {} (day {} of the year)\n",
        weekday_name(dt.get_weekday()),
        dt.get_yearday()
    ));
    out.push_str(&format!(
        "  ├─ Zone       {} {}\n",
        zone_display,
        domain::dim(app, &format!("(UTC{})", format_offset(dt.get_utc_offset_seconds())))
    ));
    out.push_str(&format!(
        "  └─ Source     {}",
        domain::dim(
            app,
            "CLOCK_REALTIME, zone from the host's own TZif file — no `date` fork"
        )
    ));
    out
}

/// The weekday's name, from the contract's ISO numbering.
fn weekday_name(iso: u8) -> &'static str {
    match iso {
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        7 => "Sunday",
        _ => "—",
    }
}

/// The UTC offset, in the shell's own `+HH:MM` spelling.
fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { "-" } else { "+" };
    let abs = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3_600, (abs % 3_600) / 60)
}

// ═════════════════════════════════════════════════════════════════════════
// `time cal` — the arithmetic calendars, one row each, in the contract's own
// order. A calendar the host refuses is named with the host's own reason,
// never approximated.
// ═════════════════════════════════════════════════════════════════════════

/// The calendars the contract names, in the contract's own order.
const CALENDARS: [CalendarSystem; 6] = [
    CalendarSystem::Gregorian,
    CalendarSystem::Julian,
    CalendarSystem::Islamic,
    CalendarSystem::Hebrew,
    CalendarSystem::Chinese,
    CalendarSystem::Ethiopian,
];

fn cal(app: &mut App, args: &[String], _path: &str) -> i32 {
    let Some((year, month, day)) = source_date(app, args) else {
        return crate::registry::EXIT_USAGE;
    };
    let wanted: Vec<CalendarSystem> = match domain::flag(args, "--to") {
        Some(token) => match calendar_of(&token) {
            Some(system) => vec![system],
            None => {
                super::usage_error_app(
                    app,
                    &format!(
                        "a calendar is gregorian/julian/islamic/ethiopian (the calendars this \
                         host's own arithmetic serves), got {token:?}"
                    ),
                );
                return crate::registry::EXIT_USAGE;
            }
        },
        None => CALENDARS.to_vec(),
    };
    // The source row, then one conversion per target calendar. The refusal of
    // a calendar is a ROW with the host's reason, never an approximation.
    let mut rows: Vec<(CalendarSystem, Result<(i64, u8, u8), String>)> = Vec::new();
    rows.push((CalendarSystem::Gregorian, Ok((year, month, day))));
    for target in wanted {
        if target == CalendarSystem::Gregorian {
            continue;
        }
        let params = msg(|m| {
            let mut params = m.init_root::<wire::time::convert_calendar_params::Builder<'_>>();
            {
                let mut date = params.reborrow().init_date();
                date.set_year(year);
                date.set_month(month);
                date.set_day(day);
                date.set_calendar(CalendarSystem::Gregorian);
            }
            params.set_from_system(CalendarSystem::Gregorian);
            params.set_to_system(target);
        });
        match super::call_in(app, STEM, INTERFACE, "convertCalendar", &params) {
            Ok(bytes) => {
                let converted = tp::with_root::<wire::time::convert_calendar_results::Owned, _, _>(
                    &bytes,
                    |results| {
                        results
                            .get_converted()
                            .ok()
                            .map(|d| (d.get_year(), d.get_month(), d.get_day()))
                    },
                )
                .flatten();
                match converted {
                    Some(converted) => rows.push((target, Ok(converted))),
                    None => rows.push((target, Err("the service answered no date".to_string()))),
                }
            }
            Err(error) => rows.push((target, Err(error.to_string()))),
        }
    }
    let scope = match domain::flag(args, "--to") {
        Some(token) => format!("→ {token}"),
        None => "every calendar in the contract's own order".to_string(),
    };
    let human = cal_human(app, &rows, &scope);
    let json_text = cal_json(&rows);
    emit(app, json_text, human)
}

/// The source date: the `--date` the caller named, or the host's own reading
/// (`getDateTime` — the composition the registry publishes).
fn source_date(app: &mut App, args: &[String]) -> Option<(i64, u8, u8)> {
    if let Some(text) = domain::flag(args, "--date") {
        return parse_date(app, &text);
    }
    let params = msg(|m| {
        let _ = m.init_root::<wire::time::get_date_time_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getDateTime", &params) {
        Ok(bytes) => bytes,
        Err(error) => {
            super::emit_error(app, &error);
            return None;
        }
    };
    tp::with_root::<wire::time::get_date_time_results::Owned, _, _>(&bytes, |results| {
        results
            .get_date_time()
            .ok()
            .map(|dt| (i64::from(dt.get_year()), dt.get_month(), dt.get_day()))
    })
    .flatten()
}

/// A `YYYY-M-D` Gregorian date, parsed through the contract's own calendar.
fn parse_date(app: &App, text: &str) -> Option<(i64, u8, u8)> {
    let parts: Vec<&str> = text.split('-').collect();
    let bad = || {
        super::usage_error_app(
            app,
            &format!("a date is YYYY-M-D, got {text:?}"),
        );
    };
    if parts.len() != 3 {
        bad();
        return None;
    }
    let Some(year) = parts[0].parse::<i64>().ok() else {
        bad();
        return None;
    };
    let Some(month) = parts[1].parse::<u8>().ok() else {
        bad();
        return None;
    };
    let Some(day) = parts[2].parse::<u8>().ok() else {
        bad();
        return None;
    };
    Some((year, month, day))
}

/// A calendar token, in the contract's own vocabulary.
fn calendar_of(token: &str) -> Option<CalendarSystem> {
    match token {
        "gregorian" => Some(CalendarSystem::Gregorian),
        "julian" => Some(CalendarSystem::Julian),
        "islamic" => Some(CalendarSystem::Islamic),
        "hebrew" => Some(CalendarSystem::Hebrew),
        "chinese" => Some(CalendarSystem::Chinese),
        "ethiopian" => Some(CalendarSystem::Ethiopian),
        _ => None,
    }
}

/// The calendar's own token, as the shell printed it.
fn calendar_token(system: CalendarSystem) -> &'static str {
    match system {
        CalendarSystem::Gregorian => "gregorian",
        CalendarSystem::Julian => "julian",
        CalendarSystem::Islamic => "islamic",
        CalendarSystem::Hebrew => "hebrew",
        CalendarSystem::Chinese => "chinese",
        CalendarSystem::Ethiopian => "ethiopian",
        _ => "unknown",
    }
}

/// The shell's calendar table: the glyph per calendar, the calendar's name,
/// the date (or the host's refusal).
fn cal_human(
    app: &App,
    rows: &[(CalendarSystem, Result<(i64, u8, u8), String>)],
    scope: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} — exact arithmetic, never an approximation\n",
        domain::cyan(app, "📅"),
        domain::bold(app, "Calendars"),
        domain::dim(app, scope)
    ));
    let mut table = present::Table::new(vec![
        String::new(),
        String::from("CALENDAR"),
        String::from("DATE"),
    ]);
    for (system, row) in rows {
        let token = calendar_token(*system);
        let date = match row {
            Ok((year, month, day)) => format!("{year:04}-{month:02}-{day:02}"),
            Err(reason) => format!(
                "{} {}",
                domain::red(app, "✗"),
                present::truncate_to(reason, 64)
            ),
        };
        let glyph = match system {
            CalendarSystem::Gregorian => present::Cell::styled("▣", present::Sgr::Cyan),
            CalendarSystem::Julian => present::Cell::styled("▤", present::Sgr::Blue),
            CalendarSystem::Islamic => present::Cell::styled("◍", present::Sgr::Yellow),
            CalendarSystem::Ethiopian => present::Cell::styled("◈", present::Sgr::Green),
            _ => present::Cell::plain("▢"),
        };
        table = table.row(vec![glyph, present::Cell::plain(token), present::Cell::plain(date)]);
    }
    out.push_str(&table.render(app.style()));
    out
}

/// The contract-shaped JSON of the calendar rows.
fn cal_json(rows: &[(CalendarSystem, Result<(i64, u8, u8), String>)]) -> String {
    let mut out = String::from("{\"conversions\":[");
    for (index, (system, row)) in rows.iter().enumerate() {
        json::sep(&mut out, index == 0);
        let mut o = json::Obj::new(&mut out);
        o.key("calendar").str(calendar_token(*system));
        match row {
            Ok((year, month, day)) => {
                o.key("date").sub(|out| {
                    let mut d = json::Obj::new(out);
                    d.key("year").int(*year);
                    d.key("month").int(*month);
                    d.key("day").int(*day);
                    d.key("calendar").str(calendar_token(*system));
                    d.end();
                });
            }
            Err(reason) => {
                o.key("error").str(reason);
            }
        }
        o.end();
    }
    out.push_str("]}");
    out
}

// ═════════════════════════════════════════════════════════════════════════
// `time add` / `time sub` — the same arithmetic kernel, the two directions.
// ═════════════════════════════════════════════════════════════════════════

fn add(app: &mut App, args: &[String], _path: &str) -> i32 {
    units_verb(app, args, "addCalendarUnits", "+")
}

fn sub(app: &mut App, args: &[String], _path: &str) -> i32 {
    units_verb(app, args, "subtractCalendarUnits", "−")
}

/// The unit arithmetic, in the shell's own words.
fn units_verb(app: &mut App, args: &[String], method: &str, op: &str) -> i32 {
    let positionals = domain::positionals(args);
    let Some(date_text) = positionals.first() else {
        super::usage_error_app(app, &format!("usage: time {method} <YYYY-M-D> <2weeks>"));
        return crate::registry::EXIT_USAGE;
    };
    let Some(unit_text) = positionals.get(1) else {
        super::usage_error_app(app, &format!("usage: time {method} <YYYY-M-D> <2weeks>"));
        return crate::registry::EXIT_USAGE;
    };
    let Some((year, month, day)) = parse_date(app, date_text) else {
        return crate::registry::EXIT_USAGE;
    };
    let Some((kind, amount)) = parse_unit(app, unit_text) else {
        return crate::registry::EXIT_USAGE;
    };
    let params = msg(|m| {
        let mut params = m.init_root::<wire::time::add_calendar_units_params::Builder<'_>>();
        {
            let mut date = params.reborrow().init_date();
            date.set_year(year);
            date.set_month(month);
            date.set_day(day);
            date.set_calendar(CalendarSystem::Gregorian);
        }
        let mut units = params.reborrow().init_units(1);
        let mut unit = units.reborrow().get(0);
        unit.set_kind(kind);
        unit.set_amount(amount);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, method, &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::time::add_calendar_units_results::Owned, _, _>(
        &bytes,
        |results| {
            let Ok(result) = results.get_result() else {
                return (
                    String::from("{\"error\":{\"kind\":\"failed\",\"message\":\"the service answered no date\"}}"),
                    format!("  {} the service answered no date\n", domain::red(app, "✗")),
                );
            };
            let to = format!(
                "{:04}-{:02}-{:02}",
                result.get_year(),
                result.get_month(),
                result.get_day()
            );
            let from = format!("{year:04}-{month:02}-{day:02}");
            (
                historical_date_json("result", &result),
                format!(
                    "  {} {} {} → {} {}",
                    domain::cyan(app, "📅"),
                    from,
                    domain::dim(app, &format!("{op}{unit_text}")),
                    domain::bold(app, &to),
                    domain::dim(app, "(month-end clamped)")
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    emit(app, json_text, human_text)
}

/// A `2weeks`/`1month`/`3days` unit token, in the contract's own vocabulary.
fn parse_unit(app: &App, text: &str) -> Option<(CalendarUnitKind, i32)> {
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = &text[digits.len()..];
    let amount: i32 = if digits.is_empty() {
        1
    } else {
        match digits.parse::<i32>() {
            Ok(amount) => amount,
            Err(_) => {
                super::usage_error_app(
                    app,
                    &format!("cannot read an amount from {text:?}"),
                );
                return None;
            }
        }
    };
    let kind = match rest.trim() {
        "d" | "day" | "days" => CalendarUnitKind::Days,
        "w" | "week" | "weeks" => CalendarUnitKind::Weeks,
        "m" | "month" | "months" => CalendarUnitKind::Months,
        "y" | "year" | "years" => CalendarUnitKind::Years,
        other => {
            super::usage_error_app(
                app,
                &format!("a unit is days/weeks/months/years, got {other:?}"),
            );
            return None;
        }
    };
    Some((kind, amount))
}

// ═════════════════════════════════════════════════════════════════════════
// `time diff` — the difference between two Gregorian dates.
// ═════════════════════════════════════════════════════════════════════════

fn diff(app: &mut App, args: &[String], _path: &str) -> i32 {
    let positionals = domain::positionals(args);
    let (Some(left), Some(right)) = (positionals.first(), positionals.get(1)) else {
        super::usage_error_app(app, "usage: time diff <YYYY-M-D> <YYYY-M-D>");
        return crate::registry::EXIT_USAGE;
    };
    let Some((year1, month1, day1)) = parse_date(app, left) else {
        return crate::registry::EXIT_USAGE;
    };
    let Some((year2, month2, day2)) = parse_date(app, right) else {
        return crate::registry::EXIT_USAGE;
    };
    let params = msg(|m| {
        let mut params = m.init_root::<wire::time::diff_calendar_dates_params::Builder<'_>>();
        {
            let mut date = params.reborrow().init_date1();
            date.set_year(year1);
            date.set_month(month1);
            date.set_day(day1);
            date.set_calendar(CalendarSystem::Gregorian);
        }
        {
            let mut date = params.reborrow().init_date2();
            date.set_year(year2);
            date.set_month(month2);
            date.set_day(day2);
            date.set_calendar(CalendarSystem::Gregorian);
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "diffCalendarDates", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::time::diff_calendar_dates_results::Owned, _, _>(&bytes, |results| {
            let Ok(diff) = results.get_diff() else {
                return (
                    String::from("{\"error\":{\"kind\":\"failed\",\"message\":\"the service answered no difference\"}}"),
                    format!("  {} the service answered no difference\n", domain::red(app, "✗")),
                );
            };
            let arrow = if diff.get_negative() { "before" } else { "after" };
            (
                {
                    let mut out = String::from("{\"diff\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("years").int(diff.get_years());
                    o.key("months").int(diff.get_months());
                    o.key("days").int(diff.get_days());
                    o.key("totalDays").int(diff.get_total_days());
                    o.key("negative").boolean(diff.get_negative());
                    o.end();
                    out
                },
                format!(
                    "  {} {} years, {} months, {} days — {} day(s) {}",
                    domain::cyan(app, "📏"),
                    diff.get_years(),
                    diff.get_months(),
                    diff.get_days(),
                    diff.get_total_days(),
                    domain::dim(app, arrow)
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    emit(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `time verify-hsm` — the contract's own attestation verdict.
// ═════════════════════════════════════════════════════════════════════════

fn verify_hsm(app: &mut App, args: &[String], _path: &str) -> i32 {
    let timestamp = domain::number::<u64>(args, "--timestamp", 0);
    let hardware = domain::flag(args, "--hardware-id").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::time::verify_hsm_timestamp_params::Builder<'_>>();
        let mut hsm = params.reborrow().init_hsm();
        hsm.reborrow().init_timestamp().set_nanoseconds(timestamp);
        hsm.set_hardware_id(&hardware);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "verifyHsmTimestamp", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::time::verify_hsm_timestamp_results::Owned, _, _>(&bytes, |results| {
            let Ok(result) = results.get_result() else {
                return (
                    String::from("{\"error\":{\"kind\":\"failed\",\"message\":\"the service answered no verdict\"}}"),
                    format!("  {} the service answered no verdict\n", domain::red(app, "✗")),
                );
            };
            match result.which() {
                Ok(crate::schema::types_capnp::bool_result::Which::Ok(ok)) => {
                    (
                        format!("{{\"result\":{{\"ok\":{ok}}}}}"),
                        format!(
                            "  {} {} — the contract's own attestation verdict",
                            domain::cyan(app, "🔏"),
                            if ok {
                                domain::green(app, "◉ attested")
                            } else {
                                domain::dim(app, "○ not attested")
                            }
                        ),
                    )
                }
                Ok(crate::schema::types_capnp::bool_result::Which::Error(error)) => {
                    let error = error.map(|e| e.get_message().map(|m| m.to_str().unwrap_or("")).unwrap_or_default().to_string()).unwrap_or_default();
                    (
                        format!("{{\"result\":{{\"error\":{}}}}}", {
                            let mut out = String::new();
                            json::write_str(&mut out, &error);
                            out
                        }),
                        format!(
                            "  {} {} — the contract's own attestation verdict",
                            domain::cyan(app, "🔏"),
                            domain::red(app, &format!("✗ {error}"))
                        ),
                    )
                }
                _ => (
                    String::from("{\"result\":null}"),
                    format!("  {} the service answered no verdict\n", domain::cyan(app, "🔏")),
                ),
            }
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    emit(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// The shared helpers.
// ═════════════════════════════════════════════════════════════════════════

/// A text reader, as a string (never a guess).
fn text(reader: capnp::Result<capnp::text::Reader<'_>>) -> String {
    reader
        .map(|t| t.to_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

/// A historical date, as the contract-shaped JSON.
fn historical_date_json(key: &str, date: &wire::historical_date::Reader<'_>) -> String {
    let mut out = String::new();
    let mut o = json::Obj::new(&mut out);
    o.key(key).sub(|out| {
        let mut d = json::Obj::new(out);
        d.key("year").int(date.get_year());
        d.key("month").int(date.get_month());
        d.key("day").int(date.get_day());
        d.key("calendar").str(calendar_token(date.get_calendar().unwrap_or(CalendarSystem::Unknown)));
        d.end();
    });
    o.end();
    out
}

/// Print the answer: the JSON object, or the human view.
fn emit(app: &App, json_text: String, human_text: String) -> i32 {
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
