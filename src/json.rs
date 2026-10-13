//! Contract-shaped JSON: keys are the contract field names VERBATIM; values
//! come from the contract readers; no added fields, no decoration (grammar law
//! #6, #11). This is the machine-mode surface — the same objects the tables
//! render, never the source of truth for them.
//!
//! Zero-cost law (Part 6): every emitter writes into a caller-owned `&mut
//! String` by borrowing the reader's values (text via `push`, integers via
//! `write!` into the `fmt::Write`), so emitting an object performs NO
//! per-field heap allocation — only the output buffer grows once. The counting
//! allocator test in `tests/` asserts this.
//!
//! The two places a contract struct is rendered to a scalar (documented, honest
//! — never a fabricated value): `T.Duration`/`T.Timestamp` render as their
//! `nanoseconds` integer, and `T.IpAddress` renders as its address spelling.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use crate::schema::types_capnp;
#[cfg(feature = "schema-network")]
use crate::schema::network_capnp as net;

// ── Primitives that borrow ────────────────────────────────────────────────

/// Write a bare key (suited for `key:`).
pub fn write_key(out: &mut String, key: &str) {
    write_str(out, key);
    out.push(':');
}

/// Comma separating object members (the caller tracks "first" itself, or uses
/// [`Obj`]).
pub fn sep(out: &mut String, first: bool) {
    if !first {
        out.push(',');
    }
}

/// The JSON `error` object the failure laws print (the contract `T.Error`
/// shape: `{"kind","code","message"}`). The kind is the contract's error-code
/// token; the message is the service's own vocabulary.
pub fn write_error(out: &mut String, code: types_capnp::ErrorCode, message: &str) {
    out.push('{');
    write_key(out, "kind");
    write_str(out, error_code(code));
    out.push(',');
    write_key(out, "code");
    write_str(out, error_code(code));
    out.push(',');
    write_key(out, "message");
    write_str(out, message);
    out.push('}');
}

/// A JSON string literal with only quotes/backslash/control escaping.
pub fn write_str(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}


/// A tiny object builder so callers never hand-track commas: `out.obj(|o| { o.key(...) })`.
pub struct Obj<'a> {
    out: &'a mut String,
    first: bool,
}

impl<'a> Obj<'a> {
    /// Open an object on `out`.
    pub fn new(out: &'a mut String) -> Self {
        out.push('{');
        Obj { out, first: true }
    }
    /// Bind a `"key":` and return a sub-builder positioned to write the value.
    pub fn key(&mut self, key: &str) -> Value<'_> {
        sep(self.out, self.first);
        self.first = false;
        write_key(self.out, key);
        Value {
            out: &mut *self.out,
        }
    }
    /// Finish the object (write the closing brace — an object left open is
    /// malformed JSON, so this is not decoration).
    pub fn end(self) {
        self.out.push('}');
    }
}

/// The value position returned by [`Obj::key`].
pub struct Value<'a> {
    out: &'a mut String,
}

impl Value<'_> {
    /// A borrowed string value.
    pub fn str(self, value: &str) {
        write_str(self.out, value);
    }
    /// A bool value.
    pub fn boolean(self, value: bool) {
        self.out.push_str(if value { "true" } else { "false" });
    }
    /// An unsigned / signed integer value.
    pub fn int<T: core::fmt::Display>(self, value: T) {
        let _ = write!(self.out, "{value}");
    }
    /// A float value (NaN/inf render as JSON null — never a fabricated number).
    pub fn float(self, value: f64) {
        if value.is_finite() {
            let _ = write!(self.out, "{value}");
        } else {
            self.out.push_str("null");
        }
    }
    /// A `T.Duration` / `T.Timestamp` as its nanoseconds (documented rendering).
    pub fn nanos(self, nanos: u64) {
        let _ = write!(self.out, "{nanos}");
    }
    /// An owned but still allocation-light string value.
    pub fn string(self, value: &String) {
        write_str(self.out, value);
    }
    /// Take the value position to hand to a sub-emitter (e.g. a nested object).
    pub fn sub<F: FnOnce(&mut String)>(self, f: F) {
        f(self.out);
    }
}

/// Open an array on `out`, run `f`, close it.
pub fn array<F: FnOnce(&mut String)>(out: &mut String, f: F) {
    out.push('[');
    f(out);
    out.push(']');
}

/// Push a list separator inside an array (caller tracks "first").
pub fn arr_sep(out: &mut String, first: bool) {
    if !first {
        out.push(',');
    }
}

// ── T.IpAddress ───────────────────────────────────────────────────────────

