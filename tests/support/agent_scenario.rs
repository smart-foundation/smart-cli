// The AI-first headless scenario (`knowledge/05-AI-FIRST-SURFACE.md` §4): a
// caller that builds capnp messages DIRECTLY — no parser, no rendering, no
// shell vocabulary — drives the network surface and asserts each step. This
// file is the one source of truth shared by `examples/agent_client.rs` (which
// runs it as the L6 proof) and `tests/headless_gate.rs` (which asserts the
// same path links no `cmd` module).
//
// The scenario: READ (getInterfaces + isNetworkingEnabled), OBSERVE
// (subscribe → pull ≥1 → cancel), DENIAL (setNetworking on a read-only
// capability → permissionDenied). No CLI code is involved.
//
// The call model is the CONTRACT'S OWN: the interface id and the method id
// come from the generated tables (`smart_protocol::contract`), the subscription is
// the contract's own capability, and the transport speaks capnp RPC.

use smart_protocol::schema::network_capnp::network as wire;
use smart_cli::transport::{CallError, Connection, Endpoint, Subscription, with_root};

/// The interface id and the method id of one network method — both GENERATED.
fn ids(method: &str) -> (u64, u16) {
    let info = smart_protocol::contract::interface_named("Network")
        .expect("the network interface is compiled into this build");
    let method_id = smart_protocol::contract::method_id(info.stem, info.name, method)
        .expect("the network interface declares that method");
    (info.type_id, method_id)
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
    let endpoint = Endpoint::new("unix", path);
    let mut conn = Connection::connect(&endpoint).map_err(|e| format!("connect: {e:?}"))?;

    // ── READ: getInterfaces returns a contract-shaped page ──
    let (net_id, get_interfaces) = ids("getInterfaces");
    let ifaces = conn
        .call(
            net_id,
            get_interfaces,
            &params(|m| {
                m.init_root::<wire::get_interfaces_params::Builder<'_>>();
            }),
        )
        .map_err(|e| format!("getInterfaces: {e:?}"))?;
    let iface_count = with_root::<wire::get_interfaces_results::Owned, _, _>(&ifaces, |r| {
        r.get_interfaces().map(|l| l.len()).unwrap_or(0)
    })
    .unwrap_or(0);
    if iface_count == 0 {
        return Err("getInterfaces returned an empty page".into());
    }

    // ── READ: isNetworkingEnabled returns the master switch ──
    let (_, is_networking) = ids("isNetworkingEnabled");
    let net = conn
        .call(
            net_id,
            is_networking,
            &params(|m| {
                m.init_root::<wire::is_networking_enabled_params::Builder<'_>>();
            }),
        )
        .map_err(|e| format!("isNetworkingEnabled: {e:?}"))?;
    let enabled =
        with_root::<wire::is_networking_enabled_results::Owned, _, _>(&net, |r| r.get_enabled())
            .unwrap_or(false);
    if !enabled {
        return Err("isNetworkingEnabled returned false".into());
    }

    // ── OBSERVE: subscribe → pull ≥1 → cancel ──
    // The subscribe is TYPED: the contract's answer carries the subscription
    // capability, and a capability does not survive serialization.
    let (_, subscribe_status) = ids("subscribeStatus");
    let subscribe_bytes = params(|m| {
        let mut p = m.init_root::<wire::subscribe_status_params::Builder<'_>>();
        p.reborrow().init_interface_id().set_id(1);
    });
    let reader =
        smart_cli::transport::decode_message::<wire::subscribe_status_params::Owned>(&subscribe_bytes)
            .map_err(|e| format!("subscribeStatus: {e}"))?;
    let params_reader = reader.get().map_err(|e| format!("subscribeStatus: {e}"))?;
    let response = conn
        .call_typed::<
            wire::subscribe_status_params::Owned,
            wire::subscribe_status_results::Owned,
        >(net_id, subscribe_status, params_reader)
        .map_err(|e| format!("subscribeStatus: {e:?}"))?;
    let results = response
        .get()
        .map_err(|e| format!("subscribeStatus: {e}"))?;
    let capability = results
        .get_subscription()
        .map_err(|e| format!("subscribeStatus: {e}"))?;
    let subscription = Subscription::new(capability.client, "network", "StatusSubscription");
    let pull = subscription
        .pull(&mut conn)
        .map_err(|e| format!("pull: {e:?}"))?;
    if pull.is_empty() {
        return Err("the pull returned nothing (a real answer, but not an event)".into());
    }
    subscription
        .cancel(&mut conn)
        .map_err(|e| format!("cancel: {e:?}"))?;

    // ── DENIAL: setNetworking on a read-only capability is permissionDenied ──
    let (_, set_networking) = ids("setNetworking");
    match conn.call(
        net_id,
        set_networking,
        &params(|m| {
            let mut p = m.init_root::<wire::set_networking_params::Builder<'_>>();
            p.set_enabled(true);
        }),
    ) {
        Err(CallError::Service(service_err)) if service_err.is_permission() => Ok(()),
        Err(CallError::Service(service_err)) => Err(format!(
            "setNetworking was not permissionDenied: {:?}",
            service_err.code
        )),
        Ok(_) => Err("setNetworking unexpectedly succeeded on a read-only capability".into()),
        Err(e) => Err(format!("setNetworking transport error: {e:?}")),
    }
}
