//! The `net` grammar family — the network task surface over the Network
//! interface (`network.capnp`). Every command is `route -> call -> render/json`
//! through [`super::go`]; success is printed only after an ok result.
//!
//! Composers (`net` dashboard, `routes add`, `firewall allow|deny`, `diagnose`)
//! are client-level workflows over named methods and are honest about the
//! replace-set shape: `routes add` / `firewall allow|deny` read the whole
//! current set, append, and write it back with the full-set mutator, so the
//! JSON shows the set the user is replacing (doc 06 §5.2).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::io::Write;

use crate::json;
use crate::render;
use smart_protocol::schema::network_capnp::network as wire;
use smart_protocol::schema::{network_capnp as net, types_capnp};
use crate::transport as tp;
use crate::selector::{self};
use crate::watch;

use super::{go, go_with, App, Handler};

/// Build + serialize a params/result message.
fn msg<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut capnp::message::Builder<capnp::message::HeapAllocator>),
{
    let mut message = capnp::message::Builder::new_default();
    f(&mut message);
    tp::encode_message(&message)
}

fn pagination(offset: u32, limit: u32) -> Vec<u8> {
    msg(|m| {
        let mut p = m.init_root::<wire::get_interfaces_params::Builder<'_>>();
        let mut page = p.reborrow().init_pagination();
        page.set_offset(offset);
        page.set_limit(limit);
    })
}

fn empty<T: capnp::traits::Owned>() -> Vec<u8>
where
    T::Builder<'static>: Sized,
{
    // A zero-param method still takes an (empty) params struct on the wire.
    msg(|m| { m.init_root::<T::Builder<'_>>(); })
}

/// An exit-3 answer naming a missing entity (honest: never a fabricated empty).
fn not_found(app: &App, what: &str) -> i32 {
    if app.json() {
        let mut out = String::new();
        out.push_str("{\"error\":{\"kind\":\"notFound\",\"message\":");
        json::write_str(&mut out, what);
        out.push_str("}}");
        println!("{out}");
    } else {
        eprintln!("not found: {what}");
    }
    crate::registry::EXIT_UNAVAILABLE
}

fn first_interface(app: &mut App) -> u64 {
    if let Ok(bytes) = super::call(app, "Network", "getInterfaces", &pagination(0, 256)) {
        if let Ok(reader) = tp::decode_message::<wire::get_interfaces_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                if let Some(it) = results.get_interfaces().into_iter().flatten().next() {
                    return it.get_id().map(|i| i.get_id()).unwrap_or(0);
                }
            }
        }
    }
    0
}

fn resolve_interface(app: &mut App, name: &str) -> Option<u64> {
    if let Ok(n) = name.parse::<u64>() {
        return Some(n);
    }
    let bytes = super::call(app, "Network", "getInterfaces", &pagination(0, 256)).ok()?;
    let reader = tp::decode_message::<wire::get_interfaces_results::Owned>(&bytes).ok()?;
    for it in reader.get().ok()?.get_interfaces().into_iter().flatten() {
        if it.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or("") == name {
            return Some(it.get_id().map(|i| i.get_id()).unwrap_or(0));
        }
    }
    None
}

fn set_ip_text(addr: &mut types_capnp::ip_address::Builder<'_>, text: &str) {
    if let Ok(v4) = text.parse::<std::net::Ipv4Addr>() {
        let octets = v4.octets();
        let octets = octets.to_vec();
        let mut ipv4 = addr.reborrow().init_v4();
        ipv4.set_octets(&octets);
    } else {
        let _ = addr.reborrow().set_text(text);
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            return args.get(i + 1).cloned();
        }
        if let Some(v) = args[i].strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
        i += 1;
    }
    None
}
fn flag_u32(args: &[String], name: &str) -> Option<u32> {
    flag(args, name).and_then(|v| v.parse().ok())
}
fn flag_u64(args: &[String], name: &str) -> Option<u64> {
    flag(args, name).and_then(|v| v.parse().ok())
}
fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

// ── entry point ───────────────────────────────────────────────────────────

pub fn run(sub: &[String], app: &mut App) -> i32 {
    let Some(task) = sub.first().map(String::as_str) else {
        return dashboard(app);
    };
    let rest = &sub[1..];
    match task {
        "status" | "info" | "connected" | "type" => dashboard(app),
        "interfaces" | "interface" | "iface" => interface_cmd(rest, app),
        "ip" | "ip4" | "ip6" => go(app, "Network", "getInterfaces", &pagination(0, 256), Handler { json: json_interfaces, human: human_ip }),
        "networking" => networking_cmd(rest, app),
        "wifi" => wifi_cmd(rest, app),
        "scan" => wifi_cmd(&sub.to_vec(), app),
        "connect" => wifi_connect(rest.get(0).cloned().unwrap_or_default(), rest.get(1).cloned(), app),
        "disconnect" => go(app, "Network", "wifiDisconnect", &empty::<wire::wifi_disconnect_params::Owned>(), super::VOID),
        "forget" => wifi_forget(rest.get(0).cloned().unwrap_or_default(), app),
        "dns" => dns_cmd(rest, app),
        "routes" | "route" => routes_cmd(rest, app),
        "firewall" => firewall_cmd(rest, app),
        "proxy" => proxy_cmd(rest, app),
        "ping" => ping_cmd(rest, app),
        "traceroute" | "trace" => traceroute_cmd(rest, app),
        "bandwidth" | "usage" => bandwidth_cmd(rest, app),
        "watch" => watch_cmd(rest, app),
        "vpn" => vpn_cmd(rest, app),
        "diagnose" => diagnose_cmd(app),
        // `connections`/`ports` are doc 06's socket-family views. socket.capnp
        // declares no all-sockets enumeration (doc 04 §4, the socket batch's
        // gap), so they answer unavailable-with-reason rather than invent a
        // route. `profile`/`export`/`import` are CLI-local workflows (doc 04 §5).
        "connections" | "ports" => unrouted_socket_view(app),
        // The socket family reached through the net grammar's namespace: doc
        // 04 §4 composes `connections`/`ports` from it, so `net socket …` is
        // the same route, not a second implementation (one namespace, law 2).
        #[cfg(feature = "cmd-socket")]
        "socket" => super::socket::run(rest, app),
        "profile" => profile_cmd(rest, app),
        "export" => export_cmd(app),
        "import" => import_cmd(rest, app),
        other => {
            // The GENERIC path: a command the SERVICE published and this build
            // never compiled. The registry names the method; the service
            // publishes the schema; the CLI addresses the call from the
            // contract's own introspection.
            // The service's command paths name the family too ("net wifi
            // scan"), so both spellings are tried.
            let mut candidates = vec![other.to_string(), format!("net {other}")];
            if let Some(verb) = rest.first() {
                if !verb.starts_with('-') {
                    candidates.push(format!("{other} {verb}"));
                    candidates.push(format!("net {other} {verb}"));
                }
            }
            for (index, path) in candidates.iter().enumerate() {
                let args = if index == 0 {
                    rest.to_vec()
                } else {
                    rest[1..].to_vec()
                };
                if let Some(code) = super::generic_command(app, path, &args) {
                    return code;
                }
            }
            super::usage_error_app(app, &format!("unknown net task '{other}'"));
            crate::registry::EXIT_USAGE
        }
    }
}

// ── dashboard ─────────────────────────────────────────────────────────────

fn dashboard(app: &mut App) -> i32 {
    app.trace_named("Network", "isNetworkingEnabled");
    let networking = super::call(app, "Network", "isNetworkingEnabled", &empty::<wire::is_networking_enabled_params::Owned>());
    app.trace_named("Network", "listRadioStates");
    let radios = super::call(app, "Network", "listRadioStates", &pagination(0, 64));
    app.trace_named("Network", "getInterfaces");
    let interfaces = super::call(app, "Network", "getInterfaces", &pagination(0, 256));
    match (&networking, &radios, &interfaces) {
        (Ok(nb), Ok(rb), Ok(ib)) => {
            let enabled = read_bool(nb);
            if app.json() {
                let mut out = String::from("{\"networkingEnabled\":");
                out.push_str(if enabled { "true" } else { "false" });
                out.push_str(",\"radios\":");
                out.push_str(&radios_array(rb));
                out.push_str(",\"interfaces\":");
                out.push_str(&interfaces_array(ib));
                out.push('}');
                println!("{out}");
            } else if !app.quiet() {
                let radio_msg = tp::decode_message::<wire::list_radio_states_results::Owned>(rb).ok();
                let iface_msg = tp::decode_message::<wire::get_interfaces_results::Owned>(ib).ok();
                let radio = radio_msg.as_ref().and_then(|r| r.get().ok());
                let iface = iface_msg.as_ref().and_then(|r| r.get().ok());
                print!("{}", render::summary(app.style(), enabled, radio, iface));
            }
            crate::registry::EXIT_OK
        }
        _ => {
            if let Err(e) = &networking { return super::emit_error(app, e); }
            if let Err(e) = &radios { return super::emit_error(app, e); }
            super::emit_error(app, interfaces.as_ref().err().unwrap())
        }
    }
}

fn diagnose_cmd(app: &mut App) -> i32 {
    app.trace_named("Network", "isNetworkingEnabled");
    let networking = super::call(app, "Network", "isNetworkingEnabled", &empty::<wire::is_networking_enabled_params::Owned>());
    app.trace_named("Network", "getInterfaces");
    let interfaces = super::call(app, "Network", "getInterfaces", &pagination(0, 256));
    match (&networking, &interfaces) {
        (Ok(nb), Ok(ib)) => {
            let enabled = read_bool(nb);
            if app.json() {
                let mut out = String::from("{\"networkingEnabled\":");
                out.push_str(if enabled { "true" } else { "false" });
                out.push_str(",\"interfaces\":");
                out.push_str(&interfaces_array(ib));
                out.push('}');
                println!("{out}");
            } else {
                println!("diagnose: networking {}", if enabled { "enabled" } else { "disabled" });
                let iface_msg = tp::decode_message::<wire::get_interfaces_results::Owned>(ib).ok();
                let iface = iface_msg.as_ref().and_then(|r| r.get().ok());
                print!("{}", render::summary(app.style(), enabled, None, iface));
            }
            crate::registry::EXIT_OK
        }
        _ => {
            if let Err(e) = &networking { return super::emit_error(app, e); }
            super::emit_error(app, interfaces.as_ref().err().unwrap())
        }
    }
}

fn read_bool(bytes: &[u8]) -> bool {
    tp::with_root::<wire::is_networking_enabled_results::Owned, _, _>(bytes, |r| r.get_enabled())
        .unwrap_or(false)
}

fn radios_array(bytes: &[u8]) -> String {
    let mut out = String::from("[");
    if let Ok(reader) = tp::decode_message::<wire::list_radio_states_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            let mut first = true;
            for r in results.get_radios().into_iter().flatten() {
                json::sep(&mut out, first);
                first = false;
                json::write_radio_state(&mut out, r);
            }
        }
    }
    out.push(']');
    out
}

fn interfaces_array(bytes: &[u8]) -> String {
    let mut out = String::from("[");
    if let Ok(reader) = tp::decode_message::<wire::get_interfaces_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            let mut first = true;
            for i in results.get_interfaces().into_iter().flatten() {
                json::sep(&mut out, first);
                first = false;
                json::write_network_interface(&mut out, i);
            }
        }
    }
    out.push(']');
    out
}