/// Render a `T.IpAddress` reader as its address spelling (dotted quad / hex
/// groups / text arm). The reader's own bytes, never fabricated.
pub fn write_ip_address(out: &mut String, ip: types_capnp::ip_address::Reader<'_>) {
    match ip.which() {
        Ok(types_capnp::ip_address::Which::V4(v4)) => {
            if let Ok(reader) = v4 {
                if let Ok(octets) = reader.get_octets() {
                    let bytes: &[u8] = &octets;
                    out.push_str(&render_v4(bytes));
                }
            }
        }
        Ok(types_capnp::ip_address::Which::V6(v6)) => {
            if let Ok(reader) = v6 {
                if let Ok(octets) = reader.get_octets() {
                    let bytes: &[u8] = &octets;
                    out.push_str(&render_v6(bytes));
                }
            }
        }
        Ok(types_capnp::ip_address::Which::Text(text)) => {
            write_str(out, text.map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
        }
        _ => out.push_str("\"\""),
    }
}

/// The dotted-quad spelling of a 4-octet address (empty bytes → empty string).
pub fn render_v4(octets: &[u8]) -> String {
    if octets.len() != 4 {
        return String::new();
    }
    format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3])
}

/// The colon-hex spelling of a 16-octet IPv6 address (uncompressed groups; a
/// valid textual form). Empty/short bytes → empty string.
pub fn render_v6(octets: &[u8]) -> String {
    if octets.len() != 16 {
        return String::new();
    }
    let mut groups = [0u16; 8];
    for i in 0..8 {
        groups[i] = u16::from_be_bytes([octets[2 * i], octets[2 * i + 1]]);
    }
    let mut s = String::new();
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            s.push(':');
        }
        let _ = write!(s, "{g:x}");
    }
    s
}

// ── Enums rendered as their contract token ────────────────────────────────

/// The token for a `FirewallDirection`.
#[cfg(feature = "schema-network")]
pub fn firewall_direction(d: net::FirewallDirection) -> &'static str {
    match d {
        net::FirewallDirection::Inbound => "inbound",
        net::FirewallDirection::Outbound => "outbound",
        net::FirewallDirection::Both => "both",
        net::FirewallDirection::Custom => "custom",
    }
}

/// The token for a `FirewallAction`.
#[cfg(feature = "schema-network")]
pub fn firewall_action(a: net::FirewallAction) -> &'static str {
    match a {
        net::FirewallAction::Accept => "accept",
        net::FirewallAction::Drop => "drop",
        net::FirewallAction::Reject => "reject",
        net::FirewallAction::Log => "log",
        net::FirewallAction::Custom => "custom",
    }
}

/// The token for a `T.ErrorCode`.
pub fn error_code(c: types_capnp::ErrorCode) -> &'static str {
    use types_capnp::ErrorCode as E;
    match c {
        E::NotFound => "notFound",
        E::PermissionDenied => "permissionDenied",
        E::Failed => "failed",
        E::Temporary => "temporary",
        E::Unavailable => "unavailable",
        E::LimitExceeded => "limitExceeded",
        E::OutOfMemory => "outOfMemory",
        E::InvalidArgument => "invalidArgument",
        E::Timeout => "timeout",
        E::Cancelled => "cancelled",
        E::AlreadyExists => "alreadyExists",
        E::NotSupported => "notSupported",
        E::Conflict => "conflict",
        E::Unknown => "unknown",
        E::HardwareFault => "hardwareFault",
        E::DriverError => "driverError",
        E::ProtocolViolation => "protocolViolation",
        E::BusError => "busError",
        E::ThermalThrottling => "thermalThrottling",
        E::PowerStateFailure => "powerStateFailure",
    }
}

/// The JSON `error` result the failure laws print: the whole answer is one
/// object carrying the contract `T.Error` shape under `error`.
pub fn error_object(out: &mut String, code: types_capnp::ErrorCode, message: &str) {
    out.push('{');
    write_key(out, "error");
    write_error(out, code, message);
    out.push('}');
}

// ═════════════════════════════════════════════════════════════════════════
// Network-family struct emitters (each borrows its reader; no per-field alloc)
// ═════════════════════════════════════════════════════════════════════════

