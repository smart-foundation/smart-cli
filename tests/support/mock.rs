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

use smart_cli::schema::capability_capnp::Operation;
use smart_cli::schema::{abi_capnp, cli_capnp, types_capnp};

/// The FIXTURE schema's generated bindings: a schema the CLI library never
/// compiles (the mock compiles it for itself, the way a service compiles its
/// own contract).
#[allow(unused_imports)]
use crate::support::fixture::fixture_capnp as fixture;
#[cfg(feature = "schema-network")]
use smart_cli::schema::network_capnp;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The name this mock publishes itself under (in discovery).
#[cfg(feature = "schema-discovery")]
const MOCK_SERVICE_NAME: &str = "mock-service";

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
    /// The loopback address the same mock serves on (the tcp carrier).
    tcp_address: std::net::SocketAddr,
    state: Arc<State>,
    /// Whether control (mutating) verbs are permitted (the capability class).
    pub allow_control: bool,
    /// Commands the registry reports unavailable (empty by default).
    pub unavailable: Vec<String>,
}

/// The service's configuration (the capability class + the shared state).
#[derive(Clone)]
pub struct Cfg {
    pub control: bool,
    pub unavailable: Vec<String>,
    /// The address this mock is reachable at (the carrier + the address), so
    /// the discovery service can publish the endpoint a client then dials.
    pub carrier: String,
    pub address: String,
    /// The CALL MODEL this mock publishes in discovery (the contract's own
    /// "capnp", unless a test wants to prove the refusal path).
    pub call_model: String,
    /// Answer the master-switch read with the contract's generic failure, so
    /// the exit-1 path (the call failed) is exercised against a real service
    /// double instead of a fabricated one.
    pub fail_reads: bool,
    /// The shared state (call counts + written sets).
    pub state: Arc<State>,
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

    /// Start a mock that publishes a CALL MODEL this client may not speak —
    /// the proof that an unknown call model is refused with a reason, never
    /// a silent wrong protocol.
    pub fn start_with_call_model(control: bool, call_model: &str) -> std::io::Result<Self> {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("smart-cli-mock-{}-{unique}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let state = Arc::new(State::new());
        let cfg = Cfg {
            control,
            unavailable: Vec::new(),
            fail_reads: false,
            carrier: "unix".to_string(),
            address: path.display().to_string(),
            call_model: call_model.to_string(),
            state: Arc::clone(&state),
        };
        thread::spawn(move || accept_loop(listener, cfg));
        Ok(MockService {
            path,
            tcp_address: "127.0.0.1:0".parse().unwrap(),
            state,
            allow_control: control,
            unavailable: Vec::new(),
        })
    }

    /// Bind the socket and serve on a background thread.
    ///
    /// The mock serves BOTH carriers the contract's transports name: a unix
    /// socket and a TCP port on the loopback. The same service answers at
    /// both, so the interchange proof (the same command over unix and over
    /// tcp produces byte-identical output) runs against ONE implementation.
    fn spawn(control: bool, unavailable: Vec<String>, fail_reads: bool) -> std::io::Result<Self> {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("smart-cli-mock-{}-{unique}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let tcp = std::net::TcpListener::bind("127.0.0.1:0")?;
        let tcp_address = tcp.local_addr()?;
        let state = Arc::new(State::new());
        let cfg = Cfg {
            control,
            unavailable,
            fail_reads,
            carrier: "unix".to_string(),
            address: path.display().to_string(),
            call_model: "capnp".to_string(),
            state: Arc::clone(&state),
        };
        let tcp_cfg = Cfg {
            carrier: "tcp".to_string(),
            address: tcp_address.to_string(),
            state: Arc::clone(&state),
            ..cfg.clone()
        };
        thread::spawn(move || accept_loop(listener, cfg));
        thread::spawn(move || accept_tcp_loop(tcp, tcp_cfg));
        Ok(MockService {
            path,
            tcp_address,
            state,
            allow_control: control,
            unavailable: Vec::new(),
        })
    }

    /// The socket path to point the CLI at (`--connect <path>`).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The loopback address the same mock serves on (`--addr host:port`).
    pub fn tcp_address(&self) -> std::net::SocketAddr {
        self.tcp_address
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

/// Accept TCP connections and serve each on its own thread.
fn accept_tcp_loop(listener: std::net::TcpListener, cfg: Cfg) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cfg = cfg.clone();
                thread::spawn(move || {
                    let _ = serve_conn_tcp(stream, cfg);
                });
            }
            Err(_) => break,
        }
    }
}

