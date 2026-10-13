// The AI-first headless scenario (`knowledge/05-AI-FIRST-SURFACE.md` §4): a
// caller that builds capnp messages DIRECTLY — no parser, no rendering, no
// shell vocabulary — drives the network surface and asserts each step. This
// file is the one source of truth shared by `examples/agent_client.rs` (which
// runs it as the L6 proof) and `tests/headless_gate.rs` (which asserts the
// same path links no `cmd` module).
//
// The scenario: READ (getInterfaces + isNetworkingEnabled), OBSERVE
// (subscribe → first event → pull ≥1 → cancel), DENIAL (setNetworking on a
// read-only capability → permissionDenied). No CLI code is involved.


use smart_cli::schema::network_capnp::network as wire;
use smart_cli::transport::{CallError, Connection, Target, with_root};

fn net_id() -> u64 {
    smart_cli::contract::schema_id("network").unwrap_or(0)
}

fn params<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut capnp::message::Builder<capnp::message::HeapAllocator>),
{
    let mut m = capnp::message::Builder::new_default();
    f(&mut m);
    smart_cli::transport::encode_message(&m)
}

/// Drive the headless scenario against a read-only service at `path`. `Ok(())`
/// only when every assertion holds; otherwise the first failed step is named.
pub fn run_against(path: &str) -> Result<(), String> {
    let mut conn = Connection::connect(&Target::Unix(path.to_string())).map_err(|e| format!("connect: {e:?}"))?;

    // ── READ: getInterfaces returns a contract-shaped page ──
    let ifaces = conn
        .call(net_id(), 0, &params(|m| { m.init_root::<wire::get_interfaces_params::Builder<'_>>(); }))
        .map_err(|e| format!("getInterfaces: {e:?}"))?;
    let iface_count =
        with_root::<wire::get_interfaces_results::Owned, _, _>(&ifaces, |r| r.get_interfaces().map(|l| l.len()).unwrap_or(0))
            .unwrap_or(0);
    if iface_count == 0 {
        return Err("getInterfaces returned an empty page".into());
    }

    // ── READ: isNetworkingEnabled returns the master switch ──
    let net = conn
        .call(net_id(), 73, &params(|m| { m.init_root::<wire::is_networking_enabled_params::Builder<'_>>(); }))
        .map_err(|e| format!("isNetworkingEnabled: {e:?}"))?;
    let enabled =
        with_root::<wire::is_networking_enabled_results::Owned, _, _>(&net, |r| r.get_enabled())
            .unwrap_or(false);
    if !enabled {
        return Err("isNetworkingEnabled returned false".into());
    }

    // ── OBSERVE: subscribe → first event → pull ≥1 → cancel ──
    let subscribe = conn
        .call(net_id(), 2, &params(|m| {
            let mut p = m.init_root::<wire::subscribe_status_params::Builder<'_>>();
            p.reborrow().init_interface_id().set_id(1);
        }))
        .map_err(|e| format!("subscribeStatus: {e:?}"))?;
    let (sub_id, first_event) = smart_cli::frame::split_sub_id(&subscribe)
        .ok_or("the subscribe answer carried no subscription id")?;
    if first_event.is_empty() {
        return Err("the subscribe answer carried no first event".into());
    }
    let pull = conn
        .pull_subscription(sub_id, 1)
        .map_err(|e| format!("pull: {e:?}"))?;
    if pull.is_empty() {
        return Err("the pull returned nothing (a real answer, but not an event)".into());
    }
    conn.cancel_subscription(sub_id).map_err(|e| format!("cancel: {e:?}"))?;

    // ── DENIAL: setNetworking on a read-only capability is permissionDenied ──
    match conn.call(net_id(), 72, &params(|m| {
        let mut p = m.init_root::<wire::set_networking_params::Builder<'_>>();
        p.set_enabled(true);
    })) {
        Err(CallError::Service(service_err)) if service_err.is_permission() => Ok(()),
        Err(CallError::Service(service_err)) => {
            Err(format!("setNetworking was not permissionDenied: {:?}", service_err.code))
        }
        Ok(_) => Err("setNetworking unexpectedly succeeded on a read-only capability".into()),
        Err(e) => Err(format!("setNetworking transport error: {e:?}")),
    }
}
