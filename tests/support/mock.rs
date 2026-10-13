// A mock service: a test double that SPEAKS THE FRAME PROTOCOL the CLI's
// transport does, so every CLI test runs against it with no daemon and no
// live host (charter law #8 — mock only). It implements the SERVER side of
// the independently-implemented wire: it parses request frames and encodes
// response frames with the same fixed-offset layout, proving both directions
// agree. Headless (`examples/agent_client.rs`) includes this file, so the
// headless proof and the CLI golden tests share ONE double.
//
// The mock is honest like the real service: a read-only-class connection is
// refused `permissionDenied` before any backend, and unavailable commands
// answer `notSupported` with a reason — never a fabricated success.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

#[allow(unused_imports)]
use smart_cli::frame::{self, SUB_CANCEL, SUB_PULL, STATUS_ERROR, STATUS_OK};
use smart_cli::schema::{abi_capnp, types_capnp};
#[cfg(feature = "schema-network")]
use smart_cli::schema::network_capnp;
use smart_cli::transport::encode_message;

const DATA: u8 = 0x01;

/// The subscription id the mock mints for a status subscription.
const MOCK_SUB_ID: u64 = 0x5B5B_0001;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// One mock service's mutable state: the frame count it has served (the
/// seam-count measurement) and the rule/route sets its mutators wrote, so a
/// read after a write across separate CLI processes sees the write (a real
/// service holds state; so does this double).
pub struct State {
    requests: AtomicUsize,
    firewall: Mutex<Option<Vec<u8>>>,
    routes: Mutex<Option<Vec<u8>>>,
}

impl State {
    fn new() -> Self {
        State {
            requests: AtomicUsize::new(0),
            firewall: Mutex::new(None),
            routes: Mutex::new(None),
        }
    }
    /// The serve frames this service has answered (excluding the handshake).
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// Read the two bounded list sets a mutator wrote, if any.
fn written_set(slot: &Mutex<Option<Vec<u8>>>) -> Option<Vec<u8>> {
    slot.lock().unwrap().clone()
}

/// A mock service bound to a unix socket, serving on a background thread.
pub struct MockService {
    path: std::path::PathBuf,
    state: Arc<State>,
    /// Whether control (mutating) verbs are permitted (the capability class).
    pub allow_control: bool,
    /// Commands the registry reports unavailable (empty by default).
    pub unavailable: Vec<String>,
}

struct Cfg {
    control: bool,
    unavailable: Vec<String>,
    /// Answer the master-switch read with the contract's generic failure, so
    /// the exit-1 path (the call failed) is exercised against a real service
    /// double instead of a fabricated one.
    fail_reads: bool,
    /// The shared state (frame counts + written sets).
    state: Arc<State>,
}

impl MockService {
    /// Start a mock; `control` selects the capability class it serves at.
    pub fn start(control: bool) -> std::io::Result<Self> {
        MockService::start_with(control, Vec::new())
    }

    /// The serve frames this service has answered (the seam-count measurement).
    pub fn requests(&self) -> usize {
        self.state.requests()
    }

    /// Start a mock whose reads answer the contract's generic failure.
    pub fn start_failing_reads() -> std::io::Result<Self> {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("smart-cli-mock-{}-{unique}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let state = Arc::new(State::new());
        let cfg = Cfg {
            control: true,
            unavailable: Vec::new(),
            fail_reads: true,
            state: Arc::clone(&state),
        };
        thread::spawn(move || accept_loop(listener, cfg));
        Ok(MockService {
            path,
            state,
            allow_control: true,
            unavailable: Vec::new(),
        })
    }

    /// Start a mock whose registry reports the given commands unavailable.
    pub fn start_with(control: bool, unavailable: Vec<String>) -> std::io::Result<Self> {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("smart-cli-mock-{}-{unique}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let state = Arc::new(State::new());
        let cfg = Cfg {
            control,
            unavailable,
            fail_reads: false,
            state: Arc::clone(&state),
        };
        thread::spawn(move || accept_loop(listener, cfg));
        Ok(MockService {
            path,
            state,
            allow_control: control,
            unavailable: Vec::new(),
        })
    }

    /// The socket path to point the CLI at (`--connect <path>`).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for MockService {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn accept_loop(listener: UnixListener, cfg: Cfg) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cfg = Cfg {
                    control: cfg.control,
                    unavailable: cfg.unavailable.clone(),
                    fail_reads: cfg.fail_reads,
                    state: Arc::clone(&cfg.state),
                };
                thread::spawn(move || {
                    let _ = serve_conn(stream, &cfg);
                });
            }
            Err(_) => break,
        }
    }
}

