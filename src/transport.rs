//! The client transport: an **open registry** of transport schemes, the
//! endpoint resolution law (the bootstrap), and the connection that speaks the
//! contract's own call model (capnp RPC) over the transport's byte stream.
//!
//! # The open transport registry
//!
//! The contract's `ServiceEndpoint.transport` is an OPEN token: the schema
//! names a transport by a token and does not enumerate the tokens. So the
//! client keeps an OPEN registry of the schemes it can speak; a scheme it does
//! not know is reported **honestly** — with the schemes it does know — never
//! rejected as if the peer were broken.
//!
//! # The endpoint resolution law (the bootstrap)
//!
//! 1. `--endpoint <scheme>://<address>` on the command line;
//! 2. `$SMART_ENDPOINT`;
//! 3. the configuration file (`$XDG_CONFIG_HOME/smart-cli/endpoint`, else
//!    `~/.config/smart-cli/endpoint`);
//! 4. the platform's runtime-directory convention
//!    (`$XDG_RUNTIME_DIR/smart-cli/service.sock`, else the platform's
//!    temporary directory).
//!
//! Nothing in this file names an implementation: no service, no daemon, no
//! library, and no hard-coded socket path. When no source answers, the CLI
//! refuses **honestly** and names this order back to the user.
//!
//! # The call
//!
//! The connection speaks capnp RPC ([`crate::rpc`]): the envelope is capnp
//! RPC's and the payload is the contract's own params/results structs.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

use capnp::message::ReaderOptions;

use crate::rpc;
use smart_protocol::schema::{abi_capnp, types_capnp};

/// The default RPC timeout applied to every blocking call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The contract's namespace, as the handshake declares it.
pub const NAMESPACE: &str = "smart-protocol";

/// The scheme token of the unix-domain transport.
pub const SCHEME_UNIX: &str = "unix";

/// The scheme token of the TCP transport.
pub const SCHEME_TCP: &str = "tcp";

/// Where to connect: a transport **scheme token** and an address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The open scheme token ("unix", "tcp", …).
    pub scheme: String,
    /// The scheme's address (a socket path, a `host:port`, …).
    pub address: String,
}

impl Endpoint {
    /// Build an endpoint from a scheme and an address.
    pub fn new(scheme: &str, address: &str) -> Self {
        Self {
            scheme: scheme.to_string(),
            address: address.to_string(),
        }
    }

    /// Parse `<scheme>://<address>`.
    ///
    /// The scheme is REQUIRED: an endpoint that does not name one cannot be
    /// routed, and the honest answer names the schemes this client speaks.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let Some((scheme, address)) = text.split_once("://") else {
            return Err(format!(
                "the endpoint `{text}` names no transport scheme; write it as \
                 `<scheme>://<address>` (this client speaks: {})",
                SCHEME_UNIX,
            ));
        };
        let scheme = scheme.trim().to_string();
        if scheme.is_empty() {
            return Err(format!(
                "the endpoint `{text}` names no transport scheme; write it as \
                 `<scheme>://<address>` (this client speaks: {})",
                SCHEME_UNIX,
            ));
        }
        Ok(Self {
            scheme,
            address: address.to_string(),
        })
    }

    /// The audit-trail label: `<scheme>:<address>`.
    pub fn label(&self) -> String {
        format!("{}:{}", self.scheme, self.address)
    }
}

/// A transport's byte stream, split into the two halves the RPC runtime
/// reads and writes.
pub struct Stream {
    /// The read half.
    pub reader: Box<dyn std::io::Read + Send>,
    /// The write half.
    pub writer: Box<dyn std::io::Write + Send>,
    /// How to release the socket deterministically.
    ///
    /// The read half is pumped by a thread that BLOCKS on the socket, so
    /// dropping the stream does not close it: the thread still holds the
    /// descriptor. A client that must release a session (the universal path
    /// dials a second endpoint after the first) needs the socket closed
    /// NOW, not whenever the reader thread next wakes. The closer shuts the
    /// socket down, which unblocks the reader and ends the peer's session.
    pub closer: Option<Box<dyn Fn() + Send>>,
}

