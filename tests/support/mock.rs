// A mock service: a test double that SPEAKS THE CONTRACT'S OWN CALL MODEL
// (capnp RPC) over a unix socket, so every CLI test runs against it with no
// daemon and no live host (charter law #8 — mock only).
//
// The mock is CONTRACT-ONLY: it is implemented from the schema alone. It
// serves the GENERATED server traits (`network_capnp::network::Server`,
// `cli_capnp::cli_service::Server`, …), so the dispatch is the generated one:
// the interface id and the method id arrive from the wire and the generated
// dispatch routes them. There is no hand-written ordinal table here and no
// second wire vocabulary — which is exactly what makes this double the proof
// that "delete the library, write your own implementation, the CLI works the
// same".
//
// The mock is honest like the real service: a read-only-class connection is
// refused `permissionDenied` before any backend, and unavailable commands
// answer `notSupported` with a reason — never a fabricated success.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use capnp::capability::{FromClientHook, Results};
use capnp::traits::HasTypeId;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty;
use capnp_rpc::RpcSystem;
use futures::executor::LocalPool;

use smart_cli::schema::{abi_capnp, cli_capnp, types_capnp};

/// The FIXTURE schema's generated bindings: a schema the CLI library never
/// compiles (the mock compiles it for itself, the way a service compiles its
/// own contract).
#[allow(unused_imports)]
use crate::support::fixture::fixture_capnp as fixture;
#[cfg(feature = "schema-network")]
use smart_cli::schema::network_capnp;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// One mock service's mutable state: the calls it has served (the seam-count
/// measurement) and the rule/route sets its mutators wrote, so a read after a
/// write across separate CLI processes sees the write (a real service holds
/// state; so does this double).
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
    /// The calls this service has answered.
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

/// The service's configuration (the capability class + the shared state).
#[derive(Clone)]
struct Cfg {
    control: bool,
    unavailable: Vec<String>,
    /// Answer the master-switch read with the contract's generic failure, so
    /// the exit-1 path (the call failed) is exercised against a real service
    /// double instead of a fabricated one.
    fail_reads: bool,
    /// The shared state (call counts + written sets).
    state: Arc<State>,
}

impl MockService {
    /// Start a mock; `control` selects the capability class it serves at.
    pub fn start(control: bool) -> std::io::Result<Self> {
        MockService::start_with(control, Vec::new())
    }

    /// The calls this service has answered (the seam-count measurement).
    pub fn requests(&self) -> usize {
        self.state.requests()
    }

    /// Start a mock whose reads answer the contract's generic failure.
    pub fn start_failing_reads() -> std::io::Result<Self> {
        Self::spawn(true, Vec::new(), true)
    }

    /// Start a mock whose registry reports the given commands unavailable.
    pub fn start_with(control: bool, unavailable: Vec<String>) -> std::io::Result<Self> {
        Self::spawn(control, unavailable, false)
    }

    /// Bind the socket and serve on a background thread.
    fn spawn(control: bool, unavailable: Vec<String>, fail_reads: bool) -> std::io::Result<Self> {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("smart-cli-mock-{}-{unique}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let state = Arc::new(State::new());
        let cfg = Cfg {
            control,
            unavailable,
            fail_reads,
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

/// Accept connections and serve each on its own thread.
fn accept_loop(listener: std::os::unix::net::UnixListener, cfg: Cfg) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cfg = cfg.clone();
                thread::spawn(move || {
                    let _ = serve_conn(stream, cfg);
                });
            }
            Err(_) => break,
        }
    }
}

/// Serve one connection: capnp RPC over the socket, with the contract's own
/// root capability (every interface the mock serves).
fn serve_conn(stream: std::os::unix::net::UnixStream, cfg: Cfg) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let (reader, writer) = smart_cli::rpc::PumpedStream::new(Box::new(reader), Box::new(stream)).split();
    let network: Box<dyn capnp_rpc::VatNetwork<twoparty::VatId>> = Box::new(
        twoparty::VatNetwork::new(
            reader,
            writer,
            Side::Server,
            capnp::message::ReaderOptions::new(),
        ),
    );
    let system: RpcSystem<twoparty::VatId> = RpcSystem::new(network, Some(bootstrap(cfg)));
    let mut pool = LocalPool::new();
    let _ = pool.run_until(system);
}

/// The mock's ROOT capability: every contract interface it serves.
///
/// A capnp RPC connection has exactly one bootstrap capability, and every
/// call on it carries the **interface id** the contract's generated code
/// assigned to the interface it means. So the bootstrap routes on the
/// interface id — and the *methods* are dispatched by each interface's
/// GENERATED `ServerDispatch`. There is no hand-written ordinal table here.
#[derive(Clone)]
struct InterfaceRouter {
    routes: Rc<Vec<Route>>,
    broken: capnp::capability::Client,
}

/// One contract interface the mock serves.
struct Route {
    /// The interface id the contract's generated code gave this interface.
    interface_id: u64,
    /// The hook that dispatches to the generated server for that interface.
    hook: Box<dyn capnp::private::capability::ClientHook>,
}