/// Serve one connection: the ABI handshake, then the frame dispatch loop.
fn serve_conn(mut stream: UnixStream, cfg: &Cfg) -> std::io::Result<()> {
    // 1. the ABI handshake (the first frame is an abi_handshake_request).
    let (_, _abi_request) = read_frame(&mut stream)?;
    let response = matched_handshake();
    write_frame(&mut stream, DATA, &response)?;

    // 2. the dispatch loop.
    let mut has_sub = false;
    loop {
        let (kind, payload) = match read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(_) => break,
        };
        if kind != DATA {
            continue;
        }
        let Some(serve) = frame::segment0(&payload) else {
            continue;
        };
        let request = match frame::parse_request(serve) {
            Ok(request) => request,
            Err(_) => continue,
        };
        cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        let body = dispatch(request, cfg, &mut has_sub);
        write_frame(&mut stream, DATA, &frame::wrap_one_segment(&body))?;
    }
    Ok(())
}

/// Dispatch one request to its response frame (the serve-layer body, before
/// the one-segment wrap). Total: every ordinal gets exactly one answer.
#[cfg_attr(not(feature = "schema-network"), allow(unused_variables))]
fn dispatch(request: frame::RequestView<'_>, cfg: &Cfg, has_sub: &mut bool) -> Vec<u8> {
    let ordinal = request.ordinal;
    // The cli registry interface (schema id differs) answers listUnavailable.
    if request.interface_id == cli_schema_id() {
        return registry_answer(request, cfg);
    }
    // Sibling families: their own schema id, so the same ordinal numbers mean
    // different methods per interface. Compiled only when a test links them.
    #[cfg(feature = "schema-dns")]
    if request.interface_id == smart_cli::contract::schema_id("dns").unwrap_or(0) {
        return dns_answer(ordinal);
    }
    #[cfg(feature = "schema-socket")]
    if request.interface_id == smart_cli::contract::schema_id("socket").unwrap_or(0) {
        return socket_answer(ordinal);
    }
    #[cfg(feature = "schema-bluetooth")]
    if request.interface_id == smart_cli::contract::schema_id("bluetooth").unwrap_or(0) {
        return bluetooth_answer(ordinal, cfg);
    }
    #[cfg(feature = "schema-tunnel")]
    if request.interface_id == smart_cli::contract::schema_id("tunnel").unwrap_or(0) {
        return tunnel_answer(ordinal, cfg);
    }
    #[cfg(feature = "schema-web")]
    if request.interface_id == smart_cli::contract::schema_id("web").unwrap_or(0) {
        return web_answer(ordinal);
    }
    #[cfg(feature = "schema-discovery")]
    if request.interface_id == smart_cli::contract::schema_id("discovery").unwrap_or(0) {
        return discovery_answer(ordinal);
    }
    #[cfg(feature = "schema-network")]
    if let Some(body) = network_dispatch(ordinal, request, cfg, has_sub) {
        return body;
    }
    // An ordinal no compiled family answers is honestly `notSupported`.
    not_supported(&format!("ordinal @{ordinal}"))
}