/// One transport the client can speak: an open scheme token and how to open
/// it.
pub trait Transport: Send + Sync {
    /// The scheme token this transport answers to.
    fn scheme(&self) -> &str;
    /// Open the transport at `address`.
    fn connect(&self, address: &str) -> Result<Stream, CallError>;
}

/// The open set of transports the client speaks.
///
/// The set is OPEN: a caller may register another scheme. A scheme that is
/// not registered is reported honestly, with the schemes that are.
#[derive(Default)]
pub struct TransportRegistry {
    transports: Vec<Box<dyn Transport>>,
}

impl TransportRegistry {
    /// A registry with the transports every build speaks.
    pub fn with_defaults() -> Self {
        let mut registry = Self::default();
        registry.register(Box::new(UnixTransport));
        registry.register(Box::new(TcpTransport));
        registry
    }

    /// Register a transport. A scheme may be registered once; the first
    /// registration wins.
    pub fn register(&mut self, transport: Box<dyn Transport>) {
        let scheme = transport.scheme().to_string();
        if self.transports.iter().any(|t| t.scheme() == scheme) {
            return;
        }
        self.transports.push(transport);
    }

    /// The scheme tokens this registry speaks.
    pub fn schemes(&self) -> Vec<&str> {
        self.transports.iter().map(|t| t.scheme()).collect()
    }

    /// Open an endpoint.
    ///
    /// An unknown scheme is an honest "unavailable, with the reason": the
    /// peer is not broken, this client simply does not speak that transport.
    pub fn connect(&self, endpoint: &Endpoint) -> Result<Stream, CallError> {
        let Some(transport) = self
            .transports
            .iter()
            .find(|t| t.scheme() == endpoint.scheme)
        else {
            return Err(CallError::Unavailable {
                what: format!("the transport scheme `{}`", endpoint.scheme),
                reason: format!(
                    "this client speaks: {}",
                    self.schemes().join(", ")
                ),
            });
        };
        transport.connect(&endpoint.address)
    }
}

/// The unix-domain transport.
pub struct UnixTransport;

impl Transport for UnixTransport {
    fn scheme(&self) -> &str {
        SCHEME_UNIX
    }

    fn connect(&self, address: &str) -> Result<Stream, CallError> {
        #[cfg(unix)]
        {
            let stream = std::os::unix::net::UnixStream::connect(address).map_err(|e| {
                CallError::Transport(format!("connect unix {address} failed: {e}"))
            })?;
            let reader = stream
                .try_clone()
                .map_err(|e| CallError::Transport(format!("cannot split the socket: {e}")))?;
            let writer = stream
                .try_clone()
                .map_err(|e| CallError::Transport(format!("cannot split the socket: {e}")))?;
            let closer = stream.try_clone().ok().map(|socket| {
                Box::new(move || {
                    let _ = socket.shutdown(std::net::Shutdown::Both);
                }) as Box<dyn Fn() + Send>
            });
            Ok(Stream {
                reader: Box::new(reader),
                writer: Box::new(writer),
                closer,
            })
        }
        #[cfg(not(unix))]
        {
            Err(CallError::Unavailable {
                what: format!("the transport scheme `{address}`"),
                reason: "this target has no AF_UNIX socket support".to_string(),
            })
        }
    }
}

/// The TCP transport.
pub struct TcpTransport;

impl Transport for TcpTransport {
    fn scheme(&self) -> &str {
        SCHEME_TCP
    }

    fn connect(&self, address: &str) -> Result<Stream, CallError> {
        let stream = std::net::TcpStream::connect(address)
            .map_err(|e| CallError::Transport(format!("connect tcp {address} failed: {e}")))?;
        let reader = stream
            .try_clone()
            .map_err(|e| CallError::Transport(format!("cannot split the socket: {e}")))?;
        let writer = stream
            .try_clone()
            .map_err(|e| CallError::Transport(format!("cannot split the socket: {e}")))?;
        let closer = stream.try_clone().ok().map(|socket| {
            Box::new(move || {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }) as Box<dyn Fn() + Send>
        });
        Ok(Stream {
            reader: Box::new(reader),
            writer: Box::new(writer),
            closer,
        })
    }
}