impl InterfaceRouter {
    /// The route for an interface id, if the mock serves it.
    fn route(&self, interface_id: u64) -> Option<&Route> {
        self.routes.iter().find(|route| route.interface_id == interface_id)
    }
}

impl capnp::private::capability::ClientHook for InterfaceRouter {
    fn add_ref(&self) -> Box<dyn capnp::private::capability::ClientHook> {
        Box::new(self.clone())
    }

    fn new_call(
        &self,
        interface_id: u64,
        method_id: u16,
        size_hint: Option<capnp::MessageSize>,
    ) -> capnp::capability::Request<capnp::any_pointer::Owned, capnp::any_pointer::Owned> {
        match self.route(interface_id) {
            Some(route) => route.hook.new_call(interface_id, method_id, size_hint),
            None => self.broken.new_call(interface_id, method_id, size_hint),
        }
    }

    fn call(
        &self,
        interface_id: u64,
        method_id: u16,
        params: Box<dyn capnp::private::capability::ParamsHook>,
        results: Box<dyn capnp::private::capability::ResultsHook>,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        match self.route(interface_id) {
            Some(route) => route.hook.call(interface_id, method_id, params, results),
            None => capnp::capability::Promise::err(capnp::Error::unimplemented(format!(
                "no contract interface with id {interface_id} is served on this connection"
            ))),
        }
    }

    fn get_brand(&self) -> usize {
        0
    }

    fn get_ptr(&self) -> usize {
        Rc::as_ptr(&self.routes) as *const u8 as usize
    }

    fn get_resolved(&self) -> Option<Box<dyn capnp::private::capability::ClientHook>> {
        None
    }

    fn when_more_resolved(
        &self,
    ) -> Option<capnp::capability::Promise<Box<dyn capnp::private::capability::ClientHook>, capnp::Error>>
    {
        None
    }

    fn when_resolved(&self) -> capnp::capability::Promise<(), capnp::Error> {
        capnp::capability::Promise::ok(())
    }
}