/// The network family's ordinal table (compiled only when its schema is).
#[cfg(feature = "schema-network")]
fn network_dispatch(
    ordinal: u16,
    request: frame::RequestView<'_>,
    cfg: &Cfg,
    has_sub: &mut bool,
) -> Option<Vec<u8>> {
    match ordinal {
        // ── reads ──
        0 => Some(ok(encode_message(&interfaces_msg()))),
        1 => Some(ok(encode_message(&status_msg()))),
        6 => Some(ok(encode_message(&wifi_scan_msg()))),
        35 => Some(ok(encode_message(&counters_msg()))),
        54 => Some(ok(encode_message(&ping_msg()))),
        73 => Some(if cfg.fail_reads {
            failed("the service could not read the networking state")
        } else {
            ok(encode_message(&is_networking_msg(true)))
        }),
        75 => Some(ok(encode_message(&radio_states_msg()))),
        78 => Some(ok(encode_message(&known_wifi_msg()))),
        79 => Some(match written_set(&cfg.state.routes) {
            Some(bytes) => ok(bytes),
            None => ok(encode_message(&routes_msg())),
        }),
        81 => Some(ok(encode_message(&proxy_msg()))),
        31 => Some(ok(encode_message(&dns_servers_msg()))),
        57 => Some(match written_set(&cfg.state.firewall) {
            Some(bytes) => ok(bytes),
            None => ok(encode_message(&firewall_rules_msg())),
        }),
        17 => Some(ok(encode_message(&vpn_list_msg()))),
        // ── subscriptions ──
        2 => {
            *has_sub = true;
            Some(frame::encode_response(
                STATUS_OK,
                "",
                &frame::payload_with_sub_id(MOCK_SUB_ID, &encode_message(&status_msg())),
            ))
        }
        37 => {
            *has_sub = true;
            Some(frame::encode_response(
                STATUS_OK,
                "",
                &frame::payload_with_sub_id(MOCK_SUB_ID, &encode_message(&counters_msg())),
            ))
        }
        SUB_PULL => {
            // The mock serves the counters stream (watch net bandwidth is the
            // only pull driver); a status subscription's first event already
            // rode its subscribe answer.
            Some(frame::encode_response(
                STATUS_OK,
                "",
                &frame::payload_with_sub_id(MOCK_SUB_ID, &encode_message(&counters_msg())),
            ))
        }
        SUB_CANCEL => {
            *has_sub = false;
            Some(ok(encode_message(&void_ok_msg())))
        }
        // ── mutators: gated by the connection class ──
        72 | 74 | 32 | 76 | 77 | 7 | 42 | 43 | 82 => Some(if cfg.control {
            ok(encode_message(&void_ok_msg()))
        } else {
            denied("a control verb")
        }),
        // The full-set mutators persist what they were handed, so the honest
        // read→write→read composer is testable across processes (this double
        // holds state the way a service does).
        56 | 80 => {
            if !cfg.control {
                return Some(denied("a control verb"));
            }
            let written = if ordinal == 56 {
                rules_results_from_params(&request)
            } else {
                routes_results_from_params(&request)
            };
            Some(match written {
                Some(bytes) => {
                    let slot = if ordinal == 56 { &cfg.state.firewall } else { &cfg.state.routes };
                    *slot.lock().unwrap() = Some(bytes);
                    ok(encode_message(&void_ok_msg()))
                }
                None => failed("the written set could not be decoded"),
            })
        }
        _ => None,
    }
}

// ── sibling-family answers (compiled only when the test links them) ────────

/// The bluetooth family: `BluetoothService` state/name/devices + the void
/// control verbs (the capability class decides them, as everywhere).
#[cfg(feature = "schema-bluetooth")]
fn bluetooth_answer(ordinal: u16, cfg: &Cfg) -> Vec<u8> {
    #[allow(unused_imports)]
    use smart_cli::schema::bluetooth_capnp::bluetooth_service as w;
    match ordinal {
        0 => ok(encode_message(&bluetooth_state_msg())),
        4 => ok(encode_message(&bluetooth_name_msg())),
        8 => ok(encode_message(&bluetooth_devices_msg())),
        1 | 2 | 3 => Some(if cfg.control {
            ok(encode_message(&void_ok_msg()))
        } else {
            denied("a control verb")
        })
        .unwrap(),
        other => not_supported(&format!("bluetooth ordinal @{other}")),
    }
}

