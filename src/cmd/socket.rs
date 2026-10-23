//! The `socket` family over `SocketService` (`socket.capnp`). Sockets are
//! created and addressed by an id the service mints; the CLI passes that id
//! back (a `SocketHandle` is a struct, not a capability) for follow-up calls.

use alloc::string::{String, ToString};

use crate::json;
use smart_protocol::schema::socket_capnp::socket_service as wire;
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
    let task = sub.first().map(String::as_str).unwrap_or("create");
    let rest = &sub[1..];
    match task {
        "create" => {
            let kind = rest.first().cloned().unwrap_or_else(|| "stream".to_string());
            let params = msg(|m| { m.init_root::<wire::create_params::Builder<'_>>().set_kind(&kind); });
            go(app, "SocketService", "create", &params, Handler { json: socket_handle, human: socket_handle_human })
        }
        "close" => {
            let Some(id) = rest.first().and_then(|s| s.parse::<u64>().ok()) else {
                super::usage_error_app(app, "usage: socket close <id>");
                return crate::registry::EXIT_USAGE;
            };
            let params = msg(|m| { m.init_root::<wire::close_params::Builder<'_>>().reborrow().init_handle().init_id().set_id(id); });
            go(app, "SocketService", "close", &params, super::VOID)
        }
        "name" => {
            let Some(id) = rest.first().and_then(|s| s.parse::<u64>().ok()) else {
                super::usage_error_app(app, "usage: socket name <id>");
                return crate::registry::EXIT_USAGE;
            };
            let params = msg(|m| { m.init_root::<wire::get_socket_name_params::Builder<'_>>().reborrow().init_handle().init_id().set_id(id); });
            go(app, "SocketService", "getSocketName", &params, Handler { json: socket_name, human: socket_name_human })
        }
        "info" => {
            let Some(id) = rest.first().and_then(|s| s.parse::<u64>().ok()) else {
                super::usage_error_app(app, "usage: socket info <id>");
                return crate::registry::EXIT_USAGE;
            };
            let params = msg(|m| { m.init_root::<wire::get_info_params::Builder<'_>>().reborrow().init_handle().init_id().set_id(id); });
            go(app, "SocketService", "getInfo", &params, Handler { json: socket_info, human: socket_info_human })
        }
        "list" | "where" => {
            super::usage_error_app(app, "socket list/where needs a per-socket enumeration the contract does not carry (socket-batch gap, doc 04 §4); use socket info <id>");
            crate::registry::EXIT_USAGE
        }
        other => {
            super::usage_error_app(app, &format!("unknown socket task '{other}' (create|info|name|close)"));
            crate::registry::EXIT_USAGE
        }
    }
}

fn socket_handle(bytes: &[u8], out: &mut String) {
    let handle_id = tp::with_root::<wire::create_results::Owned, _, _>(bytes, |r| {
        r.get_handle().map(|h| h.get_id().map(|i| i.get_id()).unwrap_or(0)).unwrap_or(0)
    })
    .unwrap_or(0);
    out.push_str("{\"socketId\":");
    out.push_str(&handle_id.to_string());
    out.push('}');
}
fn socket_handle_human(bytes: &[u8]) -> String {
    let handle_id = tp::with_root::<wire::create_results::Owned, _, _>(bytes, |r| {
        r.get_handle().map(|h| h.get_id().map(|i| i.get_id()).unwrap_or(0)).unwrap_or(0)
    })
    .unwrap_or(0);
    format!("socket {handle_id}\n")
}

fn socket_name(bytes: &[u8], out: &mut String) {
    let port = tp::with_root::<wire::get_socket_name_results::Owned, _, _>(bytes, |r| r.get_port()).unwrap_or(0);
    out.push_str("{\"port\":");
    out.push_str(&port.to_string());
    out.push('}');
}
fn socket_name_human(bytes: &[u8]) -> String {
    let port = tp::with_root::<wire::get_socket_name_results::Owned, _, _>(bytes, |r| r.get_port()).unwrap_or(0);
    format!("port: {port}\n")
}

fn socket_info(bytes: &[u8], out: &mut String) {
    tp::with_root::<wire::get_info_results::Owned, _, _>(bytes, |r| {
        if let Ok(info) = r.get_info() {
            let mut o = json::Obj::new(out);
            o.key("kind").str(info.get_kind().map(|k| k.to_str().unwrap_or("")).ok().unwrap_or(""));
            o.key("localPort").int(info.get_local_port());
            o.key("bytesSent").int(info.get_bytes_sent());
            o.key("bytesReceived").int(info.get_bytes_received());
            o.end();
        } else {
            out.push_str("null");
        }
    });
}
fn socket_info_human(bytes: &[u8]) -> String {
    tp::with_root::<wire::get_info_results::Owned, _, _>(bytes, |r| {
        if let Ok(info) = r.get_info() {
            format!(
                "kind: {}\nlocal port: {}\nsent: {} received: {}\n",
                info.get_kind().map(|k| k.to_str().unwrap_or("")).ok().unwrap_or(""),
                info.get_local_port(),
                info.get_bytes_sent(),
                info.get_bytes_received()
            )
        } else {
            String::new()
        }
    })
    .unwrap_or_default()
}

// Keep the schema import used for the (future) all-sockets gap note.