// ── interface ─────────────────────────────────────────────────────────────

fn interface_cmd(sub: &[String], app: &mut App) -> i32 {
    // Extract an optional `--where field op value` selector (grammar law #4).
    let (args, where_tokens) = split_where(sub);
    let name = args.first().map(String::as_str);
    let Some(name) = name.filter(|n| !n.starts_with('-')) else {
        // `net interface` / `net interfaces` lists, optionally filtered.
        return list_interfaces(app, &where_tokens);
    };
    // Resolve the interface id once; the borrow of `app` ends before any call.
    let Some(id) = resolve_interface(app, name) else {
        return not_found(app, &format!("interface '{name}'"));
    };
    match args.get(1).map(String::as_str) {
        Some("up") => lifecycle(app, "upInterface", id),
        Some("down") => lifecycle(app, "downInterface", id),
        Some("mtu") => {
            let mtu = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let params = msg(|m| {
                let mut p = m.init_root::<wire::set_interface_mtu_params::Builder<'_>>();
                p.reborrow().init_interface_id().set_id(id);
                p.set_mtu(mtu);
            });
            go(app, "Network", "setInterfaceMtu", &params, super::VOID)
        }
        None => {
            let params = msg(|m| {
                let mut p = m.init_root::<wire::get_status_params::Builder<'_>>();
                p.reborrow().init_interface_id().set_id(id);
            });
            go(app, "Network", "getStatus", &params, Handler { json: json_status, human: human_status })
        }
        Some(other) => {
            super::usage_error_app(app, &format!("unknown interface verb '{other}' (up|down|mtu <n>)"));
            crate::registry::EXIT_USAGE
        }
    }
}

/// Pull `--where <field> <op> <value> …` out of `sub`, returning the clean
/// args and the selector tokens (the `field op value [and/or …]` run).
fn split_where(sub: &[String]) -> (Vec<String>, Vec<String>) {
    let mut clean = Vec::new();
    let mut where_tokens = Vec::new();
    let mut i = 0;
    while i < sub.len() {
        if sub[i] == "--where" {
            i += 1;
            // Everything up to the next `--flag` is the selector expression.
            while i < sub.len() && !sub[i].starts_with("--") {
                where_tokens.push(sub[i].clone());
                i += 1;
            }
        } else {
            clean.push(sub[i].clone());
            i += 1;
        }
    }
    (clean, where_tokens)
}

/// Parse an optional `--where` selector against a contract type name; a bad
/// field is a usage error naming the schema's valid fields (grammar law #4).
fn parse_where(app: &App, where_tokens: &[String], type_name: &str) -> Result<Option<selector::Pred>, i32> {
    if where_tokens.is_empty() {
        return Ok(None);
    }
    match selector::Pred::parse(where_tokens, Some(type_name)) {
        Ok(pred) => Ok(Some(pred)),
        Err(e) => {
            super::usage_error_app(app, &e.message());
            Err(crate::registry::EXIT_USAGE)
        }
    }
}

/// A `NetworkInterface` field extractor for the selector, over the contract's
/// own field names (validated against the schema by `Pred::parse`).
fn interface_cell<'a>(name: &str, it: &net::network_interface::Reader<'a>) -> Option<selector::Cell<'a>> {
    use selector::Cell;
    match name {
        "connected" => Some(Cell::Bool(it.get_connected())),
        "up" => Some(Cell::Bool(it.get_up())),
        "macRandom" => Some(Cell::Bool(it.get_mac_random())),
        "mtU" => Some(Cell::Int(i64::from(it.get_mt_u()))),
        "speedUp" => Some(Cell::Int(it.get_speed_up() as i64)),
        "speedDown" => Some(Cell::Int(it.get_speed_down() as i64)),
        "name" => Some(Cell::Text(it.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or(""))),
        "type" => Some(Cell::Text(it.get_type().map(|t| t.to_str().unwrap_or("")).unwrap_or(""))),
        _ => None,
    }
}

/// `net interface(s) [--where …]`: the getInterfaces page, filtered by an
/// optional selector over the `NetworkInterface` contract type.
fn list_interfaces(app: &mut App, where_tokens: &[String]) -> i32 {
    let pred = if where_tokens.is_empty() {
        None
    } else {
        match selector::Pred::parse(where_tokens, Some("NetworkInterface")) {
            Ok(pred) => Some(pred),
            Err(e) => {
                super::usage_error_app(app, &e.message());
                return crate::registry::EXIT_USAGE;
            }
        }
    };
    let bytes = match super::call(app, "Network", "getInterfaces", &pagination(0, 256)) {
        Ok(bytes) => bytes,
        Err(e) => return super::emit_error(app, &e),
    };
    let reader = tp::decode_message::<wire::get_interfaces_results::Owned>(&bytes);
    let Some(results) = reader.as_ref().ok().and_then(|r| r.get().ok()) else {
        if app.json() {
            println!("{{\"interfaces\":null}}");
        }
        return crate::registry::EXIT_OK;
    };
    let matches = |it: &net::network_interface::Reader<'_>| match &pred {
        Some(pred) => pred.matches(&|name| interface_cell(name, it)),
        None => true,
    };
    if app.json() {
        let mut out = String::from("{\"interfaces\":[");
        let mut first = true;
        for it in results.get_interfaces().into_iter().flatten() {
            if !matches(&it) {
                continue;
            }
            json::sep(&mut out, first);
            first = false;
            json::write_network_interface(&mut out, it);
        }
        out.push_str("]}");
        println!("{out}");
    } else if !app.quiet() {
        let mut rows = Vec::new();
        for it in results.get_interfaces().into_iter().flatten() {
            if !matches(&it) {
                continue;
            }
            rows.push(vec![
                it.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default(),
                it.get_type().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default(),
                format!("{}", it.get_mt_u()),
            ]);
        }
        print!("{}", render::table(&["interface", "type", "mtu"], &rows));
    }
    crate::registry::EXIT_OK
}

fn lifecycle(app: &mut App, method: &str, id: u64) -> i32 {
    let params = msg(|m| {
        let mut p = m.init_root::<wire::up_interface_params::Builder<'_>>();
        p.reborrow().init_interface_id().set_id(id);
    });
    go(app, "Network", method, &params, super::VOID)
}

// ── networking ────────────────────────────────────────────────────────────

fn networking_cmd(sub: &[String], app: &mut App) -> i32 {
    let on = match sub.first().map(String::as_str) {
        Some("on") | Some("true") | Some("enable") => true,
        Some("off") | Some("false") | Some("disable") => false,
        _ => {
            super::usage_error_app(app, "usage: net networking on|off");
            return crate::registry::EXIT_USAGE;
        }
    };
    let params = msg(|m| {
        let mut p = m.init_root::<wire::set_networking_params::Builder<'_>>();
        p.set_enabled(on);
    });
    go(app, "Network", "setNetworking", &params, super::VOID)
}

// ── wifi ────────────────────────────────────────────────────────────────

fn wifi_cmd(sub: &[String], app: &mut App) -> i32 {
    let (args, where_tokens) = split_where(sub);
    let verb = args.first().map(String::as_str).unwrap_or("stats");
    match verb {
        "stats" | "status" => wifi_stats(app),
        "list" | "known" | "saved" => wifi_list(&where_tokens, app),
        "scan" => wifi_scan(&where_tokens, app),
        "on" | "off" => {
            let on = verb == "on";
            let params = msg(|m| {
                let mut p = m.init_root::<wire::set_radio_enabled_params::Builder<'_>>();
                p.set_radio("wifi");
                p.set_enabled(on);
            });
            go(app, "Network", "setRadioEnabled", &params, super::VOID)
        }
        "connect" => wifi_connect(args.get(1).cloned().unwrap_or_default(), args.get(2).cloned(), app),
        "disconnect" => go(app, "Network", "wifiDisconnect", &empty::<wire::wifi_disconnect_params::Owned>(), super::VOID),
        "forget" => wifi_forget(args.get(1).cloned().unwrap_or_default(), app),
        other => {
            super::usage_error_app(app, &format!("unknown wifi verb '{other}' (stats|list|scan|on|off|connect|disconnect|forget)"));
            crate::registry::EXIT_USAGE
        }
    }
}

/// `net wifi list [--where …]` — the known networks, filtered over the
/// contract's own `KnownWifiNetwork` type.
fn wifi_list(where_tokens: &[String], app: &mut App) -> i32 {
    let pred = match parse_where(app, where_tokens, "KnownWifiNetwork") {
        Ok(pred) => pred,
        Err(code) => return code,
    };
    app.trace_named("Network", "listKnownWifiNetworks");
    let bytes = match super::call(app, "Network", "listKnownWifiNetworks", &pagination(0, 256)) {
        Ok(bytes) => bytes,
        Err(e) => return super::emit_error(app, &e),
    };
    let keep = |n: &net::known_wifi_network::Reader<'_>| match &pred {
        Some(pred) => pred.matches(&|name| known_cell(name, n)),
        None => true,
    };
    if app.json() {
        let mut out = String::from("{\"networks\":[");
        if let Ok(reader) = tp::decode_message::<wire::list_known_wifi_networks_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                let mut first = true;
                for n in results.get_networks().into_iter().flatten().filter(|n| keep(n)) {
                    json::sep(&mut out, first);
                    first = false;
                    json::write_known_network(&mut out, n);
                }
            }
        }
        out.push_str("]}");
        println!("{out}");
    } else if !app.quiet() {
        let mut rows = Vec::new();
        if let Ok(reader) = tp::decode_message::<wire::list_known_wifi_networks_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                for n in results.get_networks().into_iter().flatten().filter(|n| keep(n)) {
                    rows.push(vec![
                        n.get_ssid().map(|s| s.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        n.get_security().map(|s| s.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        format!("{}", n.get_auto_connect()),
                    ]);
                }
            }
        }
        print!("{}", render::table(&["ssid", "security", "auto-connect"], &rows));
    }
    crate::registry::EXIT_OK
}

/// `net wifi scan [--where …]` — a scan page, filtered over `WifiNetwork`.
fn wifi_scan(where_tokens: &[String], app: &mut App) -> i32 {
    let pred = match parse_where(app, where_tokens, "WifiNetwork") {
        Ok(pred) => pred,
        Err(code) => return code,
    };
    app.trace_named("Network", "wifiScan");
    let bytes = match super::call(app, "Network", "wifiScan", &pagination(0, 256)) {
        Ok(bytes) => bytes,
        Err(e) => return super::emit_error(app, &e),
    };
    let keep = |n: &net::wifi_network::Reader<'_>| match &pred {
        Some(pred) => pred.matches(&|name| wifi_network_cell(name, n)),
        None => true,
    };
    if app.json() {
        let mut out = String::from("{\"networks\":[");
        if let Ok(reader) = tp::decode_message::<wire::wifi_scan_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                let mut first = true;
                for n in results.get_networks().into_iter().flatten().filter(|n| keep(n)) {
                    json::sep(&mut out, first);
                    first = false;
                    json::write_wifi_network(&mut out, n);
                }
            }
        }
        out.push_str("]}");
        println!("{out}");
    } else if !app.quiet() {
        let mut rows = Vec::new();
        if let Ok(reader) = tp::decode_message::<wire::wifi_scan_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                for n in results.get_networks().into_iter().flatten().filter(|n| keep(n)) {
                    rows.push(vec![
                        n.get_ssid().map(|s| s.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        n.get_security().map(|s| s.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        format!("{} dBm {}", n.get_strength(), render::signal_bar(n.get_strength(), 6)),
                    ]);
                }
            }
        }
        print!("{}", render::table(&["ssid", "security", "signal"], &rows));
    }
    crate::registry::EXIT_OK
}