#[cfg(feature = "schema-bluetooth")]
fn bluetooth_state_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::bluetooth_capnp::bluetooth_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_state_results::Builder<'_>>();
        r.reborrow().set_state(smart_cli::schema::bluetooth_capnp::BluetoothState::On);
    }
    m
}

#[cfg(feature = "schema-bluetooth")]
fn bluetooth_name_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::bluetooth_capnp::bluetooth_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_name_results::Builder<'_>>();
        r.reborrow().set_name("mock-host");
    }
    m
}

#[cfg(feature = "schema-bluetooth")]
fn bluetooth_devices_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::bluetooth_capnp::bluetooth_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_paired_devices_results::Builder<'_>>();
        let mut list = r.reborrow().init_devices(1);
        let mut d = list.reborrow().get(0);
        d.reborrow().set_name("Mock Buds");
        r.set_total_count(1);
    }
    m
}

/// The tunnel family: `TunnelService` open/info/close.
#[cfg(feature = "schema-tunnel")]
fn tunnel_answer(ordinal: u16, cfg: &Cfg) -> Vec<u8> {
    #[allow(unused_imports)]
    use smart_cli::schema::tunnel_capnp::tunnel_service as w;
    match ordinal {
        0 => ok(encode_message(&tunnel_open_msg())),
        10 => ok(encode_message(&tunnel_info_msg())),
        6 => Some(if cfg.control {
            ok(encode_message(&void_ok_msg()))
        } else {
            denied("a control verb")
        })
        .unwrap(),
        other => not_supported(&format!("tunnel ordinal @{other}")),
    }
}

#[cfg(feature = "schema-tunnel")]
fn tunnel_open_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::tunnel_capnp::tunnel_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::open_results::Builder<'_>>();
        let mut handle = r.reborrow().init_handle();
        handle.reborrow().init_id().set_id(11);
    }
    m
}

#[cfg(feature = "schema-tunnel")]
fn tunnel_info_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::tunnel_capnp::tunnel_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_info_results::Builder<'_>>();
        let mut result = r.reborrow().init_result();
        let mut info = result.reborrow().init_info();
        info.set_peer("mock-peer");
        info.reborrow().set_state("open");
    }
    m
}

/// The web family: `WebService` fetch and resolveUrl.
#[cfg(feature = "schema-web")]
fn web_answer(ordinal: u16) -> Vec<u8> {
    #[allow(unused_imports)]
    use smart_cli::schema::web_capnp::web_service as w;
    match ordinal {
        0 => ok(encode_message(&web_fetch_msg())),
        3 => ok(encode_message(&web_resolve_msg())),
        other => not_supported(&format!("web ordinal @{other}")),
    }
}

#[cfg(feature = "schema-web")]
fn web_fetch_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::web_capnp::web_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::fetch_results::Builder<'_>>();
        let mut result = r.reborrow().init_result();
        let mut response = result.reborrow().init_response();
        response.set_status(200);
    }
    m
}

#[cfg(feature = "schema-web")]
fn web_resolve_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::web_capnp::web_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::resolve_url_results::Builder<'_>>();
        r.reborrow().set_resolved("https://example.com/a");
    }
    m
}

/// The discovery family: `Discovery` lookup / resolve (ungated by design).
#[cfg(feature = "schema-discovery")]
fn discovery_answer(ordinal: u16) -> Vec<u8> {
    #[allow(unused_imports)]
    use smart_cli::schema::discovery_capnp::discovery as w;
    match ordinal {
        0 => ok(encode_message(&discovery_lookup_msg())),
        2 => ok(encode_message(&discovery_resolve_msg())),
        other => not_supported(&format!("discovery ordinal @{other}")),
    }
}

#[cfg(feature = "schema-discovery")]
fn discovery_lookup_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::discovery_capnp::discovery as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::lookup_results::Builder<'_>>();
        let mut list = r.reborrow().init_services(1);
        let mut svc = list.reborrow().get(0);
        svc.reborrow().set_name("mock-service");
        r.set_total(1);
    }
    m
}