/// A `NetworkInterface`.
#[cfg(feature = "schema-network")]
pub fn write_network_interface(out: &mut String, it: net::network_interface::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("id").int(it.get_id().map(|i| i.get_id()).unwrap_or(0));
    o.key("name").str(it.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or(""));
    o.key("type").str(it.get_type().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    o.key("up").boolean(it.get_up());
    o.key("connected").boolean(it.get_connected());
    o.key("mtU").int(it.get_mt_u());
    o.key("speedUp").int(it.get_speed_up());
    o.key("speedDown").int(it.get_speed_down());
    o.key("macRandom").boolean(it.get_mac_random());
    o.key("powerProfile")
        .str(it.get_power_profile().map(|p| p.to_str().unwrap_or("")).unwrap_or(""));
    // The address list, each address rendered to its spelling.
    {
        let v = o.key("ipAddresses");
        v.sub(|out| {
            let mut first = true;
            array(out, |out| {
                if let Ok(addrs) = it.get_ip_addresses() {
                    for ip in addrs.into_iter() {
                        arr_sep(out, first);
                        first = false;
                        write_ip_address(out, ip);
                    }
                }
            });
        });
    }
    if let Ok(hw) = it.get_hw_address() {
        o.key("hwAddress").str(hw.get_address().map(|a| a.to_str().unwrap_or("")).unwrap_or(""));
    }
    o.end();
}

/// A `RadioState`.
#[cfg(feature = "schema-network")]
pub fn write_radio_state(out: &mut String, r: net::radio_state::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("name").str(r.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or(""));
    o.key("enabled").boolean(r.get_enabled());
    o.key("hardBlocked").boolean(r.get_hard_blocked());
    o.key("softBlocked").boolean(r.get_soft_blocked());
    o.end();
}

/// A `WifiStatus` (the wifi sub-struct of `InterfaceConnectionStatus`).
#[cfg(feature = "schema-network")]
pub fn write_wifi_status(out: &mut String, w: net::wifi_status::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("ssid").str(w.get_ssid().map(|s| s.to_str().unwrap_or("")).unwrap_or(""));
    o.key("frequency").int(w.get_frequency());
    o.key("roaming").boolean(w.get_roaming());
    if let Ok(bssid) = w.get_bssid() {
        o.key("bssid").str(bssid.get_address().map(|a| a.to_str().unwrap_or("")).unwrap_or(""));
    }
    o.end();
}

/// An `InterfaceConnectionStatus` (the `getStatus` result / `net interface`).
#[cfg(feature = "schema-network")]
pub fn write_connection_status(out: &mut String, s: net::interface_connection_status::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("connected").boolean(s.get_connected());
    o.key("interface").int(s.get_interface().map(|i| i.get_id()).unwrap_or(0));
    o.key("type").str(s.get_type().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    o.key("strength").int(s.get_strength());
    o.key("speed").int(s.get_speed());
    o.key("quality").int(s.get_quality());
    o.key("metered").boolean(s.get_metered());
    o.key("behindCaptive").boolean(s.get_behind_captive());
    o.key("hostState").str(s.get_host_state().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    if let Ok(latency) = s.get_latency() {
        o.key("latency").nanos(latency.get_nanoseconds());
    }
    if let Ok(wifi) = s.get_wifi() {
        o.key("wifi").sub(|out| write_wifi_status(out, wifi));
    }
    o.end();
}

/// An `InterfaceCounters` (the bandwidth read / watch event).
#[cfg(feature = "schema-network")]
pub fn write_counters(out: &mut String, c: net::interface_counters::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("bytesIn").int(c.get_bytes_in());
    o.key("bytesOut").int(c.get_bytes_out());
    o.key("packetsIn").int(c.get_packets_in());
    o.key("packetsOut").int(c.get_packets_out());
    o.key("errorsIn").int(c.get_errors_in());
    o.key("errorsOut").int(c.get_errors_out());
    o.key("dropsIn").int(c.get_drops_in());
    o.key("dropsOut").int(c.get_drops_out());
    o.end();
}

/// A `WifiNetwork` (a scan result).
#[cfg(feature = "schema-network")]
pub fn write_wifi_network(out: &mut String, n: net::wifi_network::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("ssid").str(n.get_ssid().map(|s| s.to_str().unwrap_or("")).unwrap_or(""));
    o.key("strength").int(n.get_strength());
    o.key("frequency").int(n.get_frequency());
    o.key("security").str(n.get_security().map(|s| s.to_str().unwrap_or("")).unwrap_or(""));
    o.key("hidden").boolean(n.get_hidden());
    o.key("standard").str(n.get_standard().map(|s| s.to_str().unwrap_or("")).unwrap_or(""));
    if let Ok(bssid) = n.get_bssid() {
        o.key("bssid").str(bssid.get_address().map(|a| a.to_str().unwrap_or("")).unwrap_or(""));
    }
    o.end();
}

/// A `KnownWifiNetwork` (`net wifi list`).
#[cfg(feature = "schema-network")]
pub fn write_known_network(out: &mut String, n: net::known_wifi_network::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("ssid").str(n.get_ssid().map(|s| s.to_str().unwrap_or("")).unwrap_or(""));
    o.key("autoConnect").boolean(n.get_auto_connect());
    o.key("security").str(n.get_security().map(|s| s.to_str().unwrap_or("")).unwrap_or(""));
    if let Ok(last) = n.get_last_connected() {
        o.key("lastConnected").nanos(last.get_nanoseconds());
    }
    o.end();
}

/// A `Route` (the routing table entry).
#[cfg(feature = "schema-network")]
pub fn write_route(out: &mut String, r: net::route::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("destinationCidr")
        .str(r.get_destination_cidr().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    o.key("gateway").str(r.get_gateway().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    o.key("metric").int(r.get_metric());
    o.key("interfaceId").int(r.get_interface_id().map(|i| i.get_id()).unwrap_or(0));
    o.end();
}

/// A `ProxyConfig` (the host's own proxy).
#[cfg(feature = "schema-network")]
pub fn write_proxy_config(out: &mut String, c: net::proxy_config::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("enabled").boolean(c.get_enabled());
    o.key("url").str(c.get_url().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    if let Ok(bypass) = c.get_bypass() {
        let v = o.key("bypass");
        v.sub(|out| {
            let mut first = true;
            array(out, |out| {
                for entry in bypass.get_entries().into_iter().flatten() {
                    arr_sep(out, first);
                    first = false;
                    let value = entry.get_value().map(|t| t.to_str().unwrap_or("")).unwrap_or("");
                    write_str(out, value);
                }
            });
        });
    }
    o.end();
}

/// A network `DnsServer` (the `getDnsServers` result).
#[cfg(feature = "schema-network")]
pub fn write_dns_server(out: &mut String, s: net::dns_server::Reader<'_>) {
    let mut o = Obj::new(out);
    if let Ok(address) = s.get_address() {
        o.key("address").sub(|out| write_ip_address(out, address));
    }
    o.key("port").int(s.get_port().map(|p| p.get_number()).unwrap_or(0));
    o.key("tls").boolean(s.get_tls());
    o.end();
}

/// A `VpnProfile` (`net vpn` listing).
#[cfg(feature = "schema-network")]
pub fn write_vpn_profile(out: &mut String, p: net::vpn_profile::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("id").int(p.get_id().map(|i| i.get_id()).unwrap_or(0));
    o.key("name").str(p.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or(""));
    o.key("kind").str(p.get_kind().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    o.key("autoconnect").boolean(p.get_autoconnect());
    if let Ok(server) = p.get_server() {
        o.key("server").sub(|out| write_ip_address(out, server));
    }
    o.end();
}

/// A `VpnConnection`.
#[cfg(feature = "schema-network")]
pub fn write_vpn_connection(out: &mut String, c: net::vpn_connection::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("id").int(c.get_id().map(|i| i.get_id()).unwrap_or(0));
    o.key("profileId").int(c.get_profile_id().map(|i| i.get_id()).unwrap_or(0));
    o.key("bytesIn").int(c.get_bytes_in());
    o.key("bytesOut").int(c.get_bytes_out());
    o.end();
}

/// A `TracerouteHop`.
#[cfg(feature = "schema-network")]
pub fn write_traceroute_hop(out: &mut String, h: net::traceroute_hop::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("hop").int(h.get_hop());
    o.key("hostname").str(h.get_hostname().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    if let Ok(address) = h.get_address() {
        o.key("address").sub(|out| write_ip_address(out, address));
    }
    o.end();
}

/// A `FirewallRule` (the firewall read / hit-stats view).
#[cfg(feature = "schema-network")]
pub fn write_firewall_rule(out: &mut String, r: net::firewall_rule::Reader<'_>) {
    let mut o = Obj::new(out);
    o.key("name").str(r.get_name().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    o.key("enabled").boolean(r.get_enabled());
    o.key("direction").str(firewall_direction(r.get_direction().unwrap_or(net::FirewallDirection::Custom)));
    o.key("action").str(firewall_action(r.get_action().unwrap_or(net::FirewallAction::Custom)));
    o.key("protocol").str(r.get_protocol().map(|t| t.to_str().unwrap_or("")).unwrap_or(""));
    o.key("priority").int(r.get_priority());
    o.key("hitCount").int(r.get_hit_count());
    o.end();
}

/// A `ServiceDescriptor` (the discovery registry's listing).
#[cfg(feature = "schema-discovery")]
pub fn write_service_descriptor(out: &mut String, svc: crate::schema::discovery_capnp::service_descriptor::Reader<'_>) {
    let mut o = Obj::new(out);
    if let Ok(id) = svc.get_id() {
        o.key("id").int(id.get_id());
    }
    o.key("name").str(svc.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or(""));
    o.key("category").str(svc.get_category().map(|c| c.to_str().unwrap_or("")).unwrap_or(""));
    o.end();
}