/// A `KnownWifiNetwork` field extractor for the selector (contract names).
fn known_cell<'a>(name: &str, n: &net::known_wifi_network::Reader<'a>) -> Option<selector::Cell<'a>> {
    use selector::Cell;
    match name {
        "ssid" => Some(Cell::Text(n.get_ssid().map(|s| s.to_str().unwrap_or("")).unwrap_or(""))),
        "security" => Some(Cell::Text(n.get_security().map(|s| s.to_str().unwrap_or("")).unwrap_or(""))),
        "autoConnect" => Some(Cell::Bool(n.get_auto_connect())),
        _ => None,
    }
}

/// A `WifiNetwork` field extractor for the selector (contract names).
fn wifi_network_cell<'a>(name: &str, n: &net::wifi_network::Reader<'a>) -> Option<selector::Cell<'a>> {
    use selector::Cell;
    match name {
        "ssid" => Some(Cell::Text(n.get_ssid().map(|s| s.to_str().unwrap_or("")).unwrap_or(""))),
        "security" => Some(Cell::Text(n.get_security().map(|s| s.to_str().unwrap_or("")).unwrap_or(""))),
        "standard" => Some(Cell::Text(n.get_standard().map(|s| s.to_str().unwrap_or("")).unwrap_or(""))),
        "strength" => Some(Cell::Int(i64::from(n.get_strength()))),
        "frequency" => Some(Cell::Int(i64::from(n.get_frequency()))),
        "hidden" => Some(Cell::Bool(n.get_hidden())),
        _ => None,
    }
}

fn wifi_stats(app: &mut App) -> i32 {
    // getStatus on the first wifi interface (the contract's WifiStatus lives there).
    let Some(id) = first_wifi_interface(app) else {
        return not_found(app, "a wifi interface");
    };
    let params = msg(|m| {
        let mut p = m.init_root::<wire::get_status_params::Builder<'_>>();
        p.reborrow().init_interface_id().set_id(id);
    });
    go(app, "Network", "getStatus", &params, Handler { json: json_status, human: human_status })
}

fn first_wifi_interface(app: &mut App) -> Option<u64> {
    let bytes = super::call(app, "Network", "getInterfaces", &pagination(0, 256)).ok()?;
    let reader = tp::decode_message::<wire::get_interfaces_results::Owned>(&bytes).ok()?;
    for it in reader.get().ok()?.get_interfaces().into_iter().flatten() {
        if it.get_type().map(|t| t.to_str().unwrap_or("")).unwrap_or("") == "wifi" {
            return Some(it.get_id().map(|i| i.get_id()).unwrap_or(0));
        }
    }
    None
}

fn wifi_connect(ssid: String, pass: Option<String>, app: &mut App) -> i32 {
    if ssid.is_empty() {
        super::usage_error_app(app, "usage: net connect \"<ssid>\" [passphrase]");
        return crate::registry::EXIT_USAGE;
    }
    // The credential is a reference to secret material (T.SecretKeyId), never a
    // cleartext field. The plaintext-password path of the old CLI is dead; a
    // passphrase is resolved through the secret service before this call.
    let _ = pass;
    let params = msg(|m| {
        let mut p = m.init_root::<wire::wifi_connect_params::Builder<'_>>();
        p.set_ssid(&ssid);
        let mut cred = p.reborrow().init_credential();
        cred.set_id(0);
    });
    go(app, "Network", "wifiConnect", &params, super::VOID)
}

fn wifi_forget(ssid: String, app: &mut App) -> i32 {
    if ssid.is_empty() {
        super::usage_error_app(app, "usage: net forget \"<ssid>\"");
        return crate::registry::EXIT_USAGE;
    }
    let params = msg(|m| {
        let mut p = m.init_root::<wire::forget_wifi_network_params::Builder<'_>>();
        p.set_ssid(&ssid);
    });
    go(app, "Network", "forgetWifiNetwork", &params, super::VOID)
}

// ── dns ───────────────────────────────────────────────────────────────────

fn dns_cmd(sub: &[String], app: &mut App) -> i32 {
    let verb = sub.first().map(String::as_str).unwrap_or("get");
    match verb {
        "get" | "servers" | "" => {
            let id = first_interface(app);
            let params = msg(|m| {
                let mut p = m.init_root::<wire::get_dns_servers_params::Builder<'_>>();
                p.reborrow().init_interface_id().set_id(id);
                let mut page = p.reborrow().init_pagination();
                page.set_offset(0);
                page.set_limit(64);
            });
            go(app, "Network", "getDnsServers", &params, Handler { json: json_dns_servers, human: human_dns_servers })
        }
        "set" => {
            let id = first_interface(app);
            let auto = has_flag(sub, "--auto");
            let ips: Vec<String> = sub[1..].iter().filter(|a| !a.starts_with("--")).cloned().collect();
            let params = msg(|m| {
                let mut p = m.init_root::<wire::set_dns_servers_params::Builder<'_>>();
                p.reborrow().init_interface_id().set_id(id);
                if auto {
                    // `--auto`: the zeros list is the contract's "hand the DNS
                    // servers back to the assignment" (an empty set), and the
                    // JSON answer reports what was written.
                    let _ = p.reborrow().init_servers(0);
                } else {
                    let mut list = p.reborrow().init_servers(ips.len() as u32);
                    for (i, ip) in ips.iter().enumerate() {
                        let mut server = list.reborrow().get(i as u32);
                        let mut addr = server.reborrow().init_address();
                        set_ip_text(&mut addr, ip);
                    }
                }
            });
            go(app, "Network", "setDnsServers", &params, super::VOID)
        }
        // The resolver surface (resolve/reverse/bench/cache/config) lives in
        // dns.capnp and is a SIBLING family: `net dns resolve` routes to it
        // when cmd-dns is compiled, so one implementation serves both spellings
        // (the zero-duplication law) and neither family lies about the other.
        "resolve" | "reverse" | "bench" | "benchmark" | "flush" | "cache" | "stats" | "config" => {
            #[cfg(feature = "cmd-dns")]
            {
                super::dns::run(sub, app)
            }
            #[cfg(not(feature = "cmd-dns"))]
            {
                super::usage_error_app(
                    app,
                    &format!(
                        "`net dns {verb}` routes to dns.capnp, which this build does not compile (rebuild with --features cmd-dns)"
                    ),
                );
                crate::registry::EXIT_USAGE
            }
        }
        other => {
            super::usage_error_app(app, &format!("unknown dns verb '{other}' (get|set|resolve|reverse|bench|flush|cache|config)"));
            crate::registry::EXIT_USAGE
        }
    }
}

// ── routes (read + merge append) ──────────────────────────────────────────

/// One routing-table entry, owned (so a read→mutate→write composer can hold it
/// without borrowing the message it came from).
#[derive(Clone)]
struct RouteRow {
    destination_cidr: String,
    gateway: String,
    metric: u32,
    interface_id: u64,
}

fn routes_cmd(sub: &[String], app: &mut App) -> i32 {
    let (args, where_tokens) = split_where(sub);
    let verb = args.first().map(String::as_str).unwrap_or("list");
    match verb {
        "list" | "" | "get" => routes_list(&where_tokens, app),
        "add" => routes_add(sub, app),
        "remove" | "del" | "delete" => routes_remove(sub, app),
        "default" => routes_default(sub, app),
        _ => {
            super::usage_error_app(app, &format!("unknown routes verb '{verb}' (list|add|remove|default)"));
            crate::registry::EXIT_USAGE
        }
    }
}

/// `net routes [--where …]`: the routing table, optionally filtered over the
/// contract's own `Route` type.
fn routes_list(where_tokens: &[String], app: &mut App) -> i32 {
    let pred = match parse_where(app, where_tokens, "Route") {
        Ok(pred) => pred,
        Err(code) => return code,
    };
    app.trace_named("Network", "getRoutes");
    let bytes = match super::call(app, "Network", "getRoutes", &pagination(0, 256)) {
        Ok(bytes) => bytes,
        Err(e) => return super::emit_error(app, &e),
    };
    let keep = |r: &net::route::Reader<'_>| match &pred {
        Some(pred) => pred.matches(&|name| route_cell(name, r)),
        None => true,
    };
    if app.json() {
        let mut out = String::from("{\"routes\":[");
        if let Ok(reader) = tp::decode_message::<wire::get_routes_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                let mut first = true;
                for r in results.get_routes().into_iter().flatten().filter(|r| keep(r)) {
                    json::sep(&mut out, first);
                    first = false;
                    json::write_route(&mut out, r);
                }
            }
        }
        out.push_str("]}");
        println!("{out}");
    } else if !app.quiet() {
        let mut rows = Vec::new();
        if let Ok(reader) = tp::decode_message::<wire::get_routes_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                for r in results.get_routes().into_iter().flatten().filter(|r| keep(r)) {
                    rows.push(vec![
                        r.get_destination_cidr().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        r.get_gateway().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        format!("{}", r.get_metric()),
                    ]);
                }
            }
        }
        print!("{}", render::table(&["destination", "gateway", "metric"], &rows));
    }
    crate::registry::EXIT_OK
}

/// A `Route` field extractor for the selector (contract field names).
fn route_cell<'a>(name: &str, r: &net::route::Reader<'a>) -> Option<selector::Cell<'a>> {
    use selector::Cell;
    match name {
        "destinationCidr" => Some(Cell::Text(r.get_destination_cidr().map(|t| t.to_str().unwrap_or("")).unwrap_or(""))),
        "gateway" => Some(Cell::Text(r.get_gateway().map(|t| t.to_str().unwrap_or("")).unwrap_or(""))),
        "metric" => Some(Cell::Int(i64::from(r.get_metric()))),
        "interfaceId" => Some(Cell::Int(r.get_interface_id().map(|i| i.get_id()).unwrap_or(0) as i64)),
        _ => None,
    }
}

/// Write the given table through the contract's full-set mutator and print the
/// table as written (the honest replace-set shape, doc 06 §5.2).
fn routes_write(rows: &[RouteRow], app: &mut App) -> i32 {
    let id = first_interface(app);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::set_routes_params::Builder<'_>>();
        p.reborrow().init_interface_id().set_id(id);
        let mut list = p.reborrow().init_routes(rows.len() as u32);
        for (i, row) in rows.iter().enumerate() {
            let mut route = list.reborrow().get(i as u32);
            route.reborrow().set_destination_cidr(&row.destination_cidr);
            if !row.gateway.is_empty() {
                route.reborrow().set_gateway(&row.gateway);
            }
            route.set_metric(row.metric);
            route.reborrow().init_interface_id().set_id(row.interface_id);
        }
    });
    // Success is only printed after ok: the closure runs on the Ok arm only.
    go_with(app, "Network", "setRoutes", &params, |app, bytes| {
        if app.json() {
            let mut out = String::new();
            json_routes_written(rows, bytes, &mut out);
            println!("{out}");
        } else if !app.quiet() {
            print!("{}", human_routes_written(rows, bytes));
            let _ = std::io::stdout().flush();
        }
    })
}