#[cfg(feature = "schema-discovery")]
fn discovery_resolve_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::discovery_capnp::discovery as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::resolve_results::Builder<'_>>();
        let mut descriptor = r.reborrow().init_descriptor();
        descriptor.reborrow().set_name("mock-service");
        r.set_present(true);
    }
    m
}

/// The dns family: `DnsService` resolve/reverse/batch/cache/config.
#[cfg(feature = "schema-dns")]
fn dns_answer(ordinal: u16) -> Vec<u8> {
    #[allow(unused_imports)]
    use smart_cli::schema::dns_capnp::dns_service as w;
    match ordinal {
        0 => ok(encode_message(&dns_resolve_msg())),
        2 => ok(encode_message(&dns_reverse_msg())),
        33 => ok(encode_message(&dns_batch_msg())),
        20 => ok(encode_message(&dns_cache_stats_msg())),
        5 => ok(encode_message(&dns_config_msg())),
        3 => ok(encode_message(&void_ok_msg())),
        other => not_supported(&format!("dns ordinal @{other}")),
    }
}

#[cfg(feature = "schema-dns")]
fn dns_resolve_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::dns_capnp::dns_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::resolve_results::Builder<'_>>();
        let mut result = r.reborrow().init_result();
        let mut answer = result.reborrow().init_answer();
        answer.reborrow().set_canonical_name("example.com");
        let mut records = answer.reborrow().init_records(1);
        let mut record = records.reborrow().get(0);
        record.reborrow().set_name("example.com");
        record.set_validated(true);
    }
    m
}

#[cfg(feature = "schema-dns")]
fn dns_reverse_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::dns_capnp::dns_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::reverse_resolve_results::Builder<'_>>();
        let mut result = r.reborrow().init_result();
        result.reborrow().set_name("one.one.one.one");
    }
    m
}

#[cfg(feature = "schema-dns")]
fn dns_batch_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::dns_capnp::dns_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::resolve_batch_results::Builder<'_>>();
        let _ = r.reborrow().init_results(0);
        r.set_total_count(0);
    }
    m
}

#[cfg(feature = "schema-dns")]
fn dns_cache_stats_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::dns_capnp::dns_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_cache_stats_results::Builder<'_>>();
        r.set_hits(7);
        r.set_misses(3);
        r.set_size(12);
        r.set_evictions(1);
    }
    m
}

#[cfg(feature = "schema-dns")]
fn dns_config_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::dns_capnp::dns_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_config_results::Builder<'_>>();
        let mut config = r.reborrow().init_config();
        config.reborrow().set_default_transport("udp");
    }
    m
}

/// The socket family: `SocketService` create/info/name/close (per-handle).
#[cfg(feature = "schema-socket")]
fn socket_answer(ordinal: u16) -> Vec<u8> {
    #[allow(unused_imports)]
    use smart_cli::schema::socket_capnp::socket_service as w;
    match ordinal {
        0 => ok(encode_message(&socket_create_msg())),
        11 => ok(encode_message(&socket_info_msg())),
        19 => ok(encode_message(&socket_name_msg())),
        17 => ok(encode_message(&void_ok_msg())),
        other => not_supported(&format!("socket ordinal @{other}")),
    }
}

#[cfg(feature = "schema-socket")]
fn socket_create_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::socket_capnp::socket_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::create_results::Builder<'_>>();
        let mut handle = r.reborrow().init_handle();
        handle.reborrow().init_id().set_id(7);
    }
    m
}

#[cfg(feature = "schema-socket")]
fn socket_info_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::socket_capnp::socket_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_info_results::Builder<'_>>();
        let mut info = r.reborrow().init_info();
        info.set_local_port(4242);
        info.set_bytes_sent(10);
        info.set_bytes_received(20);
    }
    m
}

#[cfg(feature = "schema-socket")]
fn socket_name_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::socket_capnp::socket_service as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_socket_name_results::Builder<'_>>();
        r.set_port(4242);
    }
    m
}