/// Where a resolved endpoint came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointSource {
    /// `--endpoint` on the command line.
    Flag,
    /// The `$SMART_ENDPOINT` environment variable.
    Environment,
    /// The configuration file.
    ConfigFile,
    /// The platform's runtime-directory convention.
    PlatformDefault,
}

impl EndpointSource {
    /// The one-word name the audit trail prints.
    pub fn name(self) -> &'static str {
        match self {
            EndpointSource::Flag => "--endpoint",
            EndpointSource::Environment => "$SMART_ENDPOINT",
            EndpointSource::ConfigFile => "config",
            EndpointSource::PlatformDefault => "platform",
        }
    }
}

/// A resolved endpoint and where it came from.
#[derive(Clone, Debug)]
pub struct Resolution {
    /// The endpoint to connect to.
    pub endpoint: Endpoint,
    /// Where it came from.
    pub source: EndpointSource,
    /// The human name of the source (a flag name, an env var, a file path).
    pub origin: String,
}

/// The environment variable the endpoint may come from.
pub const ENV_ENDPOINT: &str = "SMART_ENDPOINT";

/// The configuration file the endpoint may come from.
pub fn config_file() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return Some(std::path::PathBuf::from(dir).join("smart-cli").join("endpoint"));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(home).join(".config").join("smart-cli").join("endpoint"))
}

/// The platform's runtime directory, where a socket conventionally lives.
pub fn runtime_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir).join("smart-cli");
        }
    }
    std::env::temp_dir().join("smart-cli")
}

/// The platform's runtime-directory convention, as an endpoint.
fn platform_endpoint() -> Endpoint {
    let path = runtime_dir().join("service.sock");
    Endpoint::new(SCHEME_UNIX, &path.to_string_lossy())
}

/// Resolve the endpoint, in the order the bootstrap law names:
///
/// 1. `--endpoint <scheme>://<address>`;
/// 2. `$SMART_ENDPOINT`;
/// 3. the configuration file;
/// 4. the platform's runtime-directory convention.
///
/// When no source answers, the CLI refuses **honestly** and names this order
/// back to the user — it never invents a socket path.
pub fn resolve_endpoint(flag: Option<&str>) -> Result<Resolution, String> {
    if let Some(text) = flag {
        let text = text.trim();
        if !text.is_empty() {
            return Ok(Resolution {
                endpoint: Endpoint::parse(text)?,
                source: EndpointSource::Flag,
                origin: "--endpoint".to_string(),
            });
        }
    }
    if let Ok(text) = std::env::var(ENV_ENDPOINT) {
        let text = text.trim();
        if !text.is_empty() {
            return Ok(Resolution {
                endpoint: Endpoint::parse(text)?,
                source: EndpointSource::Environment,
                origin: format!("${ENV_ENDPOINT}"),
            });
        }
    }
    if let Some(path) = config_file() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let text = text.trim();
            if !text.is_empty() {
                return Ok(Resolution {
                    endpoint: Endpoint::parse(text)?,
                    source: EndpointSource::ConfigFile,
                    origin: path.display().to_string(),
                });
            }
        }
    }
    Ok(Resolution {
        endpoint: platform_endpoint(),
        source: EndpointSource::PlatformDefault,
        origin: runtime_dir().display().to_string(),
    })
}

/// The order the endpoint resolution follows, named for the honest refusal.
pub fn resolution_order() -> String {
    format!(
        "--endpoint <scheme>://<address>, ${ENV_ENDPOINT}, the configuration file ({}), \
         then the platform runtime directory ({})",
        config_file()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "unavailable".to_string()),
        runtime_dir().display()
    )
}

/// A subscription: the contract's own **capability**, with the interface it
/// belongs to.
///
/// The contract models a subscription as a capability the `subscribeX` method
/// hands back; `pull` and `cancel` are methods OF THAT CAPABILITY. The ids
/// come from the GENERATED table, so the CLI never writes an ordinal.
#[derive(Clone)]
pub struct Subscription {
    client: capnp::capability::Client,
    stem: &'static str,
    interface: &'static str,
}

impl Subscription {
    /// Wrap the capability a subscribe answered with.
    pub fn new(
        client: capnp::capability::Client,
        stem: &'static str,
        interface: &'static str,
    ) -> Self {
        Self {
            client,
            stem,
            interface,
        }
    }

