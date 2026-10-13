//! The `web` family over `WebService` (`web.capnp`): HTTP fetch and URL
//! resolution. `fetch` can FAIL — the result is a union carrying `T.Error`, so
//! "server answered 500" and "we never reached the server" are distinct (the
//! predecessor wire flattened them; this client reads the honest arm).

use alloc::string::{String, ToString};

use crate::json;
use crate::schema::web_capnp::{self as web, web_service as wire};
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
    let task = sub.first().map(String::as_str).unwrap_or("get");
    let rest = &sub[1..];
    match task {
        "get" | "post" | "put" | "delete" | "patch" => {
            let Some(url) = rest.first().cloned() else {
                super::usage_error_app(app, &format!("usage: web {task} <url>"));
                return crate::registry::EXIT_USAGE;
            };
            let method = match task {
                "post" => web::HttpMethod::Post,
                "put" => web::HttpMethod::Put,
                "delete" => web::HttpMethod::Delete,
                "patch" => web::HttpMethod::Patch,
                _ => web::HttpMethod::Get,
            };
            let params = msg(|m| {
                let mut p = m.init_root::<wire::fetch_params::Builder<'_>>();
                let mut req = p.reborrow().init_request();
                req.set_method(method);
                req.set_url(&url);
            });
            go(app, "Web", "fetch", 0, &params, Handler { json: web_fetch, human: web_fetch_human })
        }
        "resolve" => {
            let Some(url) = rest.first().cloned() else {
                super::usage_error_app(app, "usage: web resolve <url> [base]");
                return crate::registry::EXIT_USAGE;
            };
            let base = rest.get(1).cloned().unwrap_or_default();
            let params = msg(|m| {
                let mut p = m.init_root::<wire::resolve_url_params::Builder<'_>>();
                p.set_url(&url);
                p.set_base_url(&base);
            });
            go(app, "Web", "resolveUrl", 3, &params, Handler { json: web_resolve, human: web_resolve_human })
        }
        "download" | "ws" => {
            super::usage_error_app(app, "web download/ws are streaming capabilities; named, not half-routed this wave");
            crate::registry::EXIT_USAGE
        }
        other => {
            super::usage_error_app(app, &format!("unknown web task '{other}' (get|post|put|delete|patch|resolve)"));
            crate::registry::EXIT_USAGE
        }
    }
}

fn web_fetch(bytes: &[u8], out: &mut String) {
    let body = tp::with_root::<wire::fetch_results::Owned, _, _>(bytes, |r| {
        let s = match r.get_result().ok().and_then(|res| match res.which() {
            Ok(crate::schema::web_capnp::http_fetch_result::Which::Response(Ok(resp))) => {
                Some(resp.get_status())
            }
            _ => None,
        }) {
            Some(status) => {
                let mut s = String::new();
                let mut o = json::Obj::new(&mut s);
                o.key("status").int(status);
                o.end();
                s
            }
            None => String::from("null"),
        };
        s
    })
    .unwrap_or_else(|| "null".to_string());
    out.push_str("{\"response\":");
    out.push_str(&body);
    out.push('}');
}
fn web_fetch_human(bytes: &[u8]) -> String {
    let status = tp::with_root::<wire::fetch_results::Owned, _, _>(bytes, |r| {
        r.get_result().ok().and_then(|res| match res.which() {
            Ok(crate::schema::web_capnp::http_fetch_result::Which::Response(Ok(resp))) => Some(resp.get_status()),
            _ => None,
        })
    })
    .flatten();
    match status {
        Some(status) => format!("http status: {status}\n"),
        None => String::from("fetch: no response\n"),
    }
}

fn web_resolve(bytes: &[u8], out: &mut String) {
    let resolved = tp::with_root::<wire::resolve_url_results::Owned, _, _>(bytes, |r| {
        r.get_resolved().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default()
    })
    .unwrap_or_default();
    out.push_str("{\"resolved\":");
    json::write_str(out, &resolved);
    out.push('}');
}
fn web_resolve_human(bytes: &[u8]) -> String {
    let resolved = tp::with_root::<wire::resolve_url_results::Owned, _, _>(bytes, |r| {
        r.get_resolved().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default()
    })
    .unwrap_or_default();
    format!("{resolved}\n")
}