/// Re-encode a `setFirewallRules` params message as the `getFirewallRules`
/// results message a following read would return (the double holds state).
#[cfg(feature = "schema-network")]
fn rules_results_from_params(request: &frame::RequestView<'_>) -> Option<Vec<u8>> {
    use network_capnp::network as w;
    let params = smart_cli::transport::decode_message::<w::set_firewall_rules_params::Owned>(
        request.params,
    )
    .ok()?;
    let id = params.get().ok()?.get_interface_id().ok()?.get_id();
    let mut message = capnp::message::Builder::new_default();
    {
        let mut r = message.init_root::<w::get_firewall_rules_results::Builder<'_>>();
        let params = params.get().ok()?;
        let rules = params.get_rules().ok()?;
        let mut list = r.reborrow().init_rules(rules.len());
        for (i, rule) in rules.into_iter().enumerate() {
            let src = rule;
            let mut dst = list.reborrow().get(i as u32);
            dst.reborrow().set_name(src.get_name().ok()?.to_str().ok()?);
            dst.set_enabled(src.get_enabled());
            dst.reborrow().set_direction(src.get_direction().ok()?);
            dst.reborrow().set_action(src.get_action().ok()?);
            dst.reborrow().set_protocol(src.get_protocol().ok()?.to_str().ok()?);
            dst.set_priority(src.get_priority());
            dst.set_hit_count(src.get_hit_count());
            dst.set_enabled(src.get_enabled());
        }
        let _ = id;
    }
    Some(smart_cli::transport::encode_message(&message))
}

/// Re-encode a `setRoutes` params message as the `getRoutes` results message.
#[cfg(feature = "schema-network")]
fn routes_results_from_params(request: &frame::RequestView<'_>) -> Option<Vec<u8>> {
    use network_capnp::network as w;
    let params = smart_cli::transport::decode_message::<w::set_routes_params::Owned>(request.params).ok()?;
    let routes = params.get().ok()?.get_routes().ok()?;
    let mut message = capnp::message::Builder::new_default();
    {
        let mut r = message.init_root::<w::get_routes_results::Builder<'_>>();
        let mut list = r.reborrow().init_routes(routes.len());
        for (i, route) in routes.into_iter().enumerate() {
            let src = route;
            let mut dst = list.reborrow().get(i as u32);
            dst.reborrow().set_destination_cidr(src.get_destination_cidr().ok()?.to_str().ok()?);
            dst.reborrow().set_gateway(src.get_gateway().ok()?.to_str().ok()?);
            dst.set_metric(src.get_metric());
            dst.reborrow().init_interface_id().set_id(src.get_interface_id().ok()?.get_id());
        }
        r.set_total(routes.len() as u64);
    }
    Some(smart_cli::transport::encode_message(&message))
}

fn registry_answer(request: frame::RequestView<'_>, cfg: &Cfg) -> Vec<u8> {
    let _ = request;
    let mut message = capnp::message::Builder::new_default();
    {
        // listUnavailable @13 on the cli interface.
        let mut results = message.init_root::<smart_cli::schema::cli_capnp::cli_service::list_unavailable_results::Builder<'_>>();
        let mut list = results.reborrow().init_commands(cfg.unavailable.len() as u32);
        for (i, path) in cfg.unavailable.iter().enumerate() {
            let mut reg = list.reborrow().get(i as u32);
            reg.reborrow().set_command_path(path);
            reg.set_unavailable_reason(smart_cli::schema::cli_capnp::AvailabilityReason::ServiceUnavailable);
            let mut meta = reg.reborrow().init_metadata();
            let mut entries = meta.reborrow().init_entries(1);
            let mut e = entries.reborrow().get(0);
            e.reborrow().set_key("reason");
            e.set_value("this host cannot run it (mock)");
        }
    }
    ok(encode_message(&message))
}

// ── stream framing helpers (the mock's own, mirroring the transport) ───────

