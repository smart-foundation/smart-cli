//! Render: dashboards, tables and bars over the SAME contract objects JSON
//! emits — decoration, never the source of truth (grammar law #11). The human
//! face borrows the contract readers it is given; JSON is produced separately
//! from those same readers in [`crate::json`]. `NO_COLOR`/non-TTY strip all
//! colour and progress decoration; a machine (or a pipe) sees plain text that
//! never diverges from the JSON.

use alloc::format;
use alloc::string::{String, ToString};

use crate::present;
use alloc::vec::Vec;

#[cfg(feature = "schema-network")]
use crate::schema::network_capnp as net;

/// The colour/decoration policy for one invocation.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// Whether colour is allowed (`fn color_from_env`).
    pub color: bool,
}

impl Style {
    /// Build a style from the environment (NO_COLOR) and TTY detection.
    #[cfg(feature = "std")]
    pub fn from_env() -> Self {
        Style {
            color: color_from_env(),
        }
    }
    /// A fixed style (tests, or an explicit --no-color).
    pub fn plain() -> Self {
        Style { color: false }
    }
    /// Paint `text` in `code` when colour is on (codes are ANSI SGR numbers).
    pub fn paint(&self, text: &str, code: u8) -> String {
        if self.color {
            format!("\u{1b}[{code}m{text}\u{1b}[0m")
        } else {
            text.to_string()
        }
    }
    /// The bright variant ("bold").
    pub fn bold(&self, text: &str) -> String {
        if self.color {
            format!("\u{1b}[1m{text}\u{1b}[0m")
        } else {
            text.to_string()
        }
    }
}

/// The colour law: colour is on only when stdout is a TTY AND `NO_COLOR` is
/// unset. A pipe or a CI log forces plain text.
#[cfg(feature = "std")]
pub fn color_from_env() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// A signal-strength bar over `width` cells, mapping dBm (-100..0 typical) to
/// 0..width. Borrows nothing; decoration only.
pub fn signal_bar(strength_dbm: i32, width: usize) -> String {
    // Clamp -100..0 to 0..1.
    let ratio = ((-strength_dbm as f32).clamp(0.0, 100.0)) / 100.0;
    let cells = (1.0 - ratio) * width as f32;
    // `f32::round` is std-only; the no_std core uses the equivalent shift-and-cast.
    let filled = (if cells < 0.0 { 0.0 } else { cells + 0.5 }).min(width as f32) as usize;
    let mut s = String::with_capacity(width);
    for i in 0..width {
        s.push(if i < filled { '#' } else { '.' });
    }
    s
}

/// A column-aligned table from owned rows (decoration; the rows are borrowed
/// into this text and never kept).
///
/// The widths are MEASURED, not counted: a width is the number of terminal
/// columns the text occupies (the presenter framework's measure), so a `◉` or
/// a CJK glyph lines up with the ASCII beside it instead of guessing.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| present::display_width(h)).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < cols {
                widths[i] = widths[i].max(present::display_width(cell));
            }
        }
    }
    let mut out = String::new();
    let header_line: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| present::pad_to(h, widths.get(i).copied().unwrap_or(0)))
        .collect();
    out.push_str(&header_line.join("  "));
    out.push('\n');
    for row in rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| present::pad_to(cell, widths.get(i).copied().unwrap_or(0)))
            .collect();
        out.push_str(&line.join("  "));
        out.push('\n');
    }
    out
}

/// The `net` dashboard: the composed summary (networking master switch, the
/// radios, the interfaces). It reads the very objects JSON emits.
#[cfg(feature = "schema-network")]
pub fn summary(
    style: &Style,
    networking_enabled: bool,
    radios: Option<net::network::list_radio_states_results::Reader<'_>>,
    interfaces: Option<net::network::get_interfaces_results::Reader<'_>>,
) -> String {
    let mut out = String::new();
    let master = if networking_enabled { "enabled" } else { "disabled" };
    out.push_str(&format!("networking: {}\n", style.bold(master)));
    if let Some(radios) = radios {
        out.push_str("\nradios:\n");
        let mut rows = Vec::new();
        for r in radios.get_radios().into_iter().flatten() {
            let state = match (r.get_enabled(), r.get_hard_blocked(), r.get_soft_blocked()) {
                (true, false, false) => "on",
                (_, true, _) => "hard-blocked",
                (_, _, true) => "soft-blocked",
                (false, _, _) => "off",
            };
            rows.push(vec![
                r.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default(),
                state.to_string(),
            ]);
        }
        out.push_str(&table(&["radio", "state"], &rows));
    }
    if let Some(interfaces) = interfaces {
        out.push_str("\ninterfaces:\n");
        out.push_str(&interface_rows(interfaces));
    }
    out
}

