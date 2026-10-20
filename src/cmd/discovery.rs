//! The `discovery` family over `Discovery` (`discovery.capnp`) — the
//! smart-protocol SERVICE registry peers (doc 04 §4). Lookup is ungated: a
//! caller can learn what exists before it holds authority.

use alloc::string::{String, ToString};

use crate::json;
use crate::schema::discovery_capnp::discovery as wire;
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
    let task = sub.first().map(String::as_str).unwrap_or("peers");
    let rest = &sub[1..];
    match task {
        "peers" | "scan" | "list" => {
            let params = msg(|m| {
                let mut p = m.init_root::<wire::lookup_params::Builder<'_>>();
                let _ = p.reborrow().init_filter();
            });
            go(app, "Discovery", "lookup", &params, Handler { json: discovery_peers, human: discovery_peers_human })
        }
        "resolve" => {
            let Some(name) = rest.first().cloned() else {
                super::usage_error_app(app, "usage: discovery resolve <name>");
                return crate::registry::EXIT_USAGE;
            };
            let params = msg(|m| { m.init_root::<wire::resolve_params::Builder<'_>>().set_name(&name); });
            go(app, "Discovery", "resolve", &params, Handler { json: discovery_resolve, human: discovery_resolve_human })
        }
        other => {
            super::usage_error_app(app, &format!("unknown discovery task '{other}' (peers|resolve)"));
            crate::registry::EXIT_USAGE
        }
    }
}

fn discovery_peers(bytes: &[u8], out: &mut String) {
    let peers = tp::with_root::<wire::lookup_results::Owned, _, _>(bytes, |r| {
        let mut s = String::from("[");
        let mut first = true;
        for svc in r.get_services().into_iter().flatten() {
            json::sep(&mut s, first);
            first = false;
            json::write_service_descriptor(&mut s, svc);
        }
        s.push(']');
        s
    })
    .unwrap_or_else(|| "[]".to_string());
    out.push_str("{\"peers\":");
    out.push_str(&peers);
    out.push('}');
}
fn discovery_peers_human(bytes: &[u8]) -> String {
    let rows = tp::with_root::<wire::lookup_results::Owned, _, _>(bytes, |r| {
        let mut rows = Vec::new();
        for svc in r.get_services().into_iter().flatten() {
            rows.push(vec![
                svc.get_name().map(|n| n.to_str().unwrap_or("")).ok().unwrap_or("").to_string(),
                svc.get_category().map(|c| c.to_str().unwrap_or("")).ok().unwrap_or("").to_string(),
            ]);
        }
        rows
    })
    .unwrap_or_default();
    crate::render::table(&["service", "category"], &rows)
}

fn discovery_resolve(bytes: &[u8], out: &mut String) {
    // Both contract fields: `present` says whether the name resolved, and the
    // descriptor is the answer itself (a key is never dropped — the JSON law
    // forbids added fields AND silently dropped ones).
    let (present, descriptor) = tp::with_root::<wire::resolve_results::Owned, _, _>(bytes, |r| {
        let present = r.get_present();
        let descriptor = match r.get_descriptor() {
            Ok(descriptor) => {
                let mut one = String::new();
                json::write_service_descriptor(&mut one, descriptor);
                Some(one)
            }
            Err(_) => None,
        };
        (present, descriptor)
    })
    .unwrap_or((false, None));
    out.push_str("{\"present\":");
    out.push_str(if present { "true" } else { "false" });
    match descriptor {
        Some(descriptor) => {
            out.push_str(",\"descriptor\":");
            out.push_str(&descriptor);
        }
        None => out.push_str(",\"descriptor\":null"),
    }
    out.push('}');
}
fn discovery_resolve_human(bytes: &[u8]) -> String {
    tp::with_root::<wire::resolve_results::Owned, _, _>(bytes, |r| {
        let present = r.get_present();
        let name = r
            .get_descriptor()
            .ok()
            .and_then(|d| d.get_name().ok())
            .and_then(|n| n.to_str().ok())
            .unwrap_or("")
            .to_string();
        format!("present: {present} service: {name}\n")
    })
    .unwrap_or_default()
}