fn read_frame(stream: &mut UnixStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let total = u32::from_be_bytes(len);
    let mut buf = vec![0u8; total as usize];
    stream.read_exact(&mut buf)?;
    Ok((buf[0], buf[1..].to_vec()))
}

fn write_frame(stream: &mut UnixStream, kind: u8, payload: &[u8]) -> std::io::Result<()> {
    let total = (1 + payload.len()) as u32;
    stream.write_all(&total.to_be_bytes())?;
    stream.write_all(&[kind])?;
    stream.write_all(payload)?;
    Ok(())
}

// ── response builders ──────────────────────────────────────────────────────

fn ok(payload: Vec<u8>) -> Vec<u8> {
    frame::encode_ok(&payload)
}

fn denied(what: &str) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    {
        let mut error = message.init_root::<types_capnp::error::Builder<'_>>();
        error.set_code(types_capnp::ErrorCode::PermissionDenied);
        let msg = format!("permissionDenied: {what}; this connection's capability class is ReadOnly");
        error.set_message(&msg);
    }
    frame::encode_response(STATUS_ERROR, "permissionDenied", &encode_message(&message))
}

fn failed(what: &str) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    {
        let mut error = message.init_root::<types_capnp::error::Builder<'_>>();
        error.set_code(types_capnp::ErrorCode::Failed);
        let msg = format!("failed: {what}");
        error.set_message(&msg);
    }
    frame::encode_response(STATUS_ERROR, "failed", &encode_message(&message))
}

fn not_supported(what: &str) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    {
        let mut error = message.init_root::<types_capnp::error::Builder<'_>>();
        error.set_code(types_capnp::ErrorCode::NotSupported);
        let msg = format!("serviceUnavailable: {what} is not available on this host (mock)");
        error.set_message(&msg);
    }
    frame::encode_response(STATUS_ERROR, "serviceUnavailable", &encode_message(&message))
}

fn matched_handshake() -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    {
        let mut resp = message.init_root::<abi_capnp::abi_handshake_response::Builder<'_>>();
        let mut matched = resp.reborrow().init_matched();
        matched.set_namespace("smart-protocol");
        let mut version = matched.reborrow().init_version();
        version.set_major(0);
        version.set_minor(1);
        version.set_patch(0);
        #[cfg(feature = "schema-network")]
        {
            let mut schemas = matched.reborrow().init_schema_versions(1);
            let mut schema = schemas.reborrow().get(0);
            schema.set_id(network_schema_id());
            schema.set_name("smart-protocol/network");
        }
        #[cfg(not(feature = "schema-network"))]
        {
            let _ = matched.reborrow().init_schema_versions(0);
        }
    }
    encode_message(&message)
}
#[cfg(feature = "schema-network")]

fn interfaces_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_interfaces_results::Builder<'_>>();
        let mut list = r.reborrow().init_interfaces(1);
        let mut it = list.reborrow().get(0);
        it.reborrow().init_id().set_id(1);
        it.reborrow().set_name("wlan0");
        it.reborrow().set_type("wifi");
        it.set_up(true);
        it.set_connected(true);
        it.set_mt_u(1500);
    }
    m
}
#[cfg(feature = "schema-network")]

fn radio_states_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::list_radio_states_results::Builder<'_>>();
        let mut list = r.reborrow().init_radios(1);
        let mut radio = list.reborrow().get(0);
        radio.reborrow().set_name("wifi");
        radio.set_enabled(true);
        r.set_total(1);
    }
    m
}
#[cfg(feature = "schema-network")]

fn status_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_status_results::Builder<'_>>();
        let mut s = r.reborrow().init_status();
        s.set_connected(true);
        s.reborrow().init_interface().set_id(1);
        s.reborrow().set_type("wifi");
        s.set_strength(-52);
        s.set_speed(866_000_000);
        s.set_quality(87);
        s.reborrow().set_host_state("managed");
        let mut wifi = s.reborrow().init_wifi();
        wifi.reborrow().set_ssid("MockNet");
    }
    m
}
#[cfg(feature = "schema-network")]