/// Serve one TCP connection: the same service, the same contract, the other
/// carrier.
fn serve_conn_tcp(stream: std::net::TcpStream, cfg: Cfg) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let (reader, writer) =
        smart_cli::rpc::PumpedStream::new(Box::new(reader), Box::new(stream)).split();
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
    {
        // The capability service: what this service HOLDS, published by the
        // service itself (the client asks; it does not assume).
        let client: smart_cli::schema::capability_capnp::capability_service::Client =
            capnp_rpc::new_client(CapabilityServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::capability_capnp::capability_service::Client::TYPE_ID,
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
    // The six families the engine now serves (spine S1): the same generated
    // server traits, the same router, the same contract.
    #[cfg(feature = "schema-files")]
    {
        let client: smart_cli::schema::files_capnp::files::Client =
            capnp_rpc::new_client(crate::support::domains::FilesServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::files_capnp::files::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-process")]
    {
        let client: smart_cli::schema::process_capnp::process::Client =
            capnp_rpc::new_client(crate::support::domains::ProcessServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::process_capnp::process::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-settings")]
    {
        let client: smart_cli::schema::settings_capnp::settings::Client =
            capnp_rpc::new_client(crate::support::domains::SettingsServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::settings_capnp::settings::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-logging")]
    {
        let client: smart_cli::schema::logging_capnp::logging::Client =
            capnp_rpc::new_client(crate::support::domains::LoggingServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::logging_capnp::logging::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-clock")]
    {
        let client: smart_cli::schema::clock_capnp::clock_service::Client =
            capnp_rpc::new_client(crate::support::domains::ClockServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::clock_capnp::clock_service::Client::TYPE_ID,
            hook: client.into_client_hook(),
        });
    }
    #[cfg(feature = "schema-time")]
    {
        let client: smart_cli::schema::time_capnp::time::Client =
            capnp_rpc::new_client(crate::support::domains::TimeServer { cfg: cfg.clone() });
        routes.push(Route {
            interface_id: smart_cli::schema::time_capnp::time::Client::TYPE_ID,
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
        params: abi_capnp::abi_service::HandshakeParams,
        results: abi_capnp::abi_service::HandshakeResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        // The results struct is the contract's own: `handshake_results` with
        // its `response` field. The wire shape is the schema's, never a guess.
        let mut message = capnp::message::Builder::new_default();
        {
            let mut results_root =
                message.init_root::<abi_capnp::abi_service::handshake_results::Builder<'_>>();
            let mut resp = results_root.reborrow().init_response();
            let mut matched = resp.reborrow().init_matched();
            matched.set_namespace("smart-protocol");
            let mut version = matched.reborrow().init_version();
            version.set_major(0);
            version.set_minor(1);
            version.set_patch(0);
            // The INTERSECTION: the schemas the caller declared that this
            // service speaks. A caller that declares none gets this
            // service's own catalogue (there is nothing to intersect).
            let declared: Vec<u64> = params
                .get()
                .ok()
                .and_then(|params| params.get_request().ok())
                .map(|request| {
                    request
                        .get_caller_abi()
                        .map(|caller| {
                            match caller.get_schema_versions() {
                                Ok(versions) => (0..versions.len())
                                    .map(|index| versions.get(index).get_id())
                                    .collect(),
                                Err(_) => Vec::new(),
                            }
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            #[cfg(feature = "schema-network")]
            {
                let served = network_schema_id();
                let agreed = declared.is_empty() || declared.contains(&served);
                let count = if agreed { 1 } else { 0 };
                let mut schemas = matched.reborrow().init_schema_versions(count);
                if agreed {
                    let mut schema = schemas.reborrow().get(0);
                    schema.set_id(served);
                    schema.set_name("smart-protocol/network");
                }
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

/// The capability service: what this service holds, published by itself.
///
/// The published OPERATIONS are the class: a control-class service publishes
/// the mutating operations, a read-only one does not. The client renders
/// what it is given.
struct CapabilityServer {
    cfg: Cfg,
}

impl smart_cli::schema::capability_capnp::capability_service::Server for CapabilityServer {
    fn list(
        self: Rc<Self>,
        _params: smart_cli::schema::capability_capnp::capability_service::ListParams,
        results: smart_cli::schema::capability_capnp::capability_service::ListResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        let control = self.cfg.control;
        let mut message = capnp::message::Builder::new_default();
        {
            use smart_cli::schema::capability_capnp::capability_service as w;
            let mut r = message.init_root::<w::list_results::Builder<'_>>();
            let mut list = r.reborrow().init_caps(1);
            let mut capability = list.reborrow().get(0);
            {
                let id = capability.reborrow().init_id(4);
                id.copy_from_slice(&[0x6d, 0x6f, 0x63, 0x6b]);
            }
            capability.set_target("mock-service");
            let operations = if control { 2 } else { 1 };
            let mut ops = capability.reborrow().init_operations(operations);
            ops.reborrow().set(0, Operation::Read);
            if control {
                ops.reborrow().set(1, Operation::Execute);
            }
            capability.reborrow().init_audit().set_issuer("mock-service");
            r.set_total_count(1);
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
                // The metadata: the method's IDENTITY (the ordinal the schema
                // declares) and, for a schema the CLI never compiled, the
                // schema's node tree. Both are contract-legal map entries.
                let mut entries: Vec<(String, String)> = Vec::new();
                if let Some(ordinal) = method_ordinal(command.interface, command.method) {
                    entries.push((
                        smart_cli::registry::METADATA_METHOD_ID.to_string(),
                        ordinal.to_string(),
                    ));
                }
                if command.domain == "fixture" {
                    entries.push((
                        smart_cli::dynamic::SCHEMA_METADATA_KEY.to_string(),
                        fixture_schema_hex(),
                    ));
                }
                if !entries.is_empty() {
                    let mut meta = reg.reborrow().init_metadata();
                    let mut map = meta.reborrow().init_entries(entries.len() as u32);
                    for (index, (key, value)) in entries.iter().enumerate() {
                        let mut e = map.reborrow().get(index as u32);
                        e.reborrow().set_key(key);
                        e.set_value(value);
                    }
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

/// The capnp ordinal of one method, read from the schema tree the mock
/// publishes.
///
/// The mock is CONTRACT-ONLY: it publishes the identity the SCHEMA declares,
/// never a constant written here. For a schema this build compiled, the
/// generated table has it; for the FIXTURE schema (which the CLI never
/// compiles), the tree the mock publishes carries it — the same tree a
/// client reads.
fn method_ordinal(interface: &str, method: &str) -> Option<u16> {
    if let Some(info) = smart_cli::contract::interface_named(interface) {
        if let Some(id) = smart_cli::contract::method_id(info.stem, info.name, method) {
            return Some(id);
        }
    }
    let tree = smart_cli::dynamic::SchemaTree::decode(&fixture_schema_bytes())?;
    tree.method(interface, method).map(|method| method.id)
}

/// The fixture schema's node tree, as bytes.
fn fixture_schema_bytes() -> Vec<u8> {
    const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fixture_schema.bin"));
    BYTES.to_vec()
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
    // The six families the engine now serves (spine S1): the paths the
    // service publishes, the identity the CONTRACT gives them.
    #[cfg(feature = "schema-files")]
    {
        let id = smart_cli::contract::schema_id("files").unwrap_or(0);
        for (path, method, summary) in [
            ("files ls", "listDirectory", "the directory's own entries"),
            ("files find", "searchFiles", "the paths matching a glob"),
            ("files tree", "walkTree", "the tree under a path"),
            ("files stat", "statFile", "one path's own metadata"),
            ("files xattrs", "getFileMetadata", "one path's extended metadata"),
            ("files cat", "readFile", "one file's bytes"),
            ("files write", "writeFile", "write bytes into a file"),
            ("files du", "getDiskUsage", "the disk usage of the filesystem a path lives on"),
            ("files locks", "getLocks", "the locks the host holds"),
            ("files hash", "getFileHash", "one file's content hash"),
            ("files versions", "getFileVersions", "one file's version trail"),
            ("files branches", "listBranches", "one file's branches"),
            ("files store", "storeContentAddressed", "store content by its address"),
            ("files dedupe", "deduplicate", "deduplicate a path's content"),
            ("files watch", "watchDirectory", "the directory's change events"),
            ("files special", "getSpecialPath", "the host's own special path"),
            ("files trash", "listTrash", "the host's own trash"),
            ("files recents", "getRecentFiles", "the host's own recent files"),
            ("files app", "getDefaultApp", "the default app for a type"),
            ("files grep", "searchFileContent", "search a tree's content"),
            ("files compare", "compareFiles", "compare two paths"),
            ("files health", "getFilesystemHealth", "the filesystem's own health"),
            ("files quota", "getQuota", "the quota on a path"),
            ("files mkdir", "createDirectory", "create a directory"),
            ("files touch", "touch", "touch a path"),
            ("files rm", "removeFile", "remove a file"),
            ("files mv", "renameFile", "rename a path"),
            ("files cp", "copyFile", "copy a path"),
            ("files link", "createSymlink", "create a symlink"),
            ("files lock", "lockFile", "lock a file"),
            ("files unlock", "unlockFile", "unlock a file"),
            ("files exists", "fileExists", "does a path exist"),
            ("files pwd", "getCurrentDirectory", "the host's working directory"),
            ("files size", "getDirectorySize", "the size of a directory"),
            ("files type", "getFileType", "one path's type"),
            ("files checksum", "getFileChecksum", "one file's checksum"),
            ("files open", "openFile", "open a file"),
            ("files close", "closeFile", "close a file"),
            ("files flush", "flushFile", "flush a file"),
            ("files create", "createFile", "create a file"),
            ("files rmdir", "removeDirectory", "remove a directory"),
            ("files batch copy", "batchCopy", "copy several paths"),
            ("files batch move", "batchMove", "move several paths"),
            ("files batch delete", "batchDelete", "delete several paths"),
            ("files quota set", "setQuota", "set the quota on a path"),
            ("files trash restore", "restoreTrash", "restore a trashed entry"),
            ("files trash empty", "emptyTrash", "empty the trash"),
            ("files recents clear", "clearRecentFiles", "clear the recent files"),
            ("files lifecycle", "getLifecycle", "one path's lifecycle policy"),
            ("files lifecycle set", "setLifecycle", "set one path's lifecycle policy"),
            ("files lifecycle apply", "applyLifecycle", "apply the lifecycle policy"),
            ("files version restore", "restoreVersion", "restore one file's version"),
            ("files compression set", "setCompression", "set one path's compression"),
            ("files compression", "getCompression", "one path's compression"),
            ("files decrypt", "decryptFile", "decrypt one file"),
            ("files branch merge", "mergeBranch", "merge a branch"),
            ("files snapshot", "createSnapshot", "snapshot a path"),
            ("files acquire", "acquireFile", "acquire the file service"),
        ] {
            commands.push(PublishedCommand {
                path,
                schema_id: id,
                interface: "Files",
                method,
                domain: "files",
                summary,
            });
        }
    }
    #[cfg(feature = "schema-process")]
    {
        let id = smart_cli::contract::schema_id("process").unwrap_or(0);
        for (path, method, summary) in [
            ("process list", "listProcesses", "the kernel's own enumeration"),
            ("process inspect", "getProcess", "one process's own reading"),
            ("process status", "getProcessStatus", "one process's state and exit"),
            ("process tree", "getProcessTree", "the tree under a pid"),
            ("process threads", "getThreads", "one process's threads"),
            ("process env", "getEnvironment", "one process's environment"),
            ("process maps", "getMemoryMap", "one process's memory map"),
            ("process limits", "getResourceLimits", "one process's declared quota"),
            ("process accounting", "getAccounting", "one process's accounting"),
            ("process groups", "listProcessGroups", "the kernel's own process groups"),
            ("process stats", "getStats", "the aggregate over the enumeration"),
            ("process context", "getExecutionContext", "one process's execution context"),
            ("process attach", "attach", "attach to a process"),
            ("process detach", "detach", "detach from a process"),
            ("process set-env", "setEnvironment", "set a process's environment"),
            ("process exit", "exitProcess", "ask a process to exit"),
            ("process step", "stepProcess", "step a process"),
            ("process continue", "continueProcess", "continue a process"),
            ("process respawn", "respawnProcess", "respawn a process"),
            ("process respawn-status", "getProcessRespawnStatus", "a process's respawn state"),
            ("process read-memory", "readMemory", "read a process's memory"),
            ("process write-memory", "writeMemory", "write a process's memory"),
            ("process set-limit", "setResourceLimit", "set a process's resource limit"),
            ("process set-context", "setExecutionContext", "set a process's execution context"),
            ("process supervise", "registerSupervisor", "register a supervisor"),
            ("process report-failure", "reportProcessFailure", "report a process's failure"),
            ("process watch", "subscribeProcess", "a process's own snapshot stream"),
            ("process watch-exit", "subscribeDeath", "a process's exit notification"),
        ] {
            commands.push(PublishedCommand {
                path,
                schema_id: id,
                interface: "Process",
                method,
                domain: "process",
                summary,
            });
        }
    }
    #[cfg(feature = "schema-settings")]
    {
        let id = smart_cli::contract::schema_id("settings").unwrap_or(0);
        for (path, method, summary) in [
            ("settings domains", "listDomains", "the schema source the host publishes"),
            ("settings list", "getAll", "one domain's live values"),
            ("settings get", "get", "one key's value"),
            ("settings set", "set", "set one key's value"),
            ("settings delete", "delete", "delete one key"),
            ("settings clear", "clear", "clear one domain"),
            ("settings version", "getVersion", "one key's version"),
            ("settings history", "getHistory", "one key's change trail"),
            ("settings diff", "getSettingsDiff", "the changes since a version"),
            ("settings category", "listByCategory", "the keys under a category"),
            ("settings bulk-get", "bulkGet", "read several keys at once"),
            ("settings bulk-set", "bulkSet", "write several keys at once"),
            ("settings set-app-override", "setAppOverride", "set an app's override"),
            ("settings get-app-override", "getAppOverride", "read an app's override"),
            ("settings clear-app-override", "clearAppOverride", "clear an app's override"),
            ("settings set-if-unchanged", "setIfUnchanged", "set a key if its version holds"),
            ("settings watch", "watch", "watch one key"),
            ("settings watch-domain", "watchDomain", "watch one domain"),
        ] {
            commands.push(PublishedCommand {
                path,
                schema_id: id,
                interface: "Settings",
                method,
                domain: "settings",
                summary,
            });
        }
    }
    #[cfg(feature = "schema-logging")]
    {
        let id = smart_cli::contract::schema_id("logging").unwrap_or(0);
        for (path, method, summary) in [
            ("logging sinks", "listDomains", "the sinks this host has, and each one's level"),
            ("logging domains", "listDomains", "the log domains this host serves"),
            ("logging tail", "getEntries", "one domain's own lines"),
            ("logging entries", "getEntries", "the retained lines"),
            ("logging emit", "log", "write one line"),
            ("logging level", "getLevel", "one domain's level"),
            ("logging set-level", "setLevel", "set one domain's level"),
            ("logging sampling", "setSamplingRate", "set one domain's sampling rate"),
            ("logging clear", "clear", "clear one domain's lines"),
            ("logging export", "export", "export the retained lines"),
            ("logging follow", "subscribe", "the live log stream"),
        ] {
            commands.push(PublishedCommand {
                path,
                schema_id: id,
                interface: "Logging",
                method,
                domain: "logging",
                summary,
            });
        }
    }
    #[cfg(feature = "schema-clock")]
    {
        let id = smart_cli::contract::schema_id("clock").unwrap_or(0);
        for (path, method, summary) in [
            ("clock info", "info", "the measured state of this host's clocks"),
            ("clock now", "realtimeNow", "the host's own CLOCK_REALTIME"),
            ("clock monotonic", "monotonicNow", "the host's own CLOCK_MONOTONIC"),
            ("clock uptime", "info", "how long the host has been up"),
            ("clock resolution", "listClockDomains", "each clock domain's resolution"),
            ("clock sync", "getClockSync", "the measured sync verdict"),
            ("clock security", "info", "the clock's own security state"),
            ("clock domains", "listClockDomains", "the clock domains this host serves"),
            ("clock quality", "getClockQuality", "the measured quality"),
            ("clock drift", "getClockDrift", "the measured drift"),
            ("clock timers", "listPersistedTimers", "the store's own persisted timers"),
            ("clock sleep", "sleep", "sleep on the caller's own thread"),
            ("clock sleep-cancellable", "sleepCancellable", "a cancellable sleep"),
            ("clock persist-timer", "persistTimer", "persist a timer"),
            ("clock set-timer-dependency", "setTimerDependency", "tie one timer to another"),
            ("clock alarms", "alarms", "acquire the alarm service"),
            ("clock watch", "subscribeTimers", "the timer event stream"),
        ] {
            commands.push(PublishedCommand {
                path,
                schema_id: id,
                interface: "ClockService",
                method,
                domain: "clock",
                summary,
            });
        }
    }
    #[cfg(feature = "schema-time")]
    {
        let id = smart_cli::contract::schema_id("time").unwrap_or(0);
        for (path, method, summary) in [
            ("time now", "getDateTime", "the host's own wall clock"),
            ("time cal", "convertCalendar", "the arithmetic calendars"),
            ("time add", "addCalendarUnits", "add units to a date"),
            ("time sub", "subtractCalendarUnits", "subtract units from a date"),
            ("time diff", "diffCalendarDates", "the difference between two dates"),
        ] {
            commands.push(PublishedCommand {
                path,
                schema_id: id,
                interface: "Time",
                method,
                domain: "time",
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
        params: smart_cli::schema::discovery_capnp::discovery::LookupParams,
        results: smart_cli::schema::discovery_capnp::discovery::LookupResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        // The lookup HONOURS the filter: this service publishes itself under
        // one name, and a caller that asks for another name gets an empty
        // answer (a discovery service that answered for a name nobody
        // published would be lying).
        let asked = params
            .get()
            .ok()
            .and_then(|params| params.get_filter().ok())
            .map(|filter| filter.get_name().map(|t| t.to_str().unwrap_or("").to_string()))
            .and_then(|name| name.ok())
            .unwrap_or_default();
        let name = if asked.is_empty() || asked == MOCK_SERVICE_NAME {
            MOCK_SERVICE_NAME.to_string()
        } else {
            asked
        };
        fill(&discovery_lookup_msg(&name, &self.cfg), results);
        std::future::ready(Ok(()))
    }

    fn resolve(
        self: Rc<Self>,
        params: smart_cli::schema::discovery_capnp::discovery::ResolveParams,
        results: smart_cli::schema::discovery_capnp::discovery::ResolveResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.cfg.state.requests.fetch_add(1, Ordering::SeqCst);
        let asked = params
            .get()
            .ok()
            .map(|params| params.get_name().map(|t| t.to_str().unwrap_or("").to_string()))
            .and_then(|name| name.ok())
            .unwrap_or_default();
        let name = if asked.is_empty() || asked == MOCK_SERVICE_NAME {
            MOCK_SERVICE_NAME.to_string()
        } else {
            asked
        };
        fill(&discovery_resolve_msg(&name, &self.cfg), results);
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

/// Write the open metadata of a descriptor or an endpoint: the CALL MODEL
/// this service speaks and the CARRIER it is reachable over.
///
/// The same keys, with the same meaning, as the service that publishes them
/// for real and the client that reads them.
#[cfg(feature = "schema-discovery")]
fn write_metadata(
    builder: &mut smart_cli::schema::types_capnp::string_map::Builder<'_>,
    entries: &[(&str, &str)],
) {
    let mut map = builder.reborrow().init_entries(entries.len() as u32);
    for (index, (key, value)) in entries.iter().enumerate() {
        let mut entry = map.reborrow().get(index as u32);
        entry.reborrow().set_key(key);
        entry.set_value(value);
    }
}

/// The descriptor the mock publishes: the name, the endpoint it is reachable
/// at, and the CALL MODEL it speaks.
#[cfg(feature = "schema-discovery")]
fn write_descriptor(
    descriptor: &mut smart_cli::schema::discovery_capnp::service_descriptor::Builder<'_>,
    name: &str,
    cfg: &Cfg,
) {
    descriptor.reborrow().set_name(name);
    descriptor.set_category("network");
    descriptor
        .reborrow()
        .init_id()
        .set_id(network_schema_id());
    let mut endpoints = descriptor.reborrow().init_endpoints(1);
    let mut endpoint = endpoints.reborrow().get(0);
    endpoint.set_transport(&cfg.carrier);
    endpoint.set_address(&cfg.address);
    write_metadata(
        &mut endpoint.reborrow().init_metadata(),
        &[
            (smart_cli::service::METADATA_CALL_MODEL, &cfg.call_model),
            (smart_cli::service::METADATA_CARRIER, &cfg.carrier),
        ],
    );
    write_metadata(
        &mut descriptor.reborrow().init_metadata(),
        &[(smart_cli::service::METADATA_CALL_MODEL, &cfg.call_model)],
    );
}

#[cfg(feature = "schema-discovery")]
fn discovery_lookup_msg(
    name: &str,
    cfg: &Cfg,
) -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::discovery_capnp::discovery as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::lookup_results::Builder<'_>>();
        // A name this service does not publish: an empty answer, honestly.
        if name != MOCK_SERVICE_NAME {
            r.set_total(0);
            return m;
        }
        let mut list = r.reborrow().init_services(1);
        let mut svc = list.reborrow().get(0);
        write_descriptor(&mut svc, name, cfg);
        r.set_total(1);
    }
    m
}

#[cfg(feature = "schema-discovery")]
fn discovery_resolve_msg(
    name: &str,
    cfg: &Cfg,
) -> capnp::message::Builder<capnp::message::HeapAllocator> {
    use smart_cli::schema::discovery_capnp::discovery as w;
    let mut m = capnp::message::Builder::new_default();
    {
        let mut r = m.init_root::<w::resolve_results::Builder<'_>>();
        if name != MOCK_SERVICE_NAME {
            r.set_present(false);
            return m;
        }
        let mut descriptor = r.reborrow().init_descriptor();
        write_descriptor(&mut descriptor, name, cfg);
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