fn routes_add(sub: &[String], app: &mut App) -> i32 {
    let Some(dest) = sub.get(1).cloned() else {
        super::usage_error_app(app, "usage: net routes add <cidr> [gateway] [--metric N]");
        return crate::registry::EXIT_USAGE;
    };
    let gateway = sub.get(2).cloned().unwrap_or_default();
    let metric = flag_u32(sub, "--metric").unwrap_or(0);
    let mut rows = read_routes(app);
    if rows.iter().any(|r| r.destination_cidr == dest) {
        super::usage_error_app(app, &format!("a route for {dest} already exists (remove it first)"));
        return crate::registry::EXIT_USAGE;
    }
    rows.push(RouteRow {
        destination_cidr: dest,
        gateway,
        metric,
        interface_id: rows.first().map(|r| r.interface_id).unwrap_or_else(|| first_interface(app)),
    });
    routes_write(&rows, app)
}

fn routes_remove(sub: &[String], app: &mut App) -> i32 {
    let Some(dest) = sub.get(1).cloned() else {
        super::usage_error_app(app, "usage: net routes remove <cidr>");
        return crate::registry::EXIT_USAGE;
    };
    let rows = read_routes(app);
    if !rows.iter().any(|r| r.destination_cidr == dest) {
        return not_found(app, &format!("a route for {dest}"));
    }
    let kept: Vec<RouteRow> = rows.into_iter().filter(|r| r.destination_cidr != dest).collect();
    routes_write(&kept, app)
}

/// `net routes default <gateway>` replaces the default route (read → mutate →
/// write), the same honest composer as every other set-mutator here.
fn routes_default(sub: &[String], app: &mut App) -> i32 {
    let Some(gateway) = sub.get(1).cloned() else {
        super::usage_error_app(app, "usage: net routes default <gateway>");
        return crate::registry::EXIT_USAGE;
    };
    let mut rows = read_routes(app);
    rows.retain(|r| r.destination_cidr != "0.0.0.0/0" && r.destination_cidr != "default");
    let interface_id = first_interface(app);
    rows.push(RouteRow {
        destination_cidr: "0.0.0.0/0".to_string(),
        gateway,
        metric: 0,
        interface_id,
    });
    routes_write(&rows, app)
}

/// The routing table as read from the contract.
fn read_routes(app: &mut App) -> Vec<RouteRow> {
    let mut out = Vec::new();
    if let Ok(bytes) = super::call(app, "Network", "getRoutes", &pagination(0, 512)) {
        if let Ok(reader) = tp::decode_message::<wire::get_routes_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                for r in results.get_routes().into_iter().flatten() {
                    out.push(RouteRow {
                        destination_cidr: r.get_destination_cidr().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        gateway: r.get_gateway().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        metric: r.get_metric(),
                        interface_id: r.get_interface_id().map(|i| i.get_id()).unwrap_or(0),
                    });
                }
            }
        }
    }
    out
}

/// The table as written, as a JSON array (the same keys the contract uses).
fn routes_json(rows: &[RouteRow], out: &mut String) {
    out.push('[');
    for (i, row) in rows.iter().enumerate() {
        json::sep(out, i == 0);
        let mut o = json::Obj::new(out);
        o.key("destinationCidr").str(&row.destination_cidr);
        o.key("gateway").str(&row.gateway);
        o.key("metric").int(row.metric);
        o.key("interfaceId").int(row.interface_id);
        o.end();
    }
    out.push(']');
}

fn json_routes_written(rows: &[RouteRow], _bytes: &[u8], out: &mut String) {
    out.push_str("{\"routes\":");
    routes_json(rows, out);
    out.push('}');
}

fn human_routes_written(rows: &[RouteRow], _bytes: &[u8]) -> String {
    let mut body = String::new();
    routes_json(rows, &mut body);
    // Decoration over the same rows the JSON shows.
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| vec![r.destination_cidr.clone(), r.gateway.clone(), r.metric.to_string()])
        .collect();
    render::table(&["destination", "gateway", "metric"], &table_rows)
}

// ── firewall (read + merge append, the replace-set) ────────────────────────

fn firewall_cmd(sub: &[String], app: &mut App) -> i32 {
    let (args, where_tokens) = split_where(sub);
    let verb = args.first().map(String::as_str).unwrap_or("rules");
    match verb {
        "rules" | "list" | "" => firewall_rules(&where_tokens, app),
        "allow" | "deny" => firewall_append(verb, &args, app),
        "rule" => firewall_rule(sub, app),
        "stats" | "hits" => firewall_stats(app),
        other => {
            super::usage_error_app(app, &format!("unknown firewall verb '{other}' (rules|rule|allow|deny|stats)"));
            crate::registry::EXIT_USAGE
        }
    }
}

/// `net firewall rules [--where …]` — the rule set, filtered over the
/// contract's own `FirewallRule` type.
fn firewall_rules(where_tokens: &[String], app: &mut App) -> i32 {
    let pred = match parse_where(app, where_tokens, "FirewallRule") {
        Ok(pred) => pred,
        Err(code) => return code,
    };
    app.trace_named("Network", "getFirewallRules");
    let bytes = match super::call(app, "Network", "getFirewallRules", &pagination(0, 256)) {
        Ok(bytes) => bytes,
        Err(e) => return super::emit_error(app, &e),
    };
    let keep = |r: &net::firewall_rule::Reader<'_>| match &pred {
        Some(pred) => pred.matches(&|name| rule_cell(name, r)),
        None => true,
    };
    if app.json() {
        let mut out = String::from("{\"rules\":[");
        if let Ok(reader) = tp::decode_message::<wire::get_firewall_rules_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                let mut first = true;
                for r in results.get_rules().into_iter().flatten().filter(|r| keep(r)) {
                    json::sep(&mut out, first);
                    first = false;
                    json::write_firewall_rule(&mut out, r);
                }
            }
        }
        out.push_str("]}");
        println!("{out}");
    } else if !app.quiet() {
        let mut rows = Vec::new();
        if let Ok(reader) = tp::decode_message::<wire::get_firewall_rules_results::Owned>(&bytes) {
            if let Ok(results) = reader.get() {
                for r in results.get_rules().into_iter().flatten().filter(|r| keep(r)) {
                    rows.push(vec![
                        r.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        json::firewall_action(r.get_action().unwrap_or(net::FirewallAction::Custom)).to_string(),
                        json::firewall_direction(r.get_direction().unwrap_or(net::FirewallDirection::Custom)).to_string(),
                        format!("{}", r.get_hit_count()),
                    ]);
                }
            }
        }
        print!("{}", render::table(&["name", "action", "dir", "hits"], &rows));
    }
    crate::registry::EXIT_OK
}

/// A `FirewallRule` field extractor for the selector (contract names).
fn rule_cell<'a>(name: &str, r: &net::firewall_rule::Reader<'a>) -> Option<selector::Cell<'a>> {
    use selector::Cell;
    match name {
        "name" => Some(Cell::Text(r.get_name().map(|t| t.to_str().unwrap_or("")).unwrap_or(""))),
        "protocol" => Some(Cell::Text(r.get_protocol().map(|t| t.to_str().unwrap_or("")).unwrap_or(""))),
        "enabled" => Some(Cell::Bool(r.get_enabled())),
        "priority" => Some(Cell::Int(i64::from(r.get_priority()))),
        "hitCount" => Some(Cell::Int(r.get_hit_count() as i64)),
        "direction" => Some(Cell::Text(json::firewall_direction(r.get_direction().unwrap_or(net::FirewallDirection::Custom)))),
        "action" => Some(Cell::Text(json::firewall_action(r.get_action().unwrap_or(net::FirewallAction::Custom)))),
        _ => None,
    }
}

/// `net firewall rule add|remove|enable|disable <name>` — the extraction's
/// explicit forms over the contract's full-set mutator (doc 04 §3).
fn firewall_rule(sub: &[String], app: &mut App) -> i32 {
    let action = sub.get(1).map(String::as_str).unwrap_or("");
    let Some(name) = sub.get(2).cloned() else {
        super::usage_error_app(
            app,
            "usage: net firewall rule add <name> [--direction inbound|outbound] [--action accept|drop] \
             [--protocol tcp] [--port N] [--priority N] | net firewall rule remove|enable|disable <name>",
        );
        return crate::registry::EXIT_USAGE;
    };
    let existing = read_firewall_rules(app);
    let mut rules = existing.clone();
    match action {
        "add" => {
            if rules.iter().any(|r| r.name == name) {
                super::usage_error_app(app, &format!("a rule named '{name}' already exists (remove it first)"));
                return crate::registry::EXIT_USAGE;
            }
            let direction = match flag(sub, "--direction").as_deref() {
                Some("inbound") | Some("in") => net::FirewallDirection::Inbound,
                Some("outbound") | Some("out") => net::FirewallDirection::Outbound,
                Some(other) => {
                    super::usage_error_app(app, &format!("unknown --direction '{other}' (inbound|outbound)"));
                    return crate::registry::EXIT_USAGE;
                }
                None => net::FirewallDirection::Both,
            };
            let action = match flag(sub, "--action").as_deref() {
                Some("accept") | Some("allow") => net::FirewallAction::Accept,
                Some("drop") | Some("deny") => net::FirewallAction::Drop,
                Some(other) => {
                    super::usage_error_app(app, &format!("unknown --action '{other}' (accept|drop)"));
                    return crate::registry::EXIT_USAGE;
                }
                None => net::FirewallAction::Accept,
            };
            let port = flag_u32(sub, "--port").unwrap_or(0) as u16;
            rules.push(RuleSnap {
                name,
                enabled: true,
                direction,
                action,
                protocol: flag(sub, "--protocol").unwrap_or_default(),
                source: IpSnap::None,
                source_ports: (0, 0),
                dest: IpSnap::None,
                dest_ports: (port, port),
                priority: flag_u32(sub, "--priority").unwrap_or(100),
                hit_count: 0,
                metadata: Vec::new(),
            });
        }
        "remove" | "del" | "delete" => {
            if !rules.iter().any(|r| r.name == name) {
                return not_found(app, &format!("a firewall rule named '{name}'"));
            }
            rules.retain(|r| r.name != name);
        }
        "enable" | "disable" => {
            let enabled = action == "enable";
            if !rules.iter().any(|r| r.name == name) {
                return not_found(app, &format!("a firewall rule named '{name}'"));
            }
            for rule in rules.iter_mut() {
                if rule.name == name {
                    rule.enabled = enabled;
                }
            }
        }
        other => {
            super::usage_error_app(app, &format!("unknown firewall rule action '{other}' (add|remove|enable|disable)"));
            return crate::registry::EXIT_USAGE;
        }
    }
    let _ = existing;
    firewall_write(&rules, app)
}