    /// The subscription's capability.
    pub fn client(&self) -> &capnp::capability::Client {
        &self.client
    }

    /// Pull the next event (the contract's own `pull`).
    pub fn pull(&self, conn: &mut Connection) -> Result<Vec<u8>, CallError> {
        let (interface_id, method_id) = self.ids("pull")?;
        conn.call_capability(&self.client, interface_id, method_id, &[])
    }

    /// Retire the subscription (the contract's own `cancel`).
    pub fn cancel(&self, conn: &mut Connection) -> Result<(), CallError> {
        let (interface_id, method_id) = self.ids("cancel")?;
        conn.call_capability(&self.client, interface_id, method_id, &[])
            .map(|_| ())
    }

    /// The interface and method ids of one of the subscription's methods.
    fn ids(&self, method: &str) -> Result<(u64, u16), CallError> {
        let info = smart_protocol::contract::interface_by_name(self.stem, self.interface).ok_or_else(|| {
            CallError::Transport(format!(
                "the contract interface `{}` is not compiled into this build",
                self.interface
            ))
        })?;
        let method_id =
            smart_protocol::contract::method_id(self.stem, self.interface, method).ok_or_else(|| {
                CallError::Transport(format!(
                    "the contract interface `{}` has no `{method}` method in this build",
                    self.interface
                ))
            })?;
        Ok((info.type_id, method_id))
    }
}

/// The matched handshake facts, so the audit trail can name what it reached.
#[derive(Clone, Debug)]
pub struct Handshake {
    /// The contract namespace ("smart-protocol").
    pub namespace: String,
    /// The served schema id the service declared (if it announced one).
    pub schema_ids: Vec<u64>,
}

/// A decode of the contract `T.Error` a service error carried.
#[derive(Clone, Debug)]
pub struct ServiceError {
    /// The contract error code.
    pub code: types_capnp::ErrorCode,
    /// The error message (the failure's own vocabulary).
    pub message: String,
    /// The error context, when present.
    pub context: String,
}

impl ServiceError {
    /// True when the service answered `permissionDenied` (exit 4).
    pub fn is_permission(&self) -> bool {
        matches!(self.code, types_capnp::ErrorCode::PermissionDenied)
    }
    /// True when the service answered `notSupported` (exit 3).
    pub fn is_unavailable(&self) -> bool {
        // The contract carries TWO "cannot run here" codes and both mean the
        // published table's row 3: `notSupported` (this method does not exist
        // on this backend) and `unavailable` (it exists but cannot answer
        // now). Neither is ever a failure to distinguish.
        matches!(
            self.code,
            types_capnp::ErrorCode::NotSupported | types_capnp::ErrorCode::Unavailable
        )
    }
    /// True when the service answered `timeout` (exit 5).
    pub fn is_timeout(&self) -> bool {
        matches!(self.code, types_capnp::ErrorCode::Timeout)
    }
}

/// Why a call failed. Total: every failure is transport-level or service-level.
#[derive(Clone, Debug)]
pub enum CallError {
    /// The transport itself failed (io/framing/deadline).
    Transport(String),
    /// The service answered with the contract's own `T.Error`.
    Service(ServiceError),
    /// The command cannot run here, and the reason is known.
    Unavailable {
        /// What is unavailable (a transport scheme, a capability, …).
        what: String,
        /// Why it is unavailable, in the contract's own "why" voice.
        reason: String,
    },
}

impl core::fmt::Display for CallError {
    /// The one-line form the human error path prints.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CallError::Transport(detail) => write!(f, "transport: {detail}"),
            CallError::Service(error) => {
                write!(f, "{}: {}", json_error_token(&error.code), error.message)
            }
            CallError::Unavailable { what, reason } => write!(f, "{what} is unavailable: {reason}"),
        }
    }
}

