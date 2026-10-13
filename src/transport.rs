//! The client transport: connect to the service over a host socket, run the
//! ABI handshake, and speak the serve frame codec ([`crate::frame`]) over it.
//!
//! This is the CLI's OWN client of the contract's transports — it does not use
//! smart-lib's transport crate (the architecture law). The stream framing is
//! the contract's stream format: `u32be total | kind:u8 | payload`, where the
//! length never counts itself, and a serve frame rides as the single segment
//! of a one-segment capnp message inside a KIND_DATA frame.
//!
//! ```text
//! frame := u32be total_len | kind:u8 | payload
//! kind 0x01 DATA  payload = one capnp flat message
//! ```
//!
//! The connect sequence (per knowledge/05 §1.1): connect -> handshake ->
//! calls. The handshake is the contract's own `AbiHandshakeRequest`, whose
//! `matched` answer names the served schema id — so a caller can see exactly
//! which contract it reached.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

use capnp::message::ReaderOptions;

use crate::frame::{self, ResponseView};
use crate::schema::{abi_capnp, types_capnp};

/// Stream frame kind: a capnp data (or handshake) message.
const KIND_DATA: u8 = 0x01;

/// Declared maximum frame size in bytes (matches the service's 64 MiB bound).
const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// The default socket path, overridable by `$SMART_SERVICE_SOCKET`.
pub const DEFAULT_SOCKET: &str = "/tmp/smart-service.sock";

/// The default RPC timeout applied to every blocking read.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where to connect: the `--connect <path>` / `--addr host:port` / default law.
#[derive(Clone, Debug)]
pub enum Target {
    /// A unix socket path (a leading NUL denotes the Linux abstract namespace).
    Unix(String),
    /// A `host:port` TCP address.
    Tcp(String),
}

/// A concrete byte stream (unix or tcp); names no platform in a required API.
///
/// The non-unix `Unix` variant exists only so the type is constructible on a
/// target without AF_UNIX; [`Connection::connect`] refuses a unix target
/// there, so the variant is never built and its Read/Write stubs never run.
#[cfg_attr(not(unix), allow(dead_code))]
enum Stream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(not(unix))]
    Unix(core::marker::PhantomData<()>),
    Tcp(std::net::TcpStream),
}

impl Stream {
    fn set_read_timeout(&self, dur: Option<Duration>) {
        match self {
            #[cfg(unix)]
            Stream::Unix(s) => {
                let _ = s.set_read_timeout(dur);
            }
            #[cfg(not(unix))]
            Stream::Unix(_) => {}
            Stream::Tcp(s) => {
                let _ = s.set_read_timeout(dur);
            }
        }
    }
}

impl std::io::Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buf),
            #[cfg(not(unix))]
            Stream::Unix(_) => Ok(0),
            Stream::Tcp(s) => s.read(buf),
        }
    }
}
impl std::io::Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Stream::Unix(s) => s.write(buf),
            #[cfg(not(unix))]
            Stream::Unix(_) => Ok(buf.len()),
            Stream::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Stream::Unix(s) => s.flush(),
            #[cfg(not(unix))]
            Stream::Unix(_) => Ok(()),
            Stream::Tcp(s) => s.flush(),
        }
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

/// A decode of the contract `T.Error` a service error frame carried.
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
    /// The frame transport itself failed (io/framing/deadline).
    Transport(String),
    /// The service answered an error frame carrying the contract's own `T.Error`.
    Service(ServiceError),
}

impl core::fmt::Display for CallError {
    /// The one-line form the human error path prints.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CallError::Transport(detail) => write!(f, "transport: {detail}"),
            CallError::Service(error) => write!(f, "{}: {}", json_error_token(&error.code), error.message),
        }
    }
}

/// One connected, handshaken session to the service.
pub struct Connection {
    stream: Stream,
    /// The declared transport (for `--verbose`): "unix:/path" / "tcp:host:port".
    label: String,
    /// The matched ABI namespace (surfaced by `--verbose`).
    namespace: String,
    /// The served schema id the service announced (if any).
    schema_ids: Vec<u64>,
    timeout: Duration,
}