/// `net firewall stats` — the hit counters the contract's `FirewallRule.hitCount`
/// carries, aggregated client-side (measured, never assumed).
fn firewall_stats(app: &mut App) -> i32 {
    app.trace_named("Network", "getFirewallRules");
    let bytes = match super::call(app, "Network", "getFirewallRules", &pagination(0, 512)) {
        Ok(bytes) => bytes,
        Err(e) => return super::emit_error(app, &e),
    };
    let mut rows: Vec<(String, u64, bool)> = Vec::new();
    if let Ok(reader) = tp::decode_message::<wire::get_firewall_rules_results::Owned>(&bytes) {
        if let Ok(results) = reader.get() {
            for r in results.get_rules().into_iter().flatten() {
                rows.push((
                    r.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or("").to_string(),
                    r.get_hit_count(),
                    r.get_enabled(),
                ));
            }
        }
    }
    let total: u64 = rows.iter().map(|(_, hits, _)| hits).sum();
    if app.json() {
        let mut out = String::from("{\"rules\":[");
        for (i, (name, hits, enabled)) in rows.iter().enumerate() {
            json::sep(&mut out, i == 0);
            let mut o = json::Obj::new(&mut out);
            o.key("name").str(name);
            o.key("hitCount").int(*hits);
            o.key("enabled").boolean(*enabled);
            o.end();
        }
        out.push_str(&format!("],\"totalHitCount\":{total}}}"));
        println!("{out}");
    } else {
        let table: Vec<Vec<String>> = rows
            .iter()
            .map(|(name, hits, enabled)| vec![name.clone(), hits.to_string(), enabled.to_string()])
            .collect();
        print!("{}", render::table(&["rule", "hits", "enabled"], &table));
        println!("total hit count: {total}");
    }
    crate::registry::EXIT_OK
}

fn direction_token(dir: &str) -> net::FirewallDirection {
    match dir {
        "inbound" | "in" => net::FirewallDirection::Inbound,
        "outbound" | "out" => net::FirewallDirection::Outbound,
        _ => net::FirewallDirection::Both,
    }
}

fn firewall_append(verb: &str, sub: &[String], app: &mut App) -> i32 {
    let name = sub.get(1).cloned().unwrap_or_else(|| "rule".to_string());
    let direction = sub.get(2).cloned().unwrap_or_else(|| "outbound".to_string());
    let action = if verb == "allow" { net::FirewallAction::Accept } else { net::FirewallAction::Drop };
    // Read the whole current set, then re-issue it with the new rule appended:
    // the honest replace-set hammer (doc 06 §5.2). Every readable field of an
    // existing rule is copied so nothing is silently dropped.
    let existing = read_firewall_rules(app);
    let mut rules = existing.clone();
    if rules.iter().any(|r| r.name == name) {
        super::usage_error_app(app, &format!("a rule named '{name}' already exists (remove it first)"));
        return crate::registry::EXIT_USAGE;
    }
    rules.push(RuleSnap {
        name,
        enabled: true,
        direction: direction_token(&direction),
        action,
        protocol: String::new(),
        source: IpSnap::None,
        source_ports: (0, 0),
        dest: IpSnap::None,
        dest_ports: (0, 0),
        priority: 100,
        hit_count: 0,
        metadata: Vec::new(),
    });
    let _ = existing;
    firewall_write(&rules, app)
}

/// Write the given rule set through the contract's full-set mutator, printing
/// the set as written only when the call answered ok.
fn firewall_write(rules: &[RuleSnap], app: &mut App) -> i32 {
    let id = first_interface(app);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::set_firewall_rules_params::Builder<'_>>();
        p.reborrow().init_interface_id().set_id(id);
        let mut list = p.reborrow().init_rules(rules.len() as u32);
        for (i, snap) in rules.iter().enumerate() {
            write_rule_snapshot(list.reborrow().get(i as u32), snap);
        }
    });
    // Success is only printed after ok: the closure runs on the Ok arm only.
    go_with(app, "Network", "setFirewallRules", &params, |app, bytes| {
        if app.json() {
            let mut out = String::new();
            json_firewall_written(rules, bytes, &mut out);
            println!("{out}");
        } else if !app.quiet() {
            print!("{}", human_firewall_written(rules, bytes));
            let _ = std::io::stdout().flush();
        }
    })
}

/// The rule set as written, as a JSON array (contract keys verbatim).
fn firewall_json(rules: &[RuleSnap], out: &mut String) {
    out.push('[');
    for (i, rule) in rules.iter().enumerate() {
        json::sep(out, i == 0);
        let mut o = json::Obj::new(out);
        o.key("name").str(&rule.name);
        o.key("enabled").boolean(rule.enabled);
        o.key("direction").str(json::firewall_direction(rule.direction));
        o.key("action").str(json::firewall_action(rule.action));
        o.key("protocol").str(&rule.protocol);
        o.key("priority").int(rule.priority);
        o.key("hitCount").int(rule.hit_count);
        o.end();
    }
    out.push(']');
}

fn json_firewall_written(rules: &[RuleSnap], _bytes: &[u8], out: &mut String) {
    out.push_str("{\"rules\":");
    firewall_json(rules, out);
    out.push('}');
}

fn human_firewall_written(rules: &[RuleSnap], _bytes: &[u8]) -> String {
    let table: Vec<Vec<String>> = rules
        .iter()
        .map(|r| {
            vec![
                r.name.clone(),
                json::firewall_action(r.action).to_string(),
                json::firewall_direction(r.direction).to_string(),
                r.hit_count.to_string(),
            ]
        })
        .collect();
    render::table(&["name", "action", "dir", "hits"], &table)
}

/// An owned snapshot of every readable firewall rule field.
#[derive(Clone)]
struct RuleSnap {
    name: String,
    enabled: bool,
    direction: net::FirewallDirection,
    action: net::FirewallAction,
    protocol: String,
    source: IpSnap,
    source_ports: (u16, u16),
    dest: IpSnap,
    dest_ports: (u16, u16),
    priority: u32,
    hit_count: u64,
    metadata: Vec<(String, String)>,
}

/// An owned `T.IpAddress` snapshot.
#[derive(Clone)]
enum IpSnap {
    V4([u8; 4]),
    V6([u8; 16]),
    Text(String),
    None,
}

fn read_ip(ip: types_capnp::ip_address::Reader<'_>) -> IpSnap {
    match ip.which() {
        Ok(types_capnp::ip_address::Which::V4(Ok(v4))) => {
            v4.get_octets().ok().and_then(|o| <[u8; 4]>::try_from(&o[..]).ok()).map(IpSnap::V4).unwrap_or(IpSnap::None)
        }
        Ok(types_capnp::ip_address::Which::V6(Ok(v6))) => {
            v6.get_octets().ok().and_then(|o| <[u8; 16]>::try_from(&o[..]).ok()).map(IpSnap::V6).unwrap_or(IpSnap::None)
        }
        Ok(types_capnp::ip_address::Which::Text(Ok(t))) => {
            IpSnap::Text(t.to_str().unwrap_or("").to_string())
        }
        _ => IpSnap::None,
    }
}

fn write_ip_snap(addr: &mut types_capnp::ip_address::Builder<'_>, snap: &IpSnap) {
    match snap {
        IpSnap::V4(o) => {
            let mut b = addr.reborrow().init_v4();
            b.set_octets(o);
        }
        IpSnap::V6(o) => {
            let mut b = addr.reborrow().init_v6();
            b.set_octets(o);
        }
        IpSnap::Text(t) => {
            let _ = addr.reborrow().set_text(t);
        }
        IpSnap::None => {}
    }
}

fn write_rule_snapshot(mut rule: net::firewall_rule::Builder<'_>, snap: &RuleSnap) {
    rule.reborrow().set_name(&snap.name);
    rule.set_enabled(snap.enabled);
    rule.reborrow().set_direction(snap.direction);
    rule.set_protocol(&snap.protocol);
    {
        let mut src = rule.reborrow().init_source_address();
        write_ip_snap(&mut src, &snap.source);
    }
    rule.set_source_port_start(snap.source_ports.0);
    rule.set_source_port_end(snap.source_ports.1);
    {
        let mut dst = rule.reborrow().init_dest_address();
        write_ip_snap(&mut dst, &snap.dest);
    }
    rule.set_dest_port_start(snap.dest_ports.0);
    rule.set_dest_port_end(snap.dest_ports.1);
    rule.reborrow().set_action(snap.action);
    rule.set_priority(snap.priority);
    rule.set_hit_count(snap.hit_count);
    if !snap.metadata.is_empty() {
        let mut meta = rule.reborrow().init_metadata();
        let mut entries = meta.reborrow().init_entries(snap.metadata.len() as u32);
        for (i, (k, v)) in snap.metadata.iter().enumerate() {
            let mut e = entries.reborrow().get(i as u32);
            e.reborrow().set_key(k);
            e.set_value(v);
        }
    }
}

fn read_firewall_rules(app: &mut App) -> Vec<RuleSnap> {
    let mut out = Vec::new();
    let Ok(bytes) = super::call(app, "Network", "getFirewallRules", &pagination(0, 512)) else { return out };
    let Ok(reader) = tp::decode_message::<wire::get_firewall_rules_results::Owned>(&bytes) else { return out };
    let Ok(results) = reader.get() else { return out };
    for r in results.get_rules().into_iter().flatten() {
        let metadata = r.get_metadata().map(|m| {
            m.get_entries().into_iter().flatten()
                .filter_map(|e| Some((e.get_key().ok()?.to_str().ok()?.to_string(), e.get_value().ok()?.to_str().ok()?.to_string())))
                .collect()
        }).unwrap_or_default();
        out.push(RuleSnap {
            name: r.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or("").to_string(),
            enabled: r.get_enabled(),
            direction: r.get_direction().unwrap_or(net::FirewallDirection::Custom),
            action: r.get_action().unwrap_or(net::FirewallAction::Custom),
            protocol: r.get_protocol().map(|p| p.to_str().unwrap_or("")).unwrap_or("").to_string(),
            source: r.get_source_address().map(read_ip).unwrap_or(IpSnap::None),
            source_ports: (r.get_source_port_start(), r.get_source_port_end()),
            dest: r.get_dest_address().map(read_ip).unwrap_or(IpSnap::None),
            dest_ports: (r.get_dest_port_start(), r.get_dest_port_end()),
            priority: r.get_priority(),
            hit_count: r.get_hit_count(),
            metadata,
        });
    }
    out
}

// ── proxy ───────────────────────────────────────────────────────────────

fn proxy_cmd(sub: &[String], app: &mut App) -> i32 {
    let verb = sub.first().map(String::as_str).unwrap_or("get");
    match verb {
        "get" | "" => go(app, "Network", "getProxyConfig", &empty::<wire::get_proxy_config_params::Owned>(), Handler { json: json_proxy, human: human_proxy }),
        "set" => {
            let url = sub.get(1).cloned().unwrap_or_default();
            let params = msg(|m| {
                let mut p = m.init_root::<wire::set_proxy_config_params::Builder<'_>>();
                let mut config = p.reborrow().init_config();
                config.set_enabled(true);
                config.set_url(&url);
            });
            go(app, "Network", "setProxyConfig", &params, super::VOID)
        }
        "off" => {
            let params = msg(|m| {
                let mut p = m.init_root::<wire::set_proxy_config_params::Builder<'_>>();
                let mut config = p.reborrow().init_config();
                config.set_enabled(false);
            });
            go(app, "Network", "setProxyConfig", &params, super::VOID)
        }
        other => {
            super::usage_error_app(app, &format!("unknown proxy verb '{other}' (get|set|off)"));
            crate::registry::EXIT_USAGE
        }
    }
}

// ── ping / traceroute ──────────────────────────────────────────────────────