/// Build the mock's router: every contract interface this build serves.
fn bootstrap(cfg: Cfg) -> capnp::capability::Client {
    let mut routes: Vec<Route> = Vec::new();
    #[cfg(feature = "schema-network")]
    {
        let client: network_capnp::network::Client =
            capnp_rpc::new_client(NetworkServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: network_capnp::network::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    {
        let client: cli_capnp::cli_service::Client = capnp_rpc::new_client(CliServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: cli_capnp::cli_service::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    {
        let client: abi_capnp::abi_service::Client = capnp_rpc::new_client(AbiServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: abi_capnp::abi_service::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-dns")]
    {
        let client: smart_cli::schema::dns_capnp::dns_service::Client =
            capnp_rpc::new_client(DnsServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::dns_capnp::dns_service::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-socket")]
    {
        let client: smart_cli::schema::socket_capnp::socket_service::Client =
            capnp_rpc::new_client(SocketServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::socket_capnp::socket_service::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-bluetooth")]
    {
        let client: smart_cli::schema::bluetooth_capnp::bluetooth_service::Client =
            capnp_rpc::new_client(BluetoothServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::bluetooth_capnp::bluetooth_service::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-tunnel")]
    {
        let client: smart_cli::schema::tunnel_capnp::tunnel_service::Client =
            capnp_rpc::new_client(TunnelServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::tunnel_capnp::tunnel_service::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-web")]
    {
        let client: smart_cli::schema::web_capnp::web_service::Client =
            capnp_rpc::new_client(WebServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::web_capnp::web_service::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    {
        // The FIXTURE schema's interface: a schema the CLI library never
        // compiled. The mock serves it the way a real service serves its own
        // contract, and the CLI reaches it through the generic path.
        let client: fixture::echo::Client = capnp_rpc::new_client(EchoServer {
            cfg: cfg.clone(),
        });
        routes.push(Route {
            interface_id: fixture::echo::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-discovery")]
    {
        let client: smart_cli::schema::discovery_capnp::discovery::Client =
            capnp_rpc::new_client(DiscoveryServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::discovery_capnp::discovery::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    capnp::capability::Client::new(Box::new(InterfaceRouter {
        routes: Rc::new(routes),
        broken: capnp_rpc::new_broken_cap(capnp::Error::unimplemented(
            "no contract interface is served on this connection".to_string(),
        )),
    }))
}

/// Copy a built message into the call's results struct (the contract's own
/// results struct, byte for byte).
fn fill<'a, T>(
    message: &'a capnp::message::Builder<capnp::message::HeapAllocator>,
    mut results: Results<T>,
) where
    T: capnp::traits::Owned,
{
    if let Ok(reader) = message.get_root_as_reader::<T::Reader<'a>>() {
        let _ = results.set(reader);
    }
}

/// The contract's own refusal, as an RPC error whose text carries the
/// contract's code token first (the channel the contract defines).
fn refuse(code: &str, what: &str) -> capnp::Error {
    capnp::Error {
        kind: match code {
            "permissionDenied" => capnp::ErrorKind::Failed,
            "notSupported" | "unavailable" => capnp::ErrorKind::Unimplemented,
            _ => capnp::ErrorKind::Failed,
        },
        extra: format!("{code}: {what}"),
    }
}

/// The ABI handshake: the contract's own `AbiHandshakeResponse`.
struct AbiServer {
    cfg: Cfg,
}

impl abi_capnp::abi_service::Server for AbiServer {
    fn handshake(
        self: Rc<Self>,
        _params: abi_capnp::abi_service::HandshakeParams,
        results: abi_capnp::abi_service::HandshakeResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
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
        fill(&message, results);
        std::future::ready(Ok(()))
    }
}

/// The CLI registry: the contract's own `listCommands` / `listUnavailable`.
struct CliServer {
    cfg: Cfg,
}

impl cli_capnp::cli_service::Server for CliServer {
    fn list_unavailable(
        self: Rc<Self>,
        _params: cli_capnp::cli_service::ListUnavailableParams,
        results: cli_capnp::cli_service::ListUnavailableResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        let unavailable = self.cfg.unavailable.clone();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut list_results =
                message.init_root::<cli_capnp::cli_service::list_unavailable_results::Builder<'_>>();
            let mut list = list_results.reborrow().init_commands(unavailable.len() as u32);
            for (i, path) in unavailable.iter().enumerate() {
                let mut reg = list.reborrow().get(i as u32);
                reg.reborrow().set_command_path(path);
                reg.set_unavailable_reason(cli_capnp::AvailabilityReason::ServiceUnavailable);
                let mut meta = reg.reborrow().init_metadata();
                let mut entries = meta.reborrow().init_entries(1);
                let mut e = entries.reborrow().get(0);
                e.reborrow().set_key("reason");
                e.set_value("this host cannot run it (mock)");
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn list_commands(
        self: Rc<Self>,
        _params: cli_capnp::cli_service::ListCommandsParams,
        results: cli_capnp::cli_service::ListCommandsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        let unavailable = self.cfg.unavailable.clone();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut list_results =
                message.init_root::<cli_capnp::cli_service::list_commands_results::Builder<'_>>();
            let mut published = published_commands(&unavailable);
            let mut list = list_results.reborrow().init_commands(published.len() as u32);
            for (i, command) in published.iter().enumerate() {
                let mut reg = list.reborrow().get(i as u32);
                reg.reborrow().set_command_path(command.path);
                reg.reborrow().init_schema_id().set_id(command.schema_id);
                reg.reborrow().set_interface_name(command.interface);
                reg.reborrow().set_method_name(command.method);
                let mut presentation = reg.reborrow().init_presentation();
                presentation.reborrow().set_domain(command.domain);
                presentation.reborrow().set_summary(command.summary);
                if unavailable.iter().any(|u| u == command.path) {
                    reg.set_unavailable_reason(cli_capnp::AvailabilityReason::ServiceUnavailable);
                    let mut meta = reg.reborrow().init_metadata();
                    let mut entries = meta.reborrow().init_entries(1);
                    let mut e = entries.reborrow().get(0);
                    e.reborrow().set_key("reason");
                    e.set_value("this host cannot run it (mock)");
                }
            }
            // The FIXTURE schema's command: the CLI has no binding for it, so
            // the only way to run it is the generic path — and the registry
            // publishes the schema's node tree so the CLI can address it.
            published.push(PublishedCommand {
                path: "net fixture echo",
                schema_id: fixture_schema_id(),
                interface: "Echo",
                method: "echo",
                domain: "fixture",
                summary: "the fixture schema's echo (a schema the CLI never compiled)",
            });
            let mut list = list_results.reborrow().init_commands(published.len() as u32);
            for (i, command) in published.iter().enumerate() {
                let mut reg = list.reborrow().get(i as u32);
                reg.reborrow().set_command_path(command.path);
                reg.reborrow().init_schema_id().set_id(command.schema_id);
                reg.reborrow().set_interface_name(command.interface);
                reg.reborrow().set_method_name(command.method);
                let mut presentation = reg.reborrow().init_presentation();
                presentation.reborrow().set_domain(command.domain);
                presentation.reborrow().set_summary(command.summary);
                if command.domain == "fixture" {
                    let mut meta = reg.reborrow().init_metadata();
                    let mut entries = meta.reborrow().init_entries(1);
                    let mut e = entries.reborrow().get(0);
                    e.reborrow().set_key(smart_cli::dynamic::SCHEMA_METADATA_KEY);
                    e.set_value(fixture_schema_hex());
                }
            }
            list_results.set_total(published.len() as u64);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }
}

/// One command the mock publishes in its registry.
#[derive(Clone)]
struct PublishedCommand {
    path: &'static str,
    schema_id: u64,
    interface: &'static str,
    method: &'static str,
    domain: &'static str,
    summary: &'static str,
}

/// The mock's published registry: the contract's own command surface.
///
/// The paths are the CLI's own grammar; the identity (schema id + interface +
/// method) is the contract's, so the CLI can route by identity alone.
fn published_commands(_unavailable: &[String]) -> Vec<PublishedCommand> {
    let mut commands = Vec::new();
    #[cfg(feature = "schema-network")]
    {
        let id = network_schema_id();
        for (path, method, summary) in [
            ("net wifi scan", "wifiScan", "scan for wifi networks"),
            ("net wifi list", "listKnownWifiNetworks", "the known wifi networks"),
            ("net status", "getStatus", "the current connection status"),
            ("net interfaces", "getInterfaces", "the network interfaces"),
            ("net bandwidth", "getCounters", "the interface counters"),
            ("net routes", "getRoutes", "the routing table"),
            ("net firewall", "getFirewallRules", "the firewall rules"),
            ("net dns", "getDnsServers", "the dns servers"),
            ("net vpn", "listVpnProfiles", "the vpn profiles"),
            ("net proxy", "getProxyConfig", "the proxy configuration"),
        ] {
            commands.push(PublishedCommand {
                path,
                schema_id: id,
                interface: "Network",
                method,
                domain: "network",
                summary,
            });
        }
    }
    commands
}

/// The FIXTURE schema's server: the proof that the CLI can call a schema it
/// never compiled.
struct EchoServer {
    cfg: Cfg,
}

impl fixture::echo::Server for EchoServer {
    fn echo(
        self: Rc<Self>,
        params: fixture::echo::EchoParams,
        mut results: fixture::echo::EchoResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        let request = params.get().ok();
        let host = request
            .as_ref()
            .and_then(|r| r.get_host().ok())
            .map(|t| t.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let count = request.as_ref().map(|r| r.get_count()).unwrap_or(1);
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<fixture::echo::echo_results::Builder<'_>>();
            let mut response = r.reborrow().init_response();
            response.set_pong(format!("pong:{host}"));
            response.set_count(count);
            response.set_ok(true);
        }
        let reader = message
            .get_root_as_reader::<fixture::echo::echo_results::Reader<'_>>()
            .ok();
        if let Some(reader) = reader {
            let _ = results.set(reader);
        }
        std::future::ready(Ok(()))
    }
}

/// The FIXTURE schema's node tree, hex-encoded: the contract's own
/// `CodeGeneratorRequest`, published so a client that never compiled the
/// schema can still address the call.
fn fixture_schema_hex() -> String {
    const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fixture_schema.bin"));
    let mut out = String::with_capacity(BYTES.len() * 2);
    for byte in BYTES {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The FIXTURE schema's canonical id, read from its own source.
fn fixture_schema_id() -> u64 {
    const SOURCE: &str = include_str!("../fixtures/fixture.capnp");
    for line in SOURCE.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@0x") {
            if let Some(end) = rest.find(';') {
                if let Ok(id) = u64::from_str_radix(&rest[..end], 16) {
                    return id;
                }
            }
        }
    }
    0
}

// ── the network family ─────────────────────────────────────────────────────

/// The network family: the GENERATED server trait, implemented from the
/// schema alone.
#[cfg(feature = "schema-network")]
struct NetworkServer {
    cfg: Cfg,
}

#[cfg(feature = "schema-network")]
impl network_capnp::network::Server for NetworkServer {
    fn get_interfaces(
        self: Rc<Self>,
        _params: network_capnp::network::GetInterfacesParams,
        results: network_capnp::network::GetInterfacesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&interfaces_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_status(
        self: Rc<Self>,
        _params: network_capnp::network::GetStatusParams,
        results: network_capnp::network::GetStatusResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&status_msg(), results);
        std::future::ready(Ok(()))
    }

    fn wifi_scan(
        self: Rc<Self>,
        _params: network_capnp::network::WifiScanParams,
        results: network_capnp::network::WifiScanResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&wifi_scan_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_counters(
        self: Rc<Self>,
        _params: network_capnp::network::GetCountersParams,
        results: network_capnp::network::GetCountersResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&counters_msg(), results);
        std::future::ready(Ok(()))
    }

    fn ping(
        self: Rc<Self>,
        _params: network_capnp::network::PingParams,
        results: network_capnp::network::PingResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&ping_msg(), results);
        std::future::ready(Ok(()))
    }

    fn is_networking_enabled(
        self: Rc<Self>,
        _params: network_capnp::network::IsNetworkingEnabledParams,
        results: network_capnp::network::IsNetworkingEnabledResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        if self.cfg.fail_reads {
            return std::future::ready(Err(refuse(
                "failed",
                "the service could not read the networking state",
            )));
        }
        fill(&is_networking_msg(true), results);
        std::future::ready(Ok(()))
    }

    fn list_radio_states(
        self: Rc<Self>,
        _params: network_capnp::network::ListRadioStatesParams,
        results: network_capnp::network::ListRadioStatesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&radio_states_msg(), results);
        std::future::ready(Ok(()))
    }

    fn list_known_wifi_networks(
        self: Rc<Self>,
        _params: network_capnp::network::ListKnownWifiNetworksParams,
        results: network_capnp::network::ListKnownWifiNetworksResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&known_wifi_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_routes(
        self: Rc<Self>,
        _params: network_capnp::network::GetRoutesParams,
        results: network_capnp::network::GetRoutesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        match written_set(&self.cfg.state.routes) {
            Some(bytes) => fill_from_bytes(&bytes, results),
            None => fill(&routes_msg(), results),
        }
        std::future::ready(Ok(()))
    }

    fn get_proxy_config(
        self: Rc<Self>,
        _params: network_capnp::network::GetProxyConfigParams,
        results: network_capnp::network::GetProxyConfigResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&proxy_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_dns_servers(
        self: Rc<Self>,
        _params: network_capnp::network::GetDnsServersParams,
        results: network_capnp::network::GetDnsServersResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&dns_servers_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_firewall_rules(
        self: Rc<Self>,
        _params: network_capnp::network::GetFirewallRulesParams,
        results: network_capnp::network::GetFirewallRulesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        match written_set(&self.cfg.state.firewall) {
            Some(bytes) => fill_from_bytes(&bytes, results),
            None => fill(&firewall_rules_msg(), results),
        }
        std::future::ready(Ok(()))
    }

    fn list_vpn_profiles(
        self: Rc<Self>,
        _params: network_capnp::network::ListVpnProfilesParams,
        results: network_capnp::network::ListVpnProfilesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&vpn_list_msg(), results);
        std::future::ready(Ok(()))
    }

    fn subscribe_counters(
        self: Rc<Self>,
        _params: network_capnp::network::SubscribeCountersParams,
        mut results: network_capnp::network::SubscribeCountersResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        let subscription: network_capnp::counter_subscription::Client =
            capnp_rpc::new_client(CountersSubscription {
                cfg: self.cfg.clone(),
            });
        results.get().set_subscription(subscription);
        std::future::ready(Ok(()))
    }

    fn subscribe_status(
        self: Rc<Self>,
        _params: network_capnp::network::SubscribeStatusParams,
        mut results: network_capnp::network::SubscribeStatusResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        let subscription: network_capnp::status_subscription::Client =
            capnp_rpc::new_client(StatusSubscription {
                cfg: self.cfg.clone(),
            });
        results.get().set_subscription(subscription);
        std::future::ready(Ok(()))
    }

    // ── the control verbs: the capability class decides them ───────────────
    fn set_networking(
        self: Rc<Self>,
        _params: network_capnp::network::SetNetworkingParams,
        results: network_capnp::network::SetNetworkingResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn set_radio_enabled(
        self: Rc<Self>,
        _params: network_capnp::network::SetRadioEnabledParams,
        results: network_capnp::network::SetRadioEnabledResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn wifi_connect(
        self: Rc<Self>,
        _params: network_capnp::network::WifiConnectParams,
        results: network_capnp::network::WifiConnectResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn wifi_disconnect(
        self: Rc<Self>,
        _params: network_capnp::network::WifiDisconnectParams,
        results: network_capnp::network::WifiDisconnectResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn forget_wifi_network(
        self: Rc<Self>,
        _params: network_capnp::network::ForgetWifiNetworkParams,
        results: network_capnp::network::ForgetWifiNetworkResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn set_dns_servers(
        self: Rc<Self>,
        _params: network_capnp::network::SetDnsServersParams,
        results: network_capnp::network::SetDnsServersResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn set_interface_mtu(
        self: Rc<Self>,
        _params: network_capnp::network::SetInterfaceMtuParams,
        results: network_capnp::network::SetInterfaceMtuResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn up_interface(
        self: Rc<Self>,
        _params: network_capnp::network::UpInterfaceParams,
        results: network_capnp::network::UpInterfaceResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn down_interface(
        self: Rc<Self>,
        _params: network_capnp::network::DownInterfaceParams,
        results: network_capnp::network::DownInterfaceResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    fn set_proxy_config(
        self: Rc<Self>,
        _params: network_capnp::network::SetProxyConfigParams,
        results: network_capnp::network::SetProxyConfigResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        control(&self.cfg, results)
    }

    // The full-set mutators persist what they were handed, so the honest
    // read→write→read composer is testable across processes (this double
    // holds state the way a service does).
    fn set_firewall_rules(
        self: Rc<Self>,
        params: network_capnp::network::SetFirewallRulesParams,
        results: network_capnp::network::SetFirewallRulesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        if !self.cfg.control {
            return std::future::ready(Err(denied()));
        }
        let written = rules_results_from_params(&params);
        match written {
            Some(bytes) => {
                *self.cfg.state.firewall.lock().unwrap() = Some(bytes);
                fill(&void_ok_msg(), results);
                std::future::ready(Ok(()))
            }
            None => std::future::ready(Err(refuse("failed", "the written set could not be decoded"))),
        }
    }

    fn set_routes(
        self: Rc<Self>,
        params: network_capnp::network::SetRoutesParams,
        results: network_capnp::network::SetRoutesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        if !self.cfg.control {
            return std::future::ready(Err(denied()));
        }
        let written = routes_results_from_params(&params);
        match written {
            Some(bytes) => {
                *self.cfg.state.routes.lock().unwrap() = Some(bytes);
                fill(&void_ok_msg(), results);
                std::future::ready(Ok(()))
            }
            None => std::future::ready(Err(refuse("failed", "the written set could not be decoded"))),
        }
    }
}

/// A control verb's answer: the capability class decides.
#[cfg(feature = "schema-network")]
fn control<T>(
    cfg: &Cfg,
    results: Results<T>,
) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static
where
    T: capnp::traits::Owned + 'static,
{
    if cfg.control {
        fill(&void_ok_msg(), results);
        std::future::ready(Ok(()))
    } else {
        std::future::ready(Err(denied()))
    }
}

/// The read-only class's refusal.
#[cfg(feature = "schema-network")]
fn denied() -> capnp::Error {
    refuse(
        "permissionDenied",
        "a control verb; this connection's capability class is ReadOnly",
    )
}

/// Copy a stored results message into the call's results struct.
#[cfg(feature = "schema-network")]
fn fill_from_bytes<'a, T>(bytes: &[u8], mut results: Results<T>)
where
    T: capnp::traits::Owned,
{
    let Ok(reader) = smart_cli::transport::decode_message::<T>(bytes) else {
        return;
    };
    let Ok(root) = reader.get() else {
        return;
    };
    let _ = results.set(root);
}

/// The counters subscription: the contract's own `pull`/`cancel` capability.
#[cfg(feature = "schema-network")]
struct CountersSubscription {
    cfg: Cfg,
}

#[cfg(feature = "schema-network")]
impl network_capnp::counter_subscription::Server for CountersSubscription {
    fn pull(
        self: Rc<Self>,
        _params: network_capnp::counter_subscription::PullParams,
        results: network_capnp::counter_subscription::PullResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        fill(&counters_msg(), results);
        std::future::ready(Ok(()))
    }

    fn cancel(
        self: Rc<Self>,
        _params: network_capnp::counter_subscription::CancelParams,
        results: network_capnp::counter_subscription::CancelResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        fill(&void_ok_msg(), results);
        std::future::ready(Ok(()))
    }
}

/// The status subscription: the contract's own `pull`/`cancel` capability.
#[cfg(feature = "schema-network")]
struct StatusSubscription {
    cfg: Cfg,
}

#[cfg(feature = "schema-network")]
impl network_capnp::status_subscription::Server for StatusSubscription {
    fn pull(
        self: Rc<Self>,
        _params: network_capnp::status_subscription::PullParams,
        results: network_capnp::status_subscription::PullResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        fill(&status_msg(), results);
        std::future::ready(Ok(()))
    }

    fn cancel(
        self: Rc<Self>,
        _params: network_capnp::status_subscription::CancelParams,
        results: network_capnp::status_subscription::CancelResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        fill(&void_ok_msg(), results);
        std::future::ready(Ok(()))
    }
}

// ── the sibling families (compiled only when the test links them) ───────────

/// The dns family.
#[cfg(feature = "schema-dns")]
struct DnsServer {
    cfg: Cfg,
}

#[cfg(feature = "schema-dns")]
impl smart_cli::schema::dns_capnp::dns_service::Server for DnsServer {
    fn resolve(
        self: Rc<Self>,
        _params: smart_cli::schema::dns_capnp::dns_service::ResolveParams,
        results: smart_cli::schema::dns_capnp::dns_service::ResolveResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&dns_resolve_msg(), results);
        std::future::ready(Ok(()))
    }

    fn reverse_resolve(
        self: Rc<Self>,
        _params: smart_cli::schema::dns_capnp::dns_service::ReverseResolveParams,
        results: smart_cli::schema::dns_capnp::dns_service::ReverseResolveResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&dns_reverse_msg(), results);
        std::future::ready(Ok(()))
    }

    fn resolve_batch(
        self: Rc<Self>,
        _params: smart_cli::schema::dns_capnp::dns_service::ResolveBatchParams,
        results: smart_cli::schema::dns_capnp::dns_service::ResolveBatchResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&dns_batch_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_cache_stats(
        self: Rc<Self>,
        _params: smart_cli::schema::dns_capnp::dns_service::GetCacheStatsParams,
        results: smart_cli::schema::dns_capnp::dns_service::GetCacheStatsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&dns_cache_stats_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_config(
        self: Rc<Self>,
        _params: smart_cli::schema::dns_capnp::dns_service::GetConfigParams,
        results: smart_cli::schema::dns_capnp::dns_service::GetConfigResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&dns_config_msg(), results);
        std::future::ready(Ok(()))
    }

    fn flush_cache(
        self: Rc<Self>,
        _params: smart_cli::schema::dns_capnp::dns_service::FlushCacheParams,
        results: smart_cli::schema::dns_capnp::dns_service::FlushCacheResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&void_ok_msg(), results);
        std::future::ready(Ok(()))
    }
}

/// The socket family.
#[cfg(feature = "schema-socket")]
struct SocketServer {
    cfg: Cfg,
}

#[cfg(feature = "schema-socket")]
impl smart_cli::schema::socket_capnp::socket_service::Server for SocketServer {
    fn create(
        self: Rc<Self>,
        _params: smart_cli::schema::socket_capnp::socket_service::CreateParams,
        results: smart_cli::schema::socket_capnp::socket_service::CreateResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&socket_create_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_info(
        self: Rc<Self>,
        _params: smart_cli::schema::socket_capnp::socket_service::GetInfoParams,
        results: smart_cli::schema::socket_capnp::socket_service::GetInfoResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&socket_info_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_socket_name(
        self: Rc<Self>,
        _params: smart_cli::schema::socket_capnp::socket_service::GetSocketNameParams,
        results: smart_cli::schema::socket_capnp::socket_service::GetSocketNameResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&socket_name_msg(), results);
        std::future::ready(Ok(()))
    }

    fn close(
        self: Rc<Self>,
        _params: smart_cli::schema::socket_capnp::socket_service::CloseParams,
        results: smart_cli::schema::socket_capnp::socket_service::CloseResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&void_ok_msg(), results);
        std::future::ready(Ok(()))
    }
}

/// The bluetooth family.
#[cfg(feature = "schema-bluetooth")]
struct BluetoothServer {
    cfg: Cfg,
}

#[cfg(feature = "schema-bluetooth")]
impl smart_cli::schema::bluetooth_capnp::bluetooth_service::Server for BluetoothServer {
    fn get_state(
        self: Rc<Self>,
        _params: smart_cli::schema::bluetooth_capnp::bluetooth_service::GetStateParams,
        results: smart_cli::schema::bluetooth_capnp::bluetooth_service::GetStateResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&bluetooth_state_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_name(
        self: Rc<Self>,
        _params: smart_cli::schema::bluetooth_capnp::bluetooth_service::GetNameParams,
        results: smart_cli::schema::bluetooth_capnp::bluetooth_service::GetNameResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&bluetooth_name_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_paired_devices(
        self: Rc<Self>,
        _params: smart_cli::schema::bluetooth_capnp::bluetooth_service::GetPairedDevicesParams,
        results: smart_cli::schema::bluetooth_capnp::bluetooth_service::GetPairedDevicesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&bluetooth_devices_msg(), results);
        std::future::ready(Ok(()))
    }

    fn enable(
        self: Rc<Self>,
        _params: smart_cli::schema::bluetooth_capnp::bluetooth_service::EnableParams,
        results: smart_cli::schema::bluetooth_capnp::bluetooth_service::EnableResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        if self.cfg.control {
            fill(&void_ok_msg(), results);
            std::future::ready(Ok(()))
        } else {
            std::future::ready(Err(denied()))
        }
    }

    fn disable(
        self: Rc<Self>,
        _params: smart_cli::schema::bluetooth_capnp::bluetooth_service::DisableParams,
        results: smart_cli::schema::bluetooth_capnp::bluetooth_service::DisableResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        if self.cfg.control {
            fill(&void_ok_msg(), results);
            std::future::ready(Ok(()))
        } else {
            std::future::ready(Err(denied()))
        }
    }

    fn set_name(
        self: Rc<Self>,
        _params: smart_cli::schema::bluetooth_capnp::bluetooth_service::SetNameParams,
        results: smart_cli::schema::bluetooth_capnp::bluetooth_service::SetNameResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        if self.cfg.control {
            fill(&void_ok_msg(), results);
            std::future::ready(Ok(()))
        } else {
            std::future::ready(Err(denied()))
        }
    }
}

/// The tunnel family.
#[cfg(feature = "schema-tunnel")]
struct TunnelServer {
    cfg: Cfg,
}

#[cfg(feature = "schema-tunnel")]
impl smart_cli::schema::tunnel_capnp::tunnel_service::Server for TunnelServer {
    fn open(
        self: Rc<Self>,
        _params: smart_cli::schema::tunnel_capnp::tunnel_service::OpenParams,
        results: smart_cli::schema::tunnel_capnp::tunnel_service::OpenResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&tunnel_open_msg(), results);
        std::future::ready(Ok(()))
    }

    fn get_info(
        self: Rc<Self>,
        _params: smart_cli::schema::tunnel_capnp::tunnel_service::GetInfoParams,
        results: smart_cli::schema::tunnel_capnp::tunnel_service::GetInfoResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&tunnel_info_msg(), results);
        std::future::ready(Ok(()))
    }

    fn close(
        self: Rc<Self>,
        _params: smart_cli::schema::tunnel_capnp::tunnel_service::CloseParams,
        results: smart_cli::schema::tunnel_capnp::tunnel_service::CloseResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        if self.cfg.control {
            fill(&void_ok_msg(), results);
            std::future::ready(Ok(()))
        } else {
            std::future::ready(Err(denied()))
        }
    }
}

/// The web family.
#[cfg(feature = "schema-web")]
struct WebServer {
    cfg: Cfg,
}

#[cfg(feature = "schema-web")]
impl smart_cli::schema::web_capnp::web_service::Server for WebServer {
    fn fetch(
        self: Rc<Self>,
        _params: smart_cli::schema::web_capnp::web_service::FetchParams,
        results: smart_cli::schema::web_capnp::web_service::FetchResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&web_fetch_msg(), results);
        std::future::ready(Ok(()))
    }

    fn resolve_url(
        self: Rc<Self>,
        _params: smart_cli::schema::web_capnp::web_service::ResolveUrlParams,
        results: smart_cli::schema::web_capnp::web_service::ResolveUrlResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&web_resolve_msg(), results);
        std::future::ready(Ok(()))
    }
}

/// The discovery family.
#[cfg(feature = "schema-discovery")]
struct DiscoveryServer {
    cfg: Cfg,
}

#[cfg(feature = "schema-discovery")]
impl smart_cli::schema::discovery_capnp::discovery::Server for DiscoveryServer {
    fn lookup(
        self: Rc<Self>,
        _params: smart_cli::schema::discovery_capnp::discovery::LookupParams,
        results: smart_cli::schema::discovery_capnp::discovery::LookupResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&discovery_lookup_msg(), results);
        std::future::ready(Ok(()))
    }

    fn resolve(
        self: Rc<Self>,
        _params: smart_cli::schema::discovery_capnp::discovery::ResolveParams,
        results: smart_cli::schema::discovery_capnp::discovery::ResolveResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        fill(&discovery_resolve_msg(), results);
        std::future::ready(Ok(()))
    }
}

// ── response builders (the contract's own structs) ──────────────────────────

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

fn void_ok_msg() -> capnp::message::Builder<capnp::message::HeapAllocator> {
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<types_capnp::void_result::Builder<'_>>();
        r.set_success(());
    }
    m
}

/// Re-encode a `setFirewallRules` params message as the `getFirewallRules`
/// results message a following read would return (the double holds state).
#[cfg(feature = "schema-network")]
fn rules_results_from_params(
    params: &network_capnp::network::SetFirewallRulesParams,
) -> Option<Vec<u8>> {
    use network_capnp::network as w;
    let params = params.get().ok()?;
    let rules = params.get_rules().ok()?;
    let mut message = capnp::message::Builder::new_default();
    {
        let mut r = message.init_root::<w::get_firewall_rules_results::Builder<'_>>();
        let mut list = r.reborrow().init_rules(rules.len());
        for (i, src) in rules.into_iter().enumerate() {
            let mut dst = list.reborrow().get(i as u32);
            dst.reborrow().set_name(src.get_name().ok()?.to_str().ok()?);
            dst.set_enabled(src.get_enabled());
            dst.reborrow().set_direction(src.get_direction().ok()?);
            dst.reborrow().set_action(src.get_action().ok()?);
            dst.reborrow().set_protocol(src.get_protocol().ok()?.to_str().ok()?);
            dst.set_priority(src.get_priority());
            dst.set_hit_count(src.get_hit_count());
        }
    }
    Some(smart_cli::transport::encode_message(&message))
}

/// Re-encode a `setRoutes` params message as the `getRoutes` results message.
#[cfg(feature = "schema-network")]
fn routes_results_from_params(params: &network_capnp::network::SetRoutesParams) -> Option<Vec<u8>> {
    use network_capnp::network as w;
    let params = params.get().ok()?;
    let routes = params.get_routes().ok()?;
    let mut message = capnp::message::Builder::new_default();
    {
        let mut r = message.init_root::<w::get_routes_results::Builder<'_>>();
        let mut list = r.reborrow().init_routes(routes.len());
        for (i, src) in routes.into_iter().enumerate() {
            let mut dst = list.reborrow().get(i as u32);
            dst.reborrow()
                .set_destination_cidr(src.get_destination_cidr().ok()?.to_str().ok()?);
            dst.reborrow().set_gateway(src.get_gateway().ok()?.to_str().ok()?);
            dst.set_metric(src.get_metric());
            dst.reborrow()
                .init_interface_id()
                .set_id(src.get_interface_id().ok()?.get_id());
        }
        r.set_total(routes.len() as u64);
    }
    Some(smart_cli::transport::encode_message(&message))
}

#[cfg(feature = "schema-network")]
fn network_schema_id() -> u64 {
    smart_cli::contract::schema_id("network").unwrap_or(0)
}
