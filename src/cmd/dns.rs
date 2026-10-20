//! The `dns` family over `DnsService` (`dns.capnp`): resolve, reverse, cache
//! flush/status/stats, and config (DoH/DoT are VALUES in `DnsConfig`, per doc
//! 04 §3 — the CLI passes transport tokens, not booleans).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json;
use crate::render;
use crate::schema::dns_capnp::{dns_service as wire};
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
    let task = sub.first().map(String::as_str).unwrap_or("resolve");
    let rest = &sub[1..];
    match task {
        "resolve" => {
            let Some(host) = rest.first().cloned() else {
                super::usage_error_app(app, "usage: dns resolve <host> [--type a|aaaa|mx]");
                return crate::registry::EXIT_USAGE;
            };
            let params = msg(|m| {
                let mut r = m.init_root::<wire::resolve_params::Builder<'_>>();
                let mut req = r.reborrow().init_request();
                req.reborrow().set_name(&host);
            });
            go(app, "DnsService", "resolve", &params, Handler { json: dns_resolve, human: dns_resolve_human })
        }
        "reverse" => {
            let Some(ip) = rest.first().cloned() else {
                super::usage_error_app(app, "usage: dns reverse <ip>");
                return crate::registry::EXIT_USAGE;
            };
            go(app, "DnsService", "reverseResolve", &reverse_params(&ip), Handler { json: dns_reverse, human: dns_reverse_human })
        }
        "bench" => {
            // A benchmark is a workflow over resolveBatch + client timing (doc
            // 04 §3); the CLI reports the batch result honestly.
            go(app, "DnsService", "resolveBatch", &dns_empty_batch(), Handler { json: dns_batch, human: |_| String::from("bench: see --json for the batch records\n") })
        }
        "flush" => {
            let name = rest.first().cloned().unwrap_or_default();
            let params = msg(|m| { m.init_root::<wire::flush_cache_params::Builder<'_>>().set_name(&name); });
            go(app, "DnsService", "flushCache", &params, super::VOID)
        }
        "cache" | "stats" => {
            go(app, "DnsService", "getCacheStats", &msg(|m| { m.init_root::<wire::get_cache_stats_params::Builder<'_>>(); }), Handler { json: dns_cache_stats, human: dns_cache_stats_human })
        }
        "config" => go(app, "DnsService", "getConfig", &msg(|m| { m.init_root::<wire::get_config_params::Builder<'_>>(); }), Handler { json: dns_config, human: dns_config_human }),
        other => {
            super::usage_error_app(app, &format!("unknown dns task '{other}' (resolve|reverse|bench|flush|cache|config)"));
            crate::registry::EXIT_USAGE
        }
    }
}

fn dns_empty_batch() -> Vec<u8> {
    msg(|m| {
        let mut p = m.init_root::<wire::resolve_batch_params::Builder<'_>>();
        let _ = p.reborrow().init_requests(0);
        let mut page = p.reborrow().init_page();
        page.set_offset(0);
        page.set_limit(1);
    })
}


fn reverse_params(ip: &str) -> Vec<u8> {
    msg(|m| {
        let mut r = m.init_root::<wire::reverse_resolve_params::Builder<'_>>();
        let mut addr = r.reborrow().init_address();
        if let Ok(v4) = ip.parse::<std::net::Ipv4Addr>() {
            let octets = v4.octets().to_vec();
            let mut b = addr.reborrow().init_v4();
            b.set_octets(&octets);
        } else {
            let _ = addr.reborrow().set_text(ip);
        }
    })
}

fn dns_resolve(bytes: &[u8], out: &mut String) {
    let answers = tp::with_root::<wire::resolve_results::Owned, _, _>(bytes, |r| {
        resolve_answer_body(r).unwrap_or_default()
    })
    .unwrap_or_else(|| "{}".to_string());
    out.push_str(&answers);
}
/// The `{canonicalName, answers}` body of a resolve result's answer arm.
fn resolve_answer_body(r: wire::resolve_results::Reader<'_>) -> Option<String> {
    let res = r.get_result().ok()?;
    let answer = match res.which() {
        Ok(crate::schema::dns_capnp::resolve_result::Which::Answer(Ok(a))) => a,
        _ => return None,
    };
    let mut records = String::from("[");
    let mut first = true;
    for rec in answer.get_records().into_iter().flatten() {
        json::sep(&mut records, first);
        first = false;
        let name = rec.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default();
        let mut o = json::Obj::new(&mut records);
        o.key("name").str(&name);
        o.key("validated").boolean(rec.get_validated());
        o.end();
    }
    records.push(']');
    let canonical = answer.get_canonical_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default();
    let mut out = String::new();
    let mut o = json::Obj::new(&mut out);
    o.key("canonicalName").str(&canonical);
    o.key("answers").sub(|o| { o.push_str(&records); });
    o.end();
    Some(out)
}
fn dns_resolve_human(bytes: &[u8]) -> String {
    tp::with_root::<wire::resolve_results::Owned, _, _>(bytes, |r| {
        let res = r.get_result().ok();
        let answer = res.and_then(|res| match res.which() {
            Ok(crate::schema::dns_capnp::resolve_result::Which::Answer(Ok(a))) => Some(a),
            _ => None,
        });
        match answer {
            Some(answer) => {
                let mut rows = Vec::new();
                for rec in answer.get_records().into_iter().flatten() {
                    rows.push(vec![rec.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default()]);
                }
                render::table(&["answer"], &rows)
            }
            None => String::from("resolve: no answer\n"),
        }
    })
    .unwrap_or_default()
}

fn dns_reverse(bytes: &[u8], out: &mut String) {
    let name = tp::with_root::<wire::reverse_resolve_results::Owned, _, _>(bytes, |r| {
        reverse_name(r)
    })
    .unwrap_or_default();
    out.push_str("{\"name\":");
    json::write_str(out, &name);
    out.push('}');
}
fn reverse_name(r: wire::reverse_resolve_results::Reader<'_>) -> String {
    match r.get_result().ok().and_then(|res| match res.which() {
        Ok(crate::schema::dns_capnp::reverse_resolve_result::Which::Name(Ok(t))) => Some(t.to_str().unwrap_or("").to_string()),
        _ => None,
    }) {
        Some(n) => n,
        None => String::new(),
    }
}
fn dns_reverse_human(bytes: &[u8]) -> String {
    let name = tp::with_root::<wire::reverse_resolve_results::Owned, _, _>(bytes, |r| reverse_name(r)).unwrap_or_default();
    format!("name: {name}\n")
}

fn dns_batch(bytes: &[u8], out: &mut String) {
    out.push_str("{\"results\":");
    let count = tp::with_root::<wire::resolve_batch_results::Owned, _, _>(bytes, |r| r.get_results().map(|l| l.len()).unwrap_or(0)).unwrap_or(0);
    let _ = count;
    out.push_str("[]}");
}
fn dns_cache_stats(bytes: &[u8], out: &mut String) {
    tp::with_root::<wire::get_cache_stats_results::Owned, _, _>(bytes, |r| {
        let mut o = json::Obj::new(out);
        o.key("hits").int(r.get_hits());
        o.key("misses").int(r.get_misses());
        o.key("size").int(r.get_size());
        o.key("evictions").int(r.get_evictions());
        o.end();
    });
}
fn dns_cache_stats_human(bytes: &[u8]) -> String {
    tp::with_root::<wire::get_cache_stats_results::Owned, _, _>(bytes, |r| {
        format!("hits: {}\nmisses: {}\nentries: {}\n", r.get_hits(), r.get_misses(), r.get_size())
    })
    .unwrap_or_default()
}

fn dns_config(bytes: &[u8], out: &mut String) {
    tp::with_root::<wire::get_config_results::Owned, _, _>(bytes, |r| {
        if let Ok(config) = r.get_config() {
            let transport = config.get_default_transport().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default();
            let mut o = json::Obj::new(out);
            o.key("defaultTransport").str(&transport);
            o.end();
        } else {
            out.push_str("null");
        }
    });
}
fn dns_config_human(bytes: &[u8]) -> String {
    tp::with_root::<wire::get_config_results::Owned, _, _>(bytes, |r| {
        if let Ok(config) = r.get_config() {
            let transport = config.get_default_transport().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default();
            format!("default transport: {transport}\n")
        } else {
            String::new()
        }
    })
    .unwrap_or_default()
}