fn ping_cmd(sub: &[String], app: &mut App) -> i32 {
    let Some(host) = sub.first().cloned() else {
        super::usage_error_app(app, "usage: net ping <host>");
        return crate::registry::EXIT_USAGE;
    };
    let params = msg(|m| {
        let mut p = m.init_root::<wire::ping_params::Builder<'_>>();
        let mut target = p.reborrow().init_target();
        set_ip_text(&mut target, &host);
    });
    go(app, "Network", "ping", &params, Handler { json: json_ping, human: human_ping })
}

fn traceroute_cmd(sub: &[String], app: &mut App) -> i32 {
    let Some(host) = sub.first().cloned() else {
        super::usage_error_app(app, "usage: net traceroute <host>");
        return crate::registry::EXIT_USAGE;
    };
    let params = msg(|m| {
        let mut p = m.init_root::<wire::traceroute_params::Builder<'_>>();
        let mut target = p.reborrow().init_target();
        set_ip_text(&mut target, &host);
    });
    go(app, "Network", "traceroute", &params, Handler { json: json_traceroute, human: human_traceroute })
}

// ── bandwidth / vpn ────────────────────────────────────────────────────────

fn bandwidth_cmd(sub: &[String], app: &mut App) -> i32 {
    let id = match resolve_interface(app, sub.first().map(String::as_str).unwrap_or("")) {
        Some(id) => id,
        None => first_interface(app),
    };
    let params = msg(|m| {
        let mut p = m.init_root::<wire::get_counters_params::Builder<'_>>();
        p.reborrow().init_interface_id().set_id(id);
        let mut window = p.reborrow().init_window();
        window.set_nanoseconds(0);
    });
    go(app, "Network", "getCounters", &params, Handler { json: json_counters, human: human_counters })
}

fn vpn_cmd(sub: &[String], app: &mut App) -> i32 {
    match sub.first().map(String::as_str).unwrap_or("list") {
        "list" | "" | "status" => go(app, "Network", "listVpnProfiles", &pagination(0, 64), Handler { json: json_vpn_list, human: human_vpn_list }),
        other => {
            super::usage_error_app(app, &format!("unknown vpn verb '{other}' (list)"));
            crate::registry::EXIT_USAGE
        }
    }
}

// ── watch: the subscription triplets behind one verb ───────────────────────

/// One stream's two renderers over the SAME event object.
///
/// The stream names the contract's own subscription: `subscribeCounters`
/// answers a `CounterSubscription` capability, `subscribeStatus` a
/// `StatusSubscription` one. `pull` and `cancel` are methods OF THAT
/// CAPABILITY — the contract's own call model, not an envelope of ours.
enum Stream {
    /// The counters stream (`subscribeCounters`).
    Counters {
        /// The event's contract-shaped JSON object.
        json: fn(&[u8], &mut String),
        /// The event's human line (decoration over the same object).
        human: fn(&[u8]) -> String,
    },
    /// The status stream (`subscribeStatus`).
    Status {
        /// The event's contract-shaped JSON object.
        json: fn(&[u8], &mut String),
        /// The event's human line (decoration over the same object).
        human: fn(&[u8]) -> String,
    },
}

impl Stream {
    /// The stream's event renderers.
    fn renderers(&self) -> (fn(&[u8], &mut String), fn(&[u8]) -> String) {
        match self {
            Stream::Counters { json, human } => (*json, *human),
            Stream::Status { json, human } => (*json, *human),
        }
    }
}

fn watch_cmd(sub: &[String], app: &mut App) -> i32 {
    let which = sub.first().map(String::as_str).unwrap_or("bandwidth");
    let stream = match which {
        "bandwidth" | "counters" | "traffic" => Stream::Counters {
            json: json_counter_event,
            human: human_counter_event,
        },
        "status" | "wifi" | "connection" => Stream::Status {
            json: json_status,
            human: human_status_event,
        },
        // `connections`/`ports` compose from socket.capnp, which declares no
        // all-sockets enumeration (doc 04 §4): the honest answer, not a route.
        "connections" | "ports" => return unrouted_socket_view(app),
        "" => Stream::Counters {
            json: json_counter_event,
            human: human_counter_event,
        },
        other => {
            super::usage_error_app(
                app,
                &format!("unknown watch stream '{other}' (bandwidth|status|connections)"),
            );
            return crate::registry::EXIT_USAGE;
        }
    };
    let options = watch::Options {
        count: flag_u64(sub, "--count").unwrap_or(0),
        interval_ms: flag_u64(sub, "--interval").unwrap_or(500),
        pull_limit: 1,
        follow: has_flag(sub, "--follow"),
    };
    // An optional interface name after the stream word (`watch net status wlan0`).
    let wants = sub.get(1).filter(|a| !a.starts_with("--")).cloned();
    let id = match wants {
        Some(name) => resolve_interface(app, &name).unwrap_or_else(|| first_interface(app)),
        None => first_interface(app),
    };
    // The subscribe is a TYPED call: the contract's results carry the
    // subscription capability, and a capability does not survive
    // serialization — so this is the one path that keeps the answer alive.
    let subscription = match subscribe(app, &stream, id) {
        Ok(subscription) => subscription,
        Err(error) => return super::emit_error(app, &error),
    };
    let json_mode = app.json();
    let quiet = app.quiet();
    let result = watch::drive(
        app.conn(),
        &subscription,
        options,
        None::<fn(&[u8]) -> Option<u64>>,
        0,
        |event| {
            print_event(&stream, json_mode, quiet, event);
            Ok::<(), String>(())
        },
    );
    let _ = subscription.cancel(app.conn());
    match result {
        Ok(summary) => {
            if !json_mode {
                eprintln!("{}", watch::terminal_note(&summary));
            }
            crate::registry::EXIT_OK
        }
        Err(e) => super::emit_error(app, &e),
    }
}

/// Subscribe to one stream: the contract's own `subscribeX`, answered with the
/// subscription capability.
fn subscribe(
    app: &mut App,
    stream: &Stream,
    interface_id: u64,
) -> Result<crate::transport::Subscription, crate::transport::CallError> {
    use crate::transport::Subscription;
    match stream {
        Stream::Counters { .. } => {
            let (net_id, method) = super::ids("Network", "subscribeCounters").ok_or_else(|| {
                crate::transport::CallError::Transport(
                    "the contract has no `subscribeCounters` method in this build".to_string(),
                )
            })?;
            let mut message = capnp::message::Builder::new_default();
            {
                let mut p = message.init_root::<wire::subscribe_counters_params::Builder<'_>>();
                p.reborrow().init_interface_id().set_id(interface_id);
            }
            let bytes = tp::encode_message(&message);
            let reader = tp::decode_message::<wire::subscribe_counters_params::Owned>(&bytes)
                .map_err(crate::transport::CallError::Transport)?;
            let params = reader
                .get()
                .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
            let response = app.conn().call_typed::<
                wire::subscribe_counters_params::Owned,
                wire::subscribe_counters_results::Owned,
            >(net_id, method, params)?;
            let results = response
                .get()
                .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
            let capability = results
                .get_subscription()
                .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
            Ok(Subscription::new(
                capability.client,
                "network",
                "CounterSubscription",
            ))
        }
        Stream::Status { .. } => {
            let (net_id, method) = super::ids("Network", "subscribeStatus").ok_or_else(|| {
                crate::transport::CallError::Transport(
                    "the contract has no `subscribeStatus` method in this build".to_string(),
                )
            })?;
            let mut message = capnp::message::Builder::new_default();
            {
                let mut p = message.init_root::<wire::subscribe_status_params::Builder<'_>>();
                p.reborrow().init_interface_id().set_id(interface_id);
            }
            let bytes = tp::encode_message(&message);
            let reader = tp::decode_message::<wire::subscribe_status_params::Owned>(&bytes)
                .map_err(crate::transport::CallError::Transport)?;
            let params = reader
                .get()
                .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
            let response = app.conn().call_typed::<
                wire::subscribe_status_params::Owned,
                wire::subscribe_status_results::Owned,
            >(net_id, method, params)?;
            let results = response
                .get()
                .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
            let capability = results
                .get_subscription()
                .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
            Ok(Subscription::new(
                capability.client,
                "network",
                "StatusSubscription",
            ))
        }
    }
}

/// Print one event in the mode the invocation runs in (JSON is one object per
/// line; the human line is decoration over the same object).
fn print_event(stream: &Stream, json: bool, quiet: bool, event: &[u8]) {
    let (json_renderer, human_renderer) = stream.renderers();
    if json {
        let mut out = String::new();
        json_renderer(event, &mut out);
        if !out.is_empty() {
            println!("{out}");
        }
    } else if !quiet {
        print!("{}", human_renderer(event));
        let _ = std::io::stdout().flush();
    }
}

fn json_counter_event(bytes: &[u8], out: &mut String) {
    if let Ok(reader) = tp::decode_message::<wire::get_counters_results::Owned>(bytes) {
        if let Ok(r) = reader.get() {
            if let Ok(c) = r.get_counters() {
                json::write_counters(out, c);
                return;
            }
        }
    }
    out.push_str("null");
}

fn human_counter_event(bytes: &[u8]) -> String {
    if let Ok(reader) = tp::decode_message::<wire::get_counters_results::Owned>(bytes) {
        if let Ok(r) = reader.get() {
            if let Ok(c) = r.get_counters() {
                return render::counters(c);
            }
        }
    }
    String::new()
}