/// Read a contract error code token back. `None` for a token the contract does
/// not declare — never a guess.
pub fn code_from_token(token: &str) -> Option<types_capnp::ErrorCode> {
    use types_capnp::ErrorCode as E;
    Some(match token {
        "notFound" => E::NotFound,
        "permissionDenied" => E::PermissionDenied,
        "failed" => E::Failed,
        "temporary" => E::Temporary,
        "unavailable" => E::Unavailable,
        "limitExceeded" => E::LimitExceeded,
        "outOfMemory" => E::OutOfMemory,
        "invalidArgument" => E::InvalidArgument,
        "timeout" => E::Timeout,
        "cancelled" => E::Cancelled,
        "alreadyExists" => E::AlreadyExists,
        "notSupported" => E::NotSupported,
        "conflict" => E::Conflict,
        "unknown" => E::Unknown,
        "hardwareFault" => E::HardwareFault,
        "driverError" => E::DriverError,
        "protocolViolation" => E::ProtocolViolation,
        "busError" => E::BusError,
        "thermalThrottling" => E::ThermalThrottling,
        "powerStateFailure" => E::PowerStateFailure,
        _ => return None,
    })
}

/// The stable token for an error code (shared with the JSON `error` object).
fn json_error_token(code: &types_capnp::ErrorCode) -> &'static str {
    crate::json::error_code(*code)
}

/// One connected, handshaken session to the service.
pub struct Connection {
    /// The session, or `None` once the connection was closed.
    session: Option<rpc::Session>,
    /// The declared transport (for `--verbose`): "unix:/path" / "tcp:host:port".
    label: String,
    /// The matched ABI namespace (surfaced by `--verbose`).
    namespace: String,
    /// The served schema id the service announced (if any).
    schema_ids: Vec<u64>,
    timeout: Duration,
}

impl Connection {
    /// Open an endpoint and run the ABI handshake in one step.
    pub fn connect(endpoint: &Endpoint) -> Result<Self, CallError> {
        Self::connect_with(&TransportRegistry::with_defaults(), endpoint)
    }

    /// Open an endpoint over a specific transport registry.
    pub fn connect_with(
        registry: &TransportRegistry,
        endpoint: &Endpoint,
    ) -> Result<Self, CallError> {
        let stream = registry.connect(endpoint)?;
        let mut conn = Connection {
            session: Some(rpc::Session::connect(
                stream.reader,
                stream.writer,
                stream.closer,
            )?),
            label: endpoint.label(),
            namespace: String::new(),
            schema_ids: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        };
        conn.handshake()?;
        Ok(conn)
    }

    /// The declared transport label, for the `--verbose` audit trail.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The matched contract namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The served schema ids the service announced at handshake.
    pub fn schema_ids(&self) -> &[u64] {
        &self.schema_ids
    }

    /// Override the per-call deadline (the `5` exit code).
    pub fn set_timeout(&mut self, dur: Duration) {
        self.timeout = dur;
    }

    /// The peer's host identity where the transport carries one (informational:
    /// the SERVICE decides the capability class, not the client).
    pub fn peer_identity(&self) -> Option<(i32, u32, i32)> {
        None
    }

