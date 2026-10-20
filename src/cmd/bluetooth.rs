//! The `bluetooth` family over `BluetoothService` (`bluetooth.capnp`). A thin
//! client: `route -> call -> render/json`. The scan/pair surfaces are
//! subscription capabilities and are named in help rather than half-routed;
//! this build covers the direct state/read/control calls (doc 04 §4).

use alloc::string::{String, ToString};

use crate::json;
use crate::render;
use crate::schema::bluetooth_capnp::{self as bt, bluetooth_service as wire};
use crate::transport as tp;

use super::{go, App, Handler};

/// The BluetoothService schema id, used when a verb addresses the interface
/// directly (every routed call goes through [`super::go`], which resolves the
/// same id from the interface name).
#[allow(dead_code)]
fn id() -> u64 {
    crate::contract::schema_id("bluetooth").unwrap_or(0)
}

fn msg<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut capnp::message::Builder<capnp::message::HeapAllocator>),
{
    let mut message = capnp::message::Builder::new_default();
    f(&mut message);
    tp::encode_message(&message)
}

pub fn run(sub: &[String], app: &mut App) -> i32 {
    let task = sub.first().map(String::as_str).unwrap_or("status");
    let rest = &sub[1..];
    match task {
        "status" | "state" => go(
            app, "Bluetooth", "getState", 0, &msg(|m| { m.init_root::<wire::get_state_params::Builder<'_>>(); }),
            Handler { json: |b, o| bt_state(b, o), human: |b| bt_state_human(b) },
        ),
        "on" | "enable" => go(app, "BluetoothService", "enable", &msg(|m| { m.init_root::<wire::enable_params::Builder<'_>>(); }), super::VOID),
        "off" | "disable" => go(app, "BluetoothService", "disable", &msg(|m| { m.init_root::<wire::disable_params::Builder<'_>>(); }), super::VOID),
        "name" => match rest.first().map(String::as_str) {
            None => go(app, "BluetoothService", "getName", &msg(|m| { m.init_root::<wire::get_name_params::Builder<'_>>(); }), Handler { json: bt_name, human: bt_name_human }),
            Some(name) => {
                let params = msg(|m| { m.init_root::<wire::set_name_params::Builder<'_>>().set_name(name); });
                go(app, "BluetoothService", "setName", &params, super::VOID)
            }
        },
        "devices" | "paired" => go(app, "BluetoothService", "getPairedDevices", &bt_page(0, 256), Handler { json: bt_devices, human: bt_devices_human }),
        "scan" | "pair" | "connect" => {
            super::usage_error_app(app, "bluetooth scan/pair/connect are subscription capabilities; named, not half-routed this wave (the daemon mints the sub id)");
            crate::registry::EXIT_USAGE
        }
        other => {
            super::usage_error_app(app, &format!("unknown bluetooth task '{other}' (status|on|off|name|devices)"));
            crate::registry::EXIT_USAGE
        }
    }
}

fn bt_page(offset: u32, limit: u32) -> Vec<u8> {
    msg(|m| {
        let mut p = m.init_root::<wire::get_paired_devices_params::Builder<'_>>();
        let mut page = p.reborrow().init_pagination();
        page.set_offset(offset);
        page.set_limit(limit);
    })
}

fn state_token(s: bt::BluetoothState) -> &'static str {
    match s {
        bt::BluetoothState::Off => "off",
        bt::BluetoothState::TurningOn => "turningOn",
        bt::BluetoothState::On => "on",
        bt::BluetoothState::TurningOff => "turningOff",
        bt::BluetoothState::Custom => "custom",
    }
}

fn bt_state(bytes: &[u8], out: &mut String) {
    out.push_str("{\"state\":");
    let token = tp::with_root::<wire::get_state_results::Owned, _, _>(bytes, |r| {
        r.get_state().map(state_token).unwrap_or("unknown").to_string()
    })
    .unwrap_or_else(|| "unknown".to_string());
    json::write_str(out, &token);
    out.push('}');
}
fn bt_state_human(bytes: &[u8]) -> String {
    let token = tp::with_root::<wire::get_state_results::Owned, _, _>(bytes, |r| r.get_state().map(state_token).unwrap_or("unknown").to_string()).unwrap_or_default();
    format!("bluetooth: {token}\n")
}

fn bt_name(bytes: &[u8], out: &mut String) {
    out.push_str("{\"name\":");
    let name = tp::with_root::<wire::get_name_results::Owned, _, _>(bytes, |r| {
        r.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default()
    })
    .unwrap_or_default();
    json::write_str(out, &name);
    out.push('}');
}
fn bt_name_human(bytes: &[u8]) -> String {
    let name = tp::with_root::<wire::get_name_results::Owned, _, _>(bytes, |r| r.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default()).unwrap_or_default();
    format!("name: {name}\n")
}

fn bt_devices(bytes: &[u8], out: &mut String) {
    let devices = tp::with_root::<wire::get_paired_devices_results::Owned, _, _>(bytes, |r| {
        let mut s = String::from("[");
        let mut first = true;
        for d in r.get_devices().into_iter().flatten() {
            json::sep(&mut s, first);
            first = false;
            let mut o = json::Obj::new(&mut s);
            o.key("name").str(d.get_name().map(|n| n.to_str().unwrap_or("")).unwrap_or(""));
            o.key("paired").boolean(d.get_paired());
            o.key("connected").boolean(d.get_connected());
            o.key("trusted").boolean(d.get_trusted());
            o.end();
        }
        s.push(']');
        s
    })
    .unwrap_or_else(|| "[]".to_string());
    out.push_str("{\"devices\":");
    out.push_str(&devices);
    out.push('}');
}
fn bt_devices_human(bytes: &[u8]) -> String {
    let rows = tp::with_root::<wire::get_paired_devices_results::Owned, _, _>(bytes, |r| {
        let mut rows = Vec::new();
        for d in r.get_devices().into_iter().flatten() {
            rows.push(vec![
                d.get_name().map(|n| n.to_str().unwrap_or("").to_string()).unwrap_or_default(),
                if d.get_connected() { "connected" } else { "paired" }.to_string(),
            ]);
        }
        rows
    })
    .unwrap_or_default();
    render::table(&["device", "state"], &rows)
}