fn is_networking_msg(enabled: bool) -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::is_networking_enabled_results::Builder<'_>>();
        r.set_enabled(enabled);
    }
    m
}
#[cfg(feature = "schema-network")]

fn wifi_scan_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::wifi_scan_results::Builder<'_>>();
        let mut list = r.reborrow().init_networks(1);
        let mut n = list.reborrow().get(0);
        n.reborrow().set_ssid("MockNet");
        n.set_strength(-50);
        n.reborrow().set_security("wpa2");
    }
    m
}
#[cfg(feature = "schema-network")]

fn known_wifi_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::list_known_wifi_networks_results::Builder<'_>>();
        let mut list = r.reborrow().init_networks(2);
        let mut a = list.reborrow().get(0);
        a.reborrow().set_ssid("Home");
        a.reborrow().set_security("wpa2");
        a.set_auto_connect(true);
        let mut b = list.reborrow().get(1);
        b.reborrow().set_ssid("Cafe");
        b.reborrow().set_security("open");
        b.set_auto_connect(false);
        r.set_total(2);
    }
    m
}
#[cfg(feature = "schema-network")]

fn routes_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_routes_results::Builder<'_>>();
        let mut list = r.reborrow().init_routes(1);
        let mut route = list.reborrow().get(0);
        route.reborrow().set_destination_cidr("0.0.0.0/0");
        route.reborrow().set_gateway("192.168.1.1");
        route.set_metric(100);
        route.reborrow().init_interface_id().set_id(1);
        r.set_total(1);
    }
    m
}
#[cfg(feature = "schema-network")]

fn proxy_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_proxy_config_results::Builder<'_>>();
        let mut c = r.reborrow().init_config();
        c.set_enabled(false);
        c.reborrow().set_url("");
    }
    m
}
#[cfg(feature = "schema-network")]

fn dns_servers_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_dns_servers_results::Builder<'_>>();
        let mut list = r.reborrow().init_servers(1);
        let mut s = list.reborrow().get(0);
        let mut addr = s.reborrow().init_address();
        let octets = std::net::Ipv4Addr::new(1, 1, 1, 1).octets();
        let mut v4 = addr.reborrow().init_v4();
        v4.set_octets(&octets.to_vec());
    }
    m
}
#[cfg(feature = "schema-network")]

fn firewall_rules_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_firewall_rules_results::Builder<'_>>();
        let mut list = r.reborrow().init_rules(1);
        let mut rule = list.reborrow().get(0);
        rule.reborrow().set_name("existing");
        rule.set_enabled(true);
        rule.reborrow().set_direction(network_capnp::FirewallDirection::Outbound);
        rule.set_protocol("tcp");
        rule.reborrow().set_action(network_capnp::FirewallAction::Accept);
        rule.set_hit_count(42);
    }
    m
}
#[cfg(feature = "schema-network")]

fn counters_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::get_counters_results::Builder<'_>>();
        let mut c = r.reborrow().init_counters();
        c.set_bytes_in(123_456);
        c.set_bytes_out(78_910);
        c.set_packets_in(1000);
        c.set_packets_out(900);
    }
    m
}
#[cfg(feature = "schema-network")]

fn ping_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::ping_results::Builder<'_>>();
        let mut latency = r.reborrow().init_latency();
        latency.set_nanoseconds(12_300_000);
        r.set_packet_loss(0.0);
    }
    m
}
#[cfg(feature = "schema-network")]

fn vpn_list_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use network_capnp::network as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::list_vpn_profiles_results::Builder<'_>>();
        let _ = r.reborrow().init_profiles(0);
    }
    m
}

fn void_ok_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<types_capnp::void_result::Builder<'_>>();
        r.set_success(());
    }
    m
}

fn cli_schema_id() -> u64 {
    smart_cli::contract::schema_id("cli").unwrap_or(0)
}

#[cfg(feature = "schema-network")]
fn network_schema_id() -> u64 {
    smart_cli::contract::schema_id("network").unwrap_or(0)
}