impl Connection {
    /// Connect and run the ABI handshake in one step.
    pub fn connect(target: &Target) -> Result<Self, CallError> {
        let (stream, label) = match target {
            #[cfg(unix)]
            Target::Unix(path) => {
                let stream = std::os::unix::net::UnixStream::connect(path).map_err(|e| {
                    CallError::Transport(format!("connect unix {path} failed: {e}"))
                })?;
                (Stream::Unix(stream), format!("unix:{path}"))
            }
            #[cfg(not(unix))]
            Target::Unix(_path) => {
                // On a target with no AF_UNIX (Windows; macOS HAS AF_UNIX, so
                // this arm is Windows-only) the unix transport cannot exist.
                // The honest answer names the alternative rather than
                // pretending a service is absent.
                return Err(CallError::Transport(
                    "this target has no AF_UNIX socket support, so the default unix transport \
                     cannot be used: select the TCP transport with `--addr host:port` or `@host:port`"
                        .into(),
                ));
            }
            Target::Tcp(addr) => {
                let stream = std::net::TcpStream::connect(addr)
                    .map_err(|e| CallError::Transport(format!("connect tcp {addr} failed: {e}")))?;
                (Stream::Tcp(stream), format!("tcp:{addr}"))
            }
        };
        let mut conn = Connection {
            stream,
            label,
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

    /// Override the per-call read timeout (the `5` exit code).
    pub fn set_timeout(&mut self, dur: Duration) {
        self.timeout = dur;
    }

    /// The peer's host identity where the transport carries one (informational:
    /// the SERVICE decides the capability class, not the client).
    pub fn peer_identity(&self) -> Option<(i32, u32, u32)> {
        None
    }

    /// The ABI handshake: send `AbiHandshakeRequest`, read `AbiHandshakeResponse`.
    fn handshake(&mut self) -> Result<(), CallError> {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut request = message.init_root::<abi_capnp::abi_handshake_request::Builder<'_>>();
            let mut abi = request.reborrow().init_caller_abi();
            abi.set_namespace("smart-protocol");
        }
        let payload = encode_message(&message);
        self.write_frame(KIND_DATA, &payload)?;
        let (_, response_payload) = self.read_frame()?;

        let reader = decode_message::<abi_capnp::abi_handshake_response::Owned>(&response_payload)
            .map_err(CallError::Transport)?;
        let response = reader.get().map_err(|e| {
            CallError::Transport(format!("handshake response has no root: {e}"))
        })?;
        match response.which() {
            Ok(abi_capnp::abi_handshake_response::Which::Matched(abi)) => {
                let abi = abi.map_err(|e| {
                    CallError::Transport(format!("handshake matched arm is empty: {e}"))
                })?;
                self.namespace = abi
                    .get_namespace()
                    .map(|t| t.to_str().unwrap_or("smart-protocol").to_string())
                    .unwrap_or_else(|_| "smart-protocol".to_string());
                for schema in abi.get_schema_versions().into_iter().flatten() {
                    self.schema_ids.push(schema.get_id());
                }
                Ok(())
            }
            Ok(abi_capnp::abi_handshake_response::Which::PartialMatch(_)) => {
                // A partial match still proceeds; the wire is honest per call.
                self.namespace = "smart-protocol".to_string();
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
                    "this service cannot speak the smart-protocol contract: {message}"
                )))
            }
            Ok(abi_capnp::abi_handshake_response::Which::Unknown(_))
            | Err(_) => Err(CallError::Transport("handshake answered unknown".into())),
        }
    }

    /// Issue one serve call and return the ok payload bytes, or the decoded
    /// service/transport error.
    pub fn call(
        &mut self,
        interface_id: u64,
        ordinal: u16,
        params: &[u8],
    ) -> Result<Vec<u8>, CallError> {
        let frame = frame::encode_request(interface_id, ordinal, params, &[]);
        self.send_serve_frame(&frame)
    }

    /// Pull one subscription's next event (bounded). An empty ok payload is a
    /// real answer ("nothing yet"), never a fabricated event.
    pub fn pull_subscription(&mut self, sub_id: u64, limit: u32) -> Result<Vec<u8>, CallError> {
        let params = frame::encode_pull_params(sub_id, limit);
        let frame = frame::encode_request(0, frame::SUB_PULL, &params, &[]);
        self.send_serve_frame(&frame)
    }

    /// Retire a subscription.
    pub fn cancel_subscription(&mut self, sub_id: u64) -> Result<(), CallError> {
        let params = frame::encode_cancel_params(sub_id);
        let frame = frame::encode_request(0, frame::SUB_CANCEL, &params, &[]);
        self.send_serve_frame(&frame).map(|_| ())
    }

    /// Wrap a serve frame as a one-segment capnp message, send it, read the
    /// response, and lift the serve frame back out.
    fn send_serve_frame(&mut self, serve_frame: &[u8]) -> Result<Vec<u8>, CallError> {
        let payload = frame::wrap_one_segment(serve_frame);
        self.write_frame(KIND_DATA, &payload)?;
        let (_, response_payload) = self.read_frame()?;
        let segment = frame::segment0(&response_payload)
            .ok_or_else(|| CallError::Transport("response is not a one-segment serve frame".into()))?;
        let view: ResponseView<'_> =
            frame::parse_response(segment).map_err(CallError::Transport)?;
        if view.is_ok() {
            Ok(view.payload.to_vec())
        } else {
            Err(CallError::Service(decode_error(view)))
        }
    }

    /// Write one stream frame: `u32be total | kind | payload` (length never
    /// counts itself).
    fn write_frame(&mut self, kind: u8, payload: &[u8]) -> Result<(), CallError> {
        use std::io::Write;
        let total = 1u32
            .checked_add(payload.len() as u32)
            .ok_or_else(|| CallError::Transport("frame too large".into()))?;
        if total > MAX_FRAME_BYTES {
            return Err(CallError::Transport("frame exceeds declared maximum".into()));
        }
        let mut buf = Vec::with_capacity(4 + total as usize);
        buf.extend_from_slice(&total.to_be_bytes());
        buf.push(kind);
        buf.extend_from_slice(payload);
        self.stream.write_all(&buf).map_err(transport_io)?;
        self.stream.flush().map_err(transport_io)
    }

    /// Read one stream frame, returning `(kind, payload)`.
    fn read_frame(&mut self) -> Result<(u8, Vec<u8>), CallError> {
        
        self.stream
            .set_read_timeout(Some(self.timeout));
        let mut len_buf = [0u8; 4];
        self.read_exact_timeout(&mut len_buf)?;
        let total = u32::from_be_bytes(len_buf);
        if total == 0 || total > MAX_FRAME_BYTES {
            return Err(CallError::Transport(format!(
                "declared frame length {total} out of range"
            )));
        }
        let mut payload = alloc::vec![0u8; total as usize];
        self.read_exact_timeout(&mut payload)?;
        Ok((payload[0], payload[1..].to_vec()))
    }

    /// `read_exact` with read-deadline awareness (a deadline maps to the
    /// timeout the exit-code table publishes as 5).
    fn read_exact_timeout(&mut self, buf: &mut [u8]) -> Result<(), CallError> {
        use std::io::Read;
        let deadline = std::time::Instant::now() + self.timeout;
        let mut read = 0usize;
        while read < buf.len() {
            match self.stream.read(&mut buf[read..]) {
                Ok(0) => {
                    return Err(CallError::Transport(
                        "peer closed the connection".to_string(),
                    ))
                }
                Ok(n) => read += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(CallError::Service(ServiceError {
                            code: types_capnp::ErrorCode::Timeout,
                            message: "the service did not answer within the timeout".to_string(),
                            context: String::new(),
                        }));
                    }
                }
                Err(e) => return Err(transport_io(e)),
            }
        }
        Ok(())
    }
}