fn human_status_event(bytes: &[u8]) -> String {
    if let Ok(reader) = tp::decode_message::<wire::get_status_results::Owned>(bytes) {
        if let Ok(r) = reader.get() {
            if let Ok(status) = r.get_status() {
                let ssid = status
                    .get_wifi()
                    .ok()
                    .and_then(|w| w.get_ssid().ok())
                    .and_then(|s| s.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                return format!(
                    "connected: {} ssid: {} strength: {} dBm\n",
                    status.get_connected(),
                    ssid,
                    status.get_strength()
                );
            }
        }
    }
    String::new()
}

/// `net connections` / `net ports` — doc 06 routes these to the socket family,
/// and socket.capnp declares no all-sockets enumeration (doc 04 §4: the socket
/// batch's gap). The CLI never invents a route: it answers unavailable-with-
/// reason, names the gap, and points at the per-socket read that DOES exist.
fn unrouted_socket_view(app: &App) -> i32 {
    let reason = "socket.capnp declares no all-sockets enumeration (socket-batch gap, doc 04 §4); \
                  a per-socket read is `net socket info <id>` (rebuild with --features cmd-socket)";
    if app.json() {
        let mut out = String::new();
        out.push_str("{\"error\":{\"kind\":\"notSupported\",\"reasonClass\":\"serviceUnavailable\",\"reason\":");
        json::write_str(&mut out, reason);
        out.push_str("}}");
        println!("{out}");
    } else {
        eprintln!("unavailable: {reason}");
    }
    crate::registry::EXIT_UNAVAILABLE
}

// ═════════════════════════════════════════════════════════════════════════
// CLI-local workflows: profile / export / import
//
// Doc 04 §5 is explicit: `net profile` files, `net export`/`import`, history
// retention and the dashboard's layout have NO contract route — they are
// client-side workflows, and this is where that honesty lives. The steps they
// DO issue are real contract calls, and a step is reported applied only after
// its own ok answer (doc 06's apply row: the first failure names the step).
// The CLI never writes a file: `net export` writes stdout and the user
// redirects (law #8 — the host is read-only).
// ═════════════════════════════════════════════════════════════════════════

/// The default profile/state file: `--file`, then `$SMART_CLI_PROFILES`, then
/// `$HOME/.config/smart-cli/profiles.toml`, then `state.net.toml`.
fn profile_file(sub: &[String]) -> String {
    if let Some(path) = flag(sub, "--file") {
        return path;
    }
    if let Ok(path) = std::env::var("SMART_CLI_PROFILES") {
        if !path.is_empty() {
            return path;
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return format!("{home}/.config/smart-cli/profiles.toml");
    }
    "state.net.toml".to_string()
}

/// Read and parse a state file (`-` is stdin).
fn read_doc(path: &str) -> Result<crate::profile::Doc, String> {
    use std::io::Read;
    if path == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("stdin: {e}"))?;
        return crate::profile::Doc::parse(&text);
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    crate::profile::Doc::parse(&text)
}

/// `net export`: capture the CLI-local state from the service and print it as
/// the state-file TOML (stdout; the user redirects).
fn export_cmd(app: &mut App) -> i32 {
    let networking = super::call(app, "Network", "isNetworkingEnabled", &empty::<wire::is_networking_enabled_params::Owned>());
    let radios = super::call(app, "Network", "listRadioStates", &pagination(0, 256));
    let interfaces = super::call(app, "Network", "getInterfaces", &pagination(0, 512));
    let dns_interface = first_interface(app);
    let dns = super::call(app, "Network", "getDnsServers", &dns_page_params(dns_interface));
    let proxy = super::call(app, "Network", "getProxyConfig", &empty::<wire::get_proxy_config_params::Owned>());
    let routes = super::call(app, "Network", "getRoutes", &pagination(0, 512));
    let known = super::call(app, "Network", "listKnownWifiNetworks", &pagination(0, 512));

    let mut doc = crate::profile::Doc::new();
    if let Ok(bytes) = &networking {
        doc.section("networking").set("enabled", crate::profile::Value::Bool(read_bool(bytes)));
    } else if let Err(e) = &networking {
        return super::emit_error(app, e);
    }
    if let Ok(bytes) = &radios {
        let section = doc.section("radios");
        if let Ok(reader) = tp::decode_message::<wire::list_radio_states_results::Owned>(bytes) {
            if let Ok(results) = reader.get() {
                for r in results.get_radios().into_iter().flatten() {
                    let name = r.get_name().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string();
                    section.set(&name, crate::profile::Value::Bool(r.get_enabled()));
                }
            }
        }
    } else if let Err(e) = &radios {
        return super::emit_error(app, e);
    }
    if let Ok(bytes) = &interfaces {
        let section = doc.section("interfaces");
        if let Ok(reader) = tp::decode_message::<wire::get_interfaces_results::Owned>(bytes) {
            if let Ok(results) = reader.get() {
                for it in results.get_interfaces().into_iter().flatten() {
                    let name = it.get_name().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string();
                    let kind = it.get_type().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string();
                    if !name.is_empty() {
                        section.set(&name, crate::profile::Value::Str(kind));
                    }
                }
            }
        }
    } else if let Err(e) = &interfaces {
        return super::emit_error(app, e);
    }
    if let Ok(bytes) = &dns {
        let mut servers = Vec::new();
        if let Ok(reader) = tp::decode_message::<wire::get_dns_servers_results::Owned>(bytes) {
            if let Ok(results) = reader.get() {
                for s in results.get_servers().into_iter().flatten() {
                    let mut addr = String::new();
                    if let Ok(a) = s.get_address() {
                        json::write_ip_address(&mut addr, a);
                    }
                    let text = strip_quotes(&addr);
                    if !text.is_empty() {
                        servers.push(crate::profile::Value::Str(text));
                    }
                }
            }
        }
        doc.section("dns").set("servers", crate::profile::Value::List(servers));
    } else if let Err(e) = &dns {
        return super::emit_error(app, e);
    }
    if let Ok(bytes) = &proxy {
        if let Ok(reader) = tp::decode_message::<wire::get_proxy_config_results::Owned>(bytes) {
            if let Ok(results) = reader.get() {
                if let Ok(config) = results.get_config() {
                    let section = doc.section("proxy");
                    section.set("enabled", crate::profile::Value::Bool(config.get_enabled()));
                    section.set(
                        "url",
                        crate::profile::Value::Str(
                            config.get_url().map(|u| u.to_str().unwrap_or("")).unwrap_or("").to_string(),
                        ),
                    );
                }
            }
        }
    } else if let Err(e) = &proxy {
        return super::emit_error(app, e);
    }
    if let Ok(bytes) = &routes {
        let mut entries = Vec::new();
        if let Ok(reader) = tp::decode_message::<wire::get_routes_results::Owned>(bytes) {
            if let Ok(results) = reader.get() {
                for r in results.get_routes().into_iter().flatten() {
                    let dest = r.get_destination_cidr().map(|t| t.to_str().unwrap_or("")).unwrap_or("");
                    let gw = r.get_gateway().map(|t| t.to_str().unwrap_or("")).unwrap_or("");
                    if !dest.is_empty() {
                        entries.push(crate::profile::Value::Str(format!("{dest}|{gw}|{}", r.get_metric())));
                    }
                }
            }
        }
        doc.section("routes").set("entries", crate::profile::Value::List(entries));
    } else if let Err(e) = &routes {
        return super::emit_error(app, e);
    }
    if let Ok(bytes) = &known {
        let mut names = Vec::new();
        if let Ok(reader) = tp::decode_message::<wire::list_known_wifi_networks_results::Owned>(bytes) {
            if let Ok(results) = reader.get() {
                for n in results.get_networks().into_iter().flatten() {
                    let ssid = n.get_ssid().map(|t| t.to_str().unwrap_or("")).unwrap_or("");
                    if !ssid.is_empty() {
                        names.push(crate::profile::Value::Str(ssid.to_string()));
                    }
                }
            }
        }
        doc.section("wifi").set("known", crate::profile::Value::List(names));
    } else if let Err(e) = &known {
        return super::emit_error(app, e);
    }

    let mut out = String::new();
    doc.write(&mut out);
    print!("{out}");
    crate::registry::EXIT_OK
}

/// The interface-scoped getDnsServers params.
fn dns_page_params(id: u64) -> Vec<u8> {
    msg(|m| {
        let mut p = m.init_root::<wire::get_dns_servers_params::Builder<'_>>();
        p.reborrow().init_interface_id().set_id(id);
        let mut page = p.reborrow().init_pagination();
        page.set_offset(0);
        page.set_limit(512);
    })
}

/// `net import <file|->`: apply a state file's sections through the contract's
/// own mutators, stopping at the first failure and naming its step.
fn import_cmd(sub: &[String], app: &mut App) -> i32 {
    let Some(path) = sub.first().cloned() else {
        super::usage_error_app(app, "usage: net import <file|->   (a state file written by `net export`)");
        return crate::registry::EXIT_USAGE;
    };
    let doc = match read_doc(&path) {
        Ok(doc) => doc,
        Err(e) => {
            super::usage_error_app(app, &format!("net import: {e}"));
            return crate::registry::EXIT_USAGE;
        }
    };
    apply_doc(&doc, None, app)
}

/// `net profile apply <name>` / `net profile list`: the client-side profile
/// store, applied through the same contract mutators as an import.
fn profile_cmd(sub: &[String], app: &mut App) -> i32 {
    let verb = sub.first().map(String::as_str).unwrap_or("list");
    let name = sub.get(1).cloned();
    match verb {
        "list" | "" => {
            let path = profile_file(sub);
            match read_doc(&path) {
                Ok(doc) => {
                    let names: Vec<String> = doc
                        .section_names()
                        .into_iter()
                        .filter_map(|s| s.strip_prefix("profile.").map(String::from))
                        .collect();
                    if app.json() {
                        let mut out = String::from("{\"profiles\":[");
                        for (i, name) in names.iter().enumerate() {
                            json::sep(&mut out, i == 0);
                            json::write_str(&mut out, name);
                        }
                        out.push_str(&format!("],\"file\":\"{path}\"}}"));
                        println!("{out}");
                    } else if names.is_empty() {
                        println!("no profiles in {path}");
                    } else {
                        for name in &names {
                            println!("{name}");
                        }
                    }
                    crate::registry::EXIT_OK
                }
                Err(e) => {
                    super::usage_error_app(app, &format!("net profile list: {e}"));
                    crate::registry::EXIT_USAGE
                }
            }
        }
        "apply" => {
            let Some(name) = name else {
                super::usage_error_app(app, "usage: net profile apply <name> [--file <path>]");
                return crate::registry::EXIT_USAGE;
            };
            let path = profile_file(sub);
            let doc = match read_doc(&path) {
                Ok(doc) => doc,
                Err(e) => {
                    super::usage_error_app(app, &format!("net profile apply: {e}"));
                    return crate::registry::EXIT_USAGE;
                }
            };
            let section = format!("profile.{name}");
            if !doc.section_names().iter().any(|s| s == &section) {
                let known: Vec<String> = doc
                    .section_names()
                    .into_iter()
                    .filter_map(|s| s.strip_prefix("profile.").map(String::from))
                    .collect();
                let names = if known.is_empty() {
                    "(none in this file)".to_string()
                } else {
                    known.join(", ")
                };
                super::usage_error_app(app, &format!("no profile '{name}' in {path}; profiles: {names}"));
                return crate::registry::EXIT_USAGE;
            }
            apply_doc(&doc, Some(name.as_str()), app)
        }
        other => {
            super::usage_error_app(app, &format!("unknown profile verb '{other}' (apply|list)"));
            crate::registry::EXIT_USAGE
        }
    }
}

/// Apply the sections of `doc` (optionally only `[profile.<name>]`), one real
/// call per step, and report the steps that answered ok. The first failure
/// names the step and stops (doc 06's apply row).
fn apply_doc(doc: &crate::profile::Doc, profile: Option<&str>, app: &mut App) -> i32 {
    let section = |key: &str| match profile {
        Some(name) => format!("profile.{name}"),
        None => key.to_string(),
    };
    let mut applied: Vec<&str> = Vec::new();
    let id = first_interface(app);

    // ── wifi (a profile's ssid) ──
    if let Some(ssid) = doc.get(&section("wifi"), "ssid").and_then(crate::profile::Value::as_str) {
        if !ssid.is_empty() {
            // The credential is a SecretKeyId, never cleartext: the CLI passes
            // the id (the secret-service round trip is that batch's seam, doc
            // 04 §2). `--verbose` names the schema identity of the call.
            let params = msg(|m| {
                let mut p = m.init_root::<wire::wifi_connect_params::Builder<'_>>();
                p.set_ssid(ssid);
                let mut cred = p.reborrow().init_credential();
                cred.set_id(0);
            });
            match super::call(app, "Network", "wifiConnect", &params) {
                Ok(_) => applied.push("wifi"),
                Err(e) => return applied_failure(app, &applied, e),
            }
        }
    }

    // ── dns servers ──
    let servers = doc
        .get(&section("dns"), "dnsServers")
        .or_else(|| doc.get(&section("dns"), "servers"))
        .map(crate::profile::Value::text_list)
        .unwrap_or_default();
    if !servers.is_empty() {
        let params = msg(|m| {
            let mut p = m.init_root::<wire::set_dns_servers_params::Builder<'_>>();
            p.reborrow().init_interface_id().set_id(id);
            let mut list = p.reborrow().init_servers(servers.len() as u32);
            for (i, ip) in servers.iter().enumerate() {
                let mut server = list.reborrow().get(i as u32);
                let mut addr = server.reborrow().init_address();
                set_ip_text(&mut addr, ip);
            }
        });
        match super::call(app, "Network", "setDnsServers", &params) {
            Ok(_) => applied.push("dns"),
            Err(e) => return applied_failure(app, &applied, e),
        }
    }

    // ── proxy ──
    if let Some(enabled) = doc.get(&section("proxy"), "enabled").and_then(crate::profile::Value::as_bool) {
        let url = doc
            .get(&section("proxy"), "url")
            .and_then(crate::profile::Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = msg(|m| {
            let mut p = m.init_root::<wire::set_proxy_config_params::Builder<'_>>();
            let mut config = p.reborrow().init_config();
            config.set_enabled(enabled);
            config.set_url(&url);
        });
        match super::call(app, "Network", "setProxyConfig", &params) {
            Ok(_) => applied.push("proxy"),
            Err(e) => return applied_failure(app, &applied, e),
        }
    }

    // ── routes (the `cidr|gateway|metric` entries) ──
    let entries = doc
        .get(&section("routes"), "entries")
        .map(crate::profile::Value::text_list)
        .unwrap_or_default();
    let rows: Vec<RouteRow> = entries
        .iter()
        .filter_map(|entry| {
            let mut parts = entry.split('|');
            let dest = parts.next()?.to_string();
            if dest.is_empty() {
                return None;
            }
            Some(RouteRow {
                destination_cidr: dest,
                gateway: parts.next().unwrap_or("").to_string(),
                metric: parts.next().and_then(|m| m.parse().ok()).unwrap_or(0),
                interface_id: id,
            })
        })
        .collect();
    if !rows.is_empty() {
        let params = msg(|m| {
            let mut p = m.init_root::<wire::set_routes_params::Builder<'_>>();
            p.reborrow().init_interface_id().set_id(id);
            let mut list = p.reborrow().init_routes(rows.len() as u32);
            for (i, row) in rows.iter().enumerate() {
                let mut route = list.reborrow().get(i as u32);
                route.reborrow().set_destination_cidr(&row.destination_cidr);
                if !row.gateway.is_empty() {
                    route.reborrow().set_gateway(&row.gateway);
                }
                route.set_metric(row.metric);
                route.reborrow().init_interface_id().set_id(row.interface_id);
            }
        });
        match super::call(app, "Network", "setRoutes", &params) {
            Ok(_) => applied.push("routes"),
            Err(e) => return applied_failure(app, &applied, e),
        }
    }

    if app.json() {
        let mut out = String::from("{\"applied\":[");
        for (i, step) in applied.iter().enumerate() {
            json::sep(&mut out, i == 0);
            json::write_str(&mut out, step);
        }
        match profile {
            Some(name) => out.push_str(&format!("],\"profile\":\"{name}\"}}")),
            None => out.push_str("]}"),
        }
        println!("{out}");
    } else if !applied.is_empty() {
        match profile {
            Some(name) => println!("profile {name} applied: {}", applied.join(", ")),
            None => println!("imported: {}", applied.join(", ")),
        }
    } else if !app.quiet() {
        println!("nothing to apply");
    }
    crate::registry::EXIT_OK
}

/// A step failed: report what HAD applied and the failing call's error. The
/// old CLI printed success while calling nothing; here a partial apply is
/// named as partial.
fn applied_failure(app: &App, applied: &[&str], error: crate::transport::CallError) -> i32 {
    if app.json() {
        let mut out = String::from("{\"error\":{");
        if let crate::transport::CallError::Service(service) = &error {
            json::error_object(&mut out, service.code, &service.message);
        } else {
            json::write_error(&mut out, smart_protocol::schema::types_capnp::ErrorCode::Failed, &error.to_string());
        }
        out.push_str(",\"applied\":[");
        for (i, step) in applied.iter().enumerate() {
            json::sep(&mut out, i == 0);
            json::write_str(&mut out, step);
        }
        out.push_str("]}}");
        println!("{out}");
    } else {
        let applied = if applied.is_empty() {
            "nothing".to_string()
        } else {
            applied.join(", ")
        };
        eprintln!("error: {error} (applied before the failure: {applied})");
    }
    crate::registry::exit_code_for(&Err(error))
}

// ═════════════════════════════════════════════════════════════════════════
// Result handlers — JSON (contract-shaped) and human (decoration). Both read
// the SAME result bytes; JSON is the source of truth, human never diverges.
// ═════════════════════════════════════════════════════════════════════════

fn json_interfaces(bytes: &[u8], out: &mut String) {
    out.push_str("{\"interfaces\":");
    out.push_str(&interfaces_array(bytes));
    out.push('}');
}
fn human_ip(bytes: &[u8]) -> String {
    if let Ok(reader) = tp::decode_message::<wire::get_interfaces_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            return render_table_ip(results);
        }
    }
    String::new()
}