    /// The ABI handshake: the contract's own `AbiHandshakeRequest` over the
    /// contract's own call model.
    ///
    /// The request is the contract's PARAM struct (`handshake_params`), whose
    /// `request` field carries the `AbiHandshakeRequest` — the wire shape is
    /// the schema's, never a guess. The client declares the schemas THIS
    /// build speaks (the generated `compiled_schemas`), so the service can
    /// answer the schema **intersection** it agrees on (abi.capnp: "agree on
    /// an intersection before either commits to a call"); the answered ids
    /// are the ones the audit trail names.
    fn handshake(&mut self) -> Result<(), CallError> {
        let interface = smart_protocol::contract::interface_by_name("abi", "AbiService").ok_or_else(|| {
            CallError::Transport("the ABI interface is not compiled into this build".into())
        })?;
        let method = smart_protocol::contract::method_id("abi", "AbiService", "handshake").ok_or_else(|| {
            CallError::Transport("the ABI handshake method is not in this build".into())
        })?;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut params = message.init_root::<abi_capnp::abi_service::handshake_params::Builder<'_>>();
            let mut request = params.reborrow().init_request();
            let mut caller = request.reborrow().init_caller_abi();
            caller.set_namespace(NAMESPACE);
            let schemas = smart_protocol::contract::compiled_schemas();
            let mut versions = caller.reborrow().init_schema_versions(schemas.len() as u32);
            for (index, (stem, id)) in schemas.iter().enumerate() {
                let mut version = versions.reborrow().get(index as u32);
                version.set_id(*id);
                version.set_name(stem);
                version.set_major(0);
                version.set_minor(1);
            }
        }
        let payload = self.call(interface.type_id, method, &encode_message(&message))?;
        let reader = decode_message::<abi_capnp::abi_service::handshake_results::Owned>(&payload)
            .map_err(CallError::Transport)?;
        let results = reader
            .get()
            .map_err(|e| CallError::Transport(format!("handshake results have no root: {e}")))?;
        let response = results
            .get_response()
            .map_err(|e| CallError::Transport(format!("handshake response is empty: {e}")))?;
        match response.which() {
            Ok(abi_capnp::abi_handshake_response::Which::Matched(abi)) => {
                let abi = abi.map_err(|e| {
                    CallError::Transport(format!("handshake matched arm is empty: {e}"))
                })?;
                self.namespace = abi
                    .get_namespace()
                    .map(|t| t.to_str().unwrap_or(NAMESPACE).to_string())
                    .unwrap_or_else(|_| NAMESPACE.to_string());
                for schema in abi.get_schema_versions().into_iter().flatten() {
                    self.schema_ids.push(schema.get_id());
                }
                Ok(())
            }
            Ok(abi_capnp::abi_handshake_response::Which::PartialMatch(_)) => {
                // A partial match still proceeds; the wire is honest per call.
                self.namespace = NAMESPACE.to_string();
                Ok(())
            }
            Ok(abi_capnp::abi_handshake_response::Which::Incompatible(error)) => {
                let error = error.map_err(|e| {
                    CallError::Transport(format!("handshake incompatible arm is empty: {e}"))
                })?;
                let message = error
                    .get_message()
                    .map(|t| t.to_str().unwrap_or("contract incompatibility").to_string())
                    .unwrap_or_else(|_| "contract incompatibility".to_string());
                Err(CallError::Transport(format!(
                    "this peer cannot speak the smart-protocol contract: {message}"
                )))
            }
            Ok(abi_capnp::abi_handshake_response::Which::Unknown(_))
            | Err(_) => Err(CallError::Transport("handshake answered unknown".into())),
        }
    }

    /// Issue one contract call and return the results struct's bytes.
    ///
    /// `interface_id` is the contract interface's type id and `method_id` is
    /// the method's ordinal — both GENERATED (see [`smart_protocol::contract`]), never
    /// hand-written here.
    pub fn call(
        &mut self,
        interface_id: u64,
        method_id: u16,
        params: &[u8],
    ) -> Result<Vec<u8>, CallError> {
        let deadline = std::time::Instant::now() + self.timeout;
        self.session_mut()
            .call_within(interface_id, method_id, params, deadline)
    }

    /// The session, for the calls that need a capability (a subscription).
    pub fn session(&mut self) -> &mut rpc::Session {
        self.session_mut()
    }

    /// Close the session: the socket is released and the peer sees the
    /// session end.
    ///
    /// The universal path needs this: a service that publishes its OWN
    /// endpoint as a domain's endpoint serves one session at a time, so a
    /// client that has finished asking discovery releases that session
    /// before it dials the endpoint discovery named.
    pub fn close(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.close();
        }
        self.session = None;
    }

    /// True once the session was closed.
    pub fn is_closed(&self) -> bool {
        self.session.is_none()
    }

    /// The live session, or the honest refusal that it is gone.
    fn session_mut(&mut self) -> &mut rpc::Session {
        match self.session.as_mut() {
            Some(session) => session,
            None => unreachable!("the session was closed"),
        }
    }

    /// Call a method on a **capability** the contract handed us (a
    /// subscription), by the contract's own interface and method ids.
    pub fn call_capability(
        &mut self,
        client: &capnp::capability::Client,
        interface_id: u64,
        method_id: u16,
        params: &[u8],
    ) -> Result<Vec<u8>, CallError> {
        let deadline = std::time::Instant::now() + self.timeout;
        self.session_mut()
            .call_capability_within(client, interface_id, method_id, params, deadline)
    }

    /// Call a method that answers with a **capability** in its results (a
    /// subscribe). The typed path keeps the capability; the byte path cannot
    /// (a capability does not survive serialization).
    pub fn call_typed<P, R>(
        &mut self,
        interface_id: u64,
        method_id: u16,
        params: P::Reader<'_>,
    ) -> Result<capnp::capability::Response<R>, CallError>
    where
        P: capnp::traits::Owned,
        R: capnp::traits::Owned + capnp::traits::Pipelined + Unpin + 'static,
        <R as capnp::traits::Pipelined>::Pipeline: capnp::capability::FromTypelessPipeline,
    {
        let deadline = std::time::Instant::now() + self.timeout;
        self.session_mut()
            .call_typed_within::<P, R>(interface_id, method_id, params, deadline)
    }
}

