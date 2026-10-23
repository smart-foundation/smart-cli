//! The `tunnel` family over `TunnelService` (`tunnel.capnp`) — the true
//! tunnels (a bidirectional async channel). WireGuard/OpenVPN *profiles* route
//! to the network VPN surface (`net vpn`, doc 04 §4); this module is the
//! `tunnel.capnp` half: open, info, close.

use alloc::string::{String, ToString};

use crate::json;
use smart_protocol::schema::tunnel_capnp::tunnel_service as wire;
use crate::transport as tp;

use super::{go, App, Handler};

fn msg<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut capnp::message::Builder<capnp::message::HeapAllocator>),
{
    let mut message = capnp::message::Builder::new_default();
    f(&mut message);
    tp::encode_message(&message)
}

pub fn run(sub: &[String], app: &mut App) -> i32 {
    let task = sub.first().map(String::as_str).unwrap_or("help");
    let rest = &sub[1..];
    match task {
        "open" => {
            let recipient = rest.first().cloned().unwrap_or_default();
            if recipient.is_empty() {
                super::usage_error_app(app, "usage: tunnel open <recipient> [--name X]");
                return crate::registry::EXIT_USAGE;
            }
            let name = flag_value(rest, "--name").unwrap_or_else(|| recipient.clone());
            let params = msg(|m| {
                let mut p = m.init_root::<wire::open_params::Builder<'_>>();
                let mut spec = p.reborrow().init_spec();
                spec.reborrow().set_name(&name);
                spec.set_max_frame_size(64 * 1024);
                p.reborrow().set_recipient(&recipient);
            });
            go(app, "TunnelService", "open", &params, Handler { json: tunnel_handle, human: tunnel_handle_human })
        }
        "close" => {
            let Some(id) = rest.first().and_then(|s| s.parse::<u64>().ok()) else {
                super::usage_error_app(app, "usage: tunnel close <id>");
                return crate::registry::EXIT_USAGE;
            };
            let params = msg(|m| { m.init_root::<wire::close_params::Builder<'_>>().reborrow().init_handle().init_id().set_id(id); });
            go(app, "TunnelService", "close", &params, super::VOID)
        }
        "info" => {
            let Some(id) = rest.first().and_then(|s| s.parse::<u64>().ok()) else {
                super::usage_error_app(app, "usage: tunnel info <id>");
                return crate::registry::EXIT_USAGE;
            };
            let params = msg(|m| { m.init_root::<wire::get_info_params::Builder<'_>>().reborrow().init_handle().init_id().set_id(id); });
            go(app, "TunnelService", "getInfo", &params, Handler { json: tunnel_info, human: tunnel_info_human })
        }
        _ => {
            super::usage_error_app(app, "usage: tunnel open <recipient> | tunnel info <id> | tunnel close <id>");
            crate::registry::EXIT_USAGE
        }
    }
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
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

fn tunnel_handle(bytes: &[u8], out: &mut String) {
    let id = tp::with_root::<wire::open_results::Owned, _, _>(bytes, |r| {
        r.get_handle().map(|h| h.get_id().map(|i| i.get_id()).unwrap_or(0)).unwrap_or(0)
    })
    .unwrap_or(0);
    out.push_str("{\"tunnelId\":");
    out.push_str(&id.to_string());
    out.push('}');
}
fn tunnel_handle_human(bytes: &[u8]) -> String {
    let id = tp::with_root::<wire::open_results::Owned, _, _>(bytes, |r| {
        r.get_handle().map(|h| h.get_id().map(|i| i.get_id()).unwrap_or(0)).unwrap_or(0)
    })
    .unwrap_or(0);
    format!("tunnel {id}\n")
}

fn tunnel_info(bytes: &[u8], out: &mut String) {
    out.push_str("{\"info\":");
    let body = tp::with_root::<wire::get_info_results::Owned, _, _>(bytes, |r| {
        match r.get_result().ok().and_then(|res| match res.which() {
            Ok(smart_protocol::schema::tunnel_capnp::tunnel_info_result::Which::Info(Ok(info))) => Some(info),
            _ => None,
        }) {
            Some(info) => {
                let mut s = String::new();
                let mut o = json::Obj::new(&mut s);
                o.key("peer").str(info.get_peer().map(|p| p.to_str().unwrap_or("")).ok().unwrap_or(""));
                o.key("state").str(info.get_state().map(|t| t.to_str().unwrap_or("")).ok().unwrap_or(""));
                o.key("framesIn").int(info.get_frames_in());
                o.key("framesOut").int(info.get_frames_out());
                o.end();
                s
            }
            None => String::from("null"),
        }
    })
    .unwrap_or_else(|| "null".to_string());
    out.push_str(&body);
    out.push('}');
}
fn tunnel_info_human(bytes: &[u8]) -> String {
    tp::with_root::<wire::get_info_results::Owned, _, _>(bytes, |r| {
        match r.get_result().ok().and_then(|res| match res.which() {
            Ok(smart_protocol::schema::tunnel_capnp::tunnel_info_result::Which::Info(Ok(info))) => Some(info),
            _ => None,
        }) {
            Some(info) => format!(
                "peer: {}\nstate: {}\nframes: {} in / {} out\n",
                info.get_peer().map(|p| p.to_str().unwrap_or("")).ok().unwrap_or(""),
                info.get_state().map(|t| t.to_str().unwrap_or("")).ok().unwrap_or(""),
                info.get_frames_in(),
                info.get_frames_out()
            ),
            None => String::new(),
        }
    })
    .unwrap_or_default()
}