/// Build the interface rows table from the `getInterfaces` result.
#[cfg(feature = "schema-network")]
fn interface_rows(interfaces: net::network::get_interfaces_results::Reader<'_>) -> String {
    let mut rows = Vec::new();
    for it in interfaces.get_interfaces().into_iter().flatten() {
        let state = match (it.get_up(), it.get_connected()) {
            (true, true) => "up",
            (true, false) => "up/disconnected",
            (false, _) => "down",
        };
        rows.push(vec![
            it.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default(),
            it.get_type().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default(),
            state.to_string(),
            format!("{}", it.get_mt_u()),
        ]);
    }
    table(&["interface", "type", "state", "mtu"], &rows)
}

/// The `net interface <name>` detail view (the `getStatus` result + a strength
/// bar for wifi).
#[cfg(feature = "schema-network")]
pub fn status(status: net::interface_connection_status::Reader<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "interface: {}\n",
        status.get_interface().map(|i| i.get_id()).unwrap_or(0)
    ));
    out.push_str(&format!("type:      {}\n", status.get_type().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default()));
    // The state glyph: one glyph per state, never a colour alone.
    let connected = status.get_connected();
    out.push_str(&format!(
        "connected: {connected} {}\n",
        if connected {
            present::glyph::ACTIVE
        } else {
            present::glyph::UNKNOWN
        }
    ));
    if let Ok(host) = status.get_host_state() {
        out.push_str(&format!("hostState: {}\n", host.to_str().unwrap_or("")));
    }
    if let Ok(wifi) = status.get_wifi() {
        if let Ok(ssid) = wifi.get_ssid() {
            let ssid = ssid.to_str().unwrap_or("");
            out.push_str(&format!("ssid:      {ssid}\n"));
        }
    }
    out.push_str(&format!(
        "signal:    {} dBm {}\n",
        status.get_strength(),
        signal_bar(status.get_strength(), 10)
    ));
    out
}

/// The `net wifi list` table (the `listKnownWifiNetworks` result).
#[cfg(feature = "schema-network")]
pub fn wifi_list(results: net::network::list_known_wifi_networks_results::Reader<'_>) -> String {
    let mut rows = Vec::new();
    for n in results.get_networks().into_iter().flatten() {
        rows.push(vec![
            n.get_ssid().map(|s| s.to_str().unwrap_or("").to_string()).unwrap_or_default(),
            n.get_security().map(|s| s.to_str().unwrap_or("").to_string()).unwrap_or_default(),
            if n.get_auto_connect() { "auto" } else { "manual" }.to_string(),
        ]);
    }
    table(&["ssid", "security", "connect"], &rows)
}

/// The `net routes` table.
#[cfg(feature = "schema-network")]
pub fn routes(results: net::network::get_routes_results::Reader<'_>) -> String {
    let mut rows = Vec::new();
    for r in results.get_routes().into_iter().flatten() {
        rows.push(vec![
            r.get_destination_cidr().map(|s| s.to_str().unwrap_or("").to_string()).unwrap_or_default(),
            r.get_gateway().map(|s| s.to_str().unwrap_or("").to_string()).unwrap_or_default(),
            format!("{}", r.get_metric()),
        ]);
    }
    table(&["destination", "gateway", "metric"], &rows)
}

/// The counters view (bytes in/out with a bar).
#[cfg(feature = "schema-network")]
pub fn counters(counters: net::interface_counters::Reader<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("bytes in:  {}\n", counters.get_bytes_in()));
    out.push_str(&format!("bytes out: {}\n", counters.get_bytes_out()));
    out.push_str(&format!("packets in:  {}\n", counters.get_packets_in()));
    out.push_str(&format!("packets out: {}\n", counters.get_packets_out()));
    out
}

/// The radios table (from `listRadioStates`).
#[cfg(feature = "schema-network")]
pub fn radios(results: net::network::list_radio_states_results::Reader<'_>) -> String {
    let mut rows = Vec::new();
    for r in results.get_radios().into_iter().flatten() {
        let state = match (r.get_enabled(), r.get_hard_blocked(), r.get_soft_blocked()) {
            (true, false, false) => "on",
            (_, true, _) => "hard-blocked",
            (_, _, true) => "soft-blocked",
            (false, _, _) => "off",
        };
        rows.push(vec![
            r.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default(),
            state.to_string(),
        ]);
    }
    table(&["radio", "state"], &rows)
}

/// A one-line void-result acknowledgement (success is only ever printed after
/// an ok result — the caller has already checked).
pub fn ok_line(what: &str) -> String {
    format!("{what}\n")
}