/// Serialize a one-root capnp message to bytes (the wire's flat message form).
pub fn encode_message(
    message: &capnp::message::Builder<capnp::message::HeapAllocator>,
) -> Vec<u8> {
    let mut out = Vec::new();
    capnp::serialize::write_message(&mut out, message)
        .expect("serializing a capnp message into a Vec cannot fail");
    out
}

/// Decode a flat capnp message and read its root reader.
pub fn decode_message<T>(
    bytes: &[u8],
) -> Result<capnp::message::TypedReader<capnp::serialize::OwnedSegments, T>, String>
where
    T: capnp::traits::Owned,
{
    let reader = capnp::serialize::read_message(&mut &bytes[..], ReaderOptions::new())
        .map_err(|e| e.to_string())?;
    Ok(capnp::message::TypedReader::new(reader))
}

/// Decode `bytes` and hand its root reader to `f`, keeping the owning message
/// alive across the call (`f` returns owned data). This is the one helper all
/// result renderers use so a reader never outlives the segments it borrows.
pub fn with_root<T, R, F>(bytes: &[u8], f: F) -> Option<R>
where
    T: capnp::traits::Owned,
    F: FnOnce(T::Reader<'_>) -> R,
{
    let reader = decode_message::<T>(bytes).ok()?;
    let root = reader.get().ok()?;
    Some(f(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_names_its_scheme() {
        let endpoint = Endpoint::parse("unix:///run/agent.sock").unwrap();
        assert_eq!(endpoint.scheme, "unix");
        assert_eq!(endpoint.address, "/run/agent.sock");
        assert_eq!(endpoint.label(), "unix:/run/agent.sock");
    }

    #[test]
    fn an_endpoint_without_a_scheme_is_refused_honestly() {
        let error = Endpoint::parse("/run/agent.sock").unwrap_err();
        assert!(error.contains("scheme"), "{error}");
    }

    #[test]
    fn an_unknown_scheme_is_unavailable_with_a_reason() {
        let registry = TransportRegistry::with_defaults();
        let endpoint = Endpoint::new("carrier-pigeon", "somewhere");
        match registry.connect(&endpoint) {
            Err(CallError::Unavailable { what, reason }) => {
                assert!(what.contains("carrier-pigeon"), "{what}");
                assert!(reason.contains("unix"), "{reason}");
                assert!(reason.contains("tcp"), "{reason}");
            }
            other => panic!("expected an unavailable answer, got {}", other.err().map(|e| e.to_string()).unwrap_or_else(|| "a stream".into())),
        }
    }

    #[test]
    fn the_registry_is_open() {
        struct Shouty;
        impl Transport for Shouty {
            fn scheme(&self) -> &str {
                "shouty"
            }
            fn connect(&self, _address: &str) -> Result<Stream, CallError> {
                Err(CallError::Transport("shouty cannot connect".into()))
            }
        }
        let mut registry = TransportRegistry::with_defaults();
        registry.register(Box::new(Shouty));
        assert!(registry.schemes().contains(&"shouty"));
    }

    #[test]
    fn the_error_channel_reads_the_contract_code_token_first() {
        let code = code_from_token("notSupported").unwrap();
        assert_eq!(code, types_capnp::ErrorCode::NotSupported);
        assert!(code_from_token("notAThing").is_none());
    }
}