/// The stable token for an error code (shared with the JSON `error` object).
fn json_error_token(code: &types_capnp::ErrorCode) -> &'static str {
    crate::json::error_code(*code)
}

/// Decode a service error frame's `T.Error` payload into a [`ServiceError`].
fn decode_error(view: ResponseView<'_>) -> ServiceError {
    let fallback = ServiceError {
        code: types_capnp::ErrorCode::Failed,
        message: view.message_text(),
        context: String::new(),
    };
    if view.payload.is_empty() {
        return ServiceError {
            code: types_capnp::ErrorCode::Failed,
            message: view.message_text(),
            context: String::new(),
        };
    }
    let Ok(typed) = decode_message::<types_capnp::error::Owned>(view.payload) else {
        return fallback;
    };
    let Ok(reader) = typed.get() else {
        return fallback;
    };
    let code = reader.get_code().unwrap_or(types_capnp::ErrorCode::Failed);
    ServiceError {
        code,
        message: reader.get_message().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default(),
        context: reader.get_context().map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default(),
    }
}

/// Map an I/O error into a transport-level call error.
fn transport_io(e: std::io::Error) -> CallError {
    CallError::Transport(e.to_string())
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
pub fn decode_message<T>(bytes: &[u8]) -> Result<capnp::message::TypedReader<capnp::serialize::OwnedSegments, T>, String>
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

    /// A full in-process loopback pair speaking the same framing, so the
    /// transport codec is proven without a daemon (charter law #8: mock only).
    #[test]
    fn stream_frame_length_counts_kind_plus_payload_not_itself() {
        // 4-byte length == 1 (kind) + payload length, big-endian.
        let payload = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let total = 1 + payload.len() as u32;
        let mut frame = Vec::new();
        frame.extend_from_slice(&total.to_be_bytes());
        frame.push(KIND_DATA);
        frame.extend_from_slice(&payload);
        // The first four bytes read back as total = 9.
        assert_eq!(u32::from_be_bytes(frame[0..4].try_into().unwrap()), 9);
    }

    #[test]
    fn handshake_message_is_a_capnp_abi_request() {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut request = message.init_root::<abi_capnp::abi_handshake_request::Builder<'_>>();
            request.reborrow().init_caller_abi().set_namespace("smart-protocol");
        }
        let bytes = encode_message(&message);
        // Round-trips as an AbiHandshakeRequest naming the contract family.
        let reader = decode_message::<abi_capnp::abi_handshake_request::Owned>(&bytes).unwrap();
        let request = reader.get().unwrap();
        let ns = request.get_caller_abi().unwrap().get_namespace().unwrap();
        assert_eq!(ns.to_str().unwrap(), "smart-protocol");
    }

    #[test]
    fn default_paths_are_sane() {
        assert_eq!(DEFAULT_SOCKET, "/tmp/smart-service.sock");
        let _ = DEFAULT_TIMEOUT;
    }
}