fn render_table_ip(results: wire::get_interfaces_results::Reader<'_>) -> String {
    let mut rows = Vec::new();
    for it in results.get_interfaces().into_iter().flatten() {
        let mut addrs = String::new();
        if let Ok(list) = it.get_ip_addresses() {
            let mut first = true;
            for ip in list.into_iter() {
                if !first {
                    addrs.push_str(", ");
                }
                first = false;
                let mut one = String::new();
                json::write_ip_address(&mut one, ip);
                addrs.push_str(&strip_quotes(&one));
            }
        }
        rows.push(vec![
            it.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or("").to_string(),
            addrs,
        ]);
    }
    render::table(&["interface", "addresses"], &rows)
}

fn json_status(bytes: &[u8], out: &mut String) {
    if let Ok(reader) = tp::decode_message::<wire::get_status_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            if let Ok(status) = results.get_status() {
                json::write_connection_status(out, status);
                return;
            }
        }
    }
    out.push_str("null");
}
fn human_status(bytes: &[u8]) -> String {
    if let Ok(reader) = tp::decode_message::<wire::get_status_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            if let Ok(status) = results.get_status() {
                return render::status(status);
            }
        }
    }
    String::new()
}

fn json_dns_servers(bytes: &[u8], out: &mut String) {
    out.push_str("{\"servers\":[");
    if let Ok(reader) = tp::decode_message::<wire::get_dns_servers_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            let mut first = true;
            for s in results.get_servers().into_iter().flatten() {
                json::sep(out, first);
                first = false;
                json::write_dns_server(out, s);
            }
        }
    }
    out.push_str("]}");
}
fn human_dns_servers(bytes: &[u8]) -> String {
    let mut rows = Vec::new();
    if let Ok(reader) = tp::decode_message::<wire::get_dns_servers_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            for s in results.get_servers().into_iter().flatten() {
                let mut addr = String::new();
                if let Ok(a) = s.get_address() {
                    json::write_ip_address(&mut addr, a);
                }
                rows.push(vec![
                    strip_quotes(&addr),
                    format!("{}", s.get_port().map(|p| p.get_number()).unwrap_or(0)),
                ]);
            }
        }
    }
    render::table(&["address", "port"], &rows)
}



fn json_proxy(bytes: &[u8], out: &mut String) {
    if let Ok(reader) = tp::decode_message::<wire::get_proxy_config_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            if let Ok(config) = results.get_config() {
                json::write_proxy_config(out, config);
                return;
            }
        }
    }
    out.push_str("null");
}
fn human_proxy(bytes: &[u8]) -> String {
    if let Ok(reader) = tp::decode_message::<wire::get_proxy_config_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            if let Ok(config) = results.get_config() {
                let mut s = String::new();
                s.push_str(&format!("enabled: {}\n", config.get_enabled()));
                s.push_str(&format!("url: {}\n", config.get_url().map(|u| u.to_str().unwrap_or("")).unwrap_or("")));
                return s;
            }
        }
    }
    String::new()
}

fn json_ping(bytes: &[u8], out: &mut String) {
    if let Ok(reader) = tp::decode_message::<wire::ping_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            let mut o = json::Obj::new(out);
            if let Ok(latency) = results.get_latency() {
                o.key("latencyMs").int(latency.get_nanoseconds() / 1_000_000);
            }
            o.key("packetLoss").float(f64::from(results.get_packet_loss()));
            o.end();
            return;
        }
    }
    out.push_str("null");
}
fn human_ping(bytes: &[u8]) -> String {
    if let Ok(reader) = tp::decode_message::<wire::ping_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            let ms = results.get_latency().map(|l| l.get_nanoseconds() / 1_000_000).unwrap_or(0);
            return format!("latency: {ms} ms\nloss: {}%\n", results.get_packet_loss() * 100.0);
        }
    }
    String::new()
}

fn json_traceroute(bytes: &[u8], out: &mut String) {
    out.push_str("{\"hops\":[");
    if let Ok(reader) = tp::decode_message::<wire::traceroute_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            let mut first = true;
            for h in results.get_hops().into_iter().flatten() {
                json::sep(out, first);
                first = false;
                json::write_traceroute_hop(out, h);
            }
        }
    }
    out.push_str("]}");
}
fn human_traceroute(bytes: &[u8]) -> String {
    let mut rows = Vec::new();
    if let Ok(reader) = tp::decode_message::<wire::traceroute_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            for h in results.get_hops().into_iter().flatten() {
                let mut addr = String::new();
                if let Ok(a) = h.get_address() {
                    json::write_ip_address(&mut addr, a);
                }
                rows.push(vec![
                    format!("{}", h.get_hop()),
                    strip_quotes(&addr),
                    h.get_hostname().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string(),
                ]);
            }
        }
    }
    render::table(&["hop", "address", "host"], &rows)
}

fn json_counters(bytes: &[u8], out: &mut String) {
    if let Ok(reader) = tp::decode_message::<wire::get_counters_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            if let Ok(c) = results.get_counters() {
                json::write_counters(out, c);
                return;
            }
        }
    }
    out.push_str("null");
}
fn human_counters(bytes: &[u8]) -> String {
    if let Ok(reader) = tp::decode_message::<wire::get_counters_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            if let Ok(c) = results.get_counters() {
                return render::counters(c);
            }
        }
    }
    String::new()
}

fn json_vpn_list(bytes: &[u8], out: &mut String) {
    out.push_str("{\"profiles\":[");
    if let Ok(reader) = tp::decode_message::<wire::list_vpn_profiles_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            let mut first = true;
            for p in results.get_profiles().into_iter().flatten() {
                json::sep(out, first);
                first = false;
                json::write_vpn_profile(out, p);
            }
        }
    }
    out.push_str("]}");
}
fn human_vpn_list(bytes: &[u8]) -> String {
    let mut rows = Vec::new();
    if let Ok(reader) = tp::decode_message::<wire::list_vpn_profiles_results::Owned>(bytes) {
        if let Ok(results) = reader.get() {
            for p in results.get_profiles().into_iter().flatten() {
                rows.push(vec![
                    p.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or("").to_string(),
                    p.get_kind().map(|k| k.to_str().unwrap_or("")).unwrap_or("").to_string(),
                ]);
            }
        }
    }
    render::table(&["profile", "kind"], &rows)
}



/// Strip the surrounding quotes a `write_ip_address`/`write_str` left.
fn strip_quotes(s: &str) -> String {
    s.trim_matches('"').to_string()
}
