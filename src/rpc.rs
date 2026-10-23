//! The contract's own call model: **capnp RPC** over a transport's byte
//! stream.
//!
//! # What is on the wire
//!
//! capnp RPC's own messages (`rpc.capnp`), serialized by capnp's own
//! serializer. Nothing else. There is no second vocabulary here: no
//! fixed-offset envelope, no hand-written kind byte, no ordinal table in the
//! source. The payload of a call is the **contract's own params struct**; the
//! payload of a return is the contract's own results struct.
//!
//! ```text
//!   transport byte stream ──▶ capnp RPC (rpc.capnp) ──▶ interface id + method id
//!                                  │
//!                                  └──▶ the contract's params / results structs
//! ```
//!
//! # The pump: why the socket gets its own threads
//!
//! capnp RPC reads its messages through an [`AsyncRead`] and writes them
//! through an [`AsyncWrite`]. The tree's transports are *blocking* sockets.
//! Adapting them with a blocking adapter would deadlock: the RPC runtime
//! awaits the next inbound message inside its connection task, and while it
//! waits there, the task that must write the answer never gets polled.
//!
//! So the read half is pumped by its own thread — the thread blocks on the
//! socket (that is what a socket is for) and hands what it read to the
//! executor through a channel. The write half is pumped the same way, so the
//! executor thread never blocks on a full socket buffer either.
//!
//! # One call path
//!
//! [`Session::call`] takes an **interface id** and a **method id** and returns
//! the results struct's bytes. The typed fast path and the generic path (a
//! schema this CLI never compiled) are the same call: the generic path only
//! differs in *where the ids came from*.

use std::future::Future;
use std::io::{Read, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use capnp::message::ReaderOptions;
use capnp::private::capability::ClientHook;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty;
use capnp_rpc::RpcSystem;
use futures::channel::{mpsc, oneshot};
use futures::executor::LocalPool;
use futures::io::{AsyncRead, AsyncWrite};
use futures::task::LocalSpawnExt;
use futures::StreamExt;

use crate::transport::{encode_message, CallError};

/// How much the reader thread pulls from the socket at a time.
const READ_CHUNK: usize = 64 * 1024;

/// A transport's byte stream, pumped into the async world.
pub struct PumpedStream {
    reader: ReaderPump,
    writer: WriterPump,
}

impl PumpedStream {
    /// Pump a transport's byte stream: the read half and the write half.
    pub fn new(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        Self {
            reader: ReaderPump::spawn(reader),
            writer: WriterPump::spawn(writer),
        }
    }

    /// Both halves, for the RPC runtime (which wants them separately).
    pub fn split(self) -> (ReaderPump, WriterPump) {
        (self.reader, self.writer)
    }
}

/// The read half: a thread that blocks on the socket, an executor side that
/// never blocks.
pub struct ReaderPump {
    receiver: mpsc::UnboundedReceiver<std::io::Result<Vec<u8>>>,
    chunk: Vec<u8>,
    offset: usize,
    failure: Option<std::io::Error>,
}

impl ReaderPump {
    /// Start the reader thread.
    fn spawn(mut stream: Box<dyn Read + Send>) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        std::thread::Builder::new()
            .name("smart-cli-rpc-reader".to_string())
            .spawn(move || {
                let mut buffer = vec![0u8; READ_CHUNK];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            let chunk = buffer[..count].to_vec();
                            if sender.unbounded_send(Ok(chunk)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = sender.unbounded_send(Err(error));
                            break;
                        }
                    }
                }
            })
            .expect("the reader thread is the only thing that can fail to start here");
        Self {
            receiver,
            chunk: Vec::new(),
            offset: 0,
            failure: None,
        }
    }
}

impl AsyncRead for ReaderPump {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        loop {
            if this.offset < this.chunk.len() {
                let count = std::cmp::min(buf.len(), this.chunk.len() - this.offset);
                buf[..count].copy_from_slice(&this.chunk[this.offset..this.offset + count]);
                this.offset += count;
                return Poll::Ready(Ok(count));
            }
            if let Some(error) = this.failure.take() {
                return Poll::Ready(Err(error));
            }
            match this.receiver.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    this.chunk = chunk;
                    this.offset = 0;
                }
                Poll::Ready(Some(Err(error))) => this.failure = Some(error),
                Poll::Ready(None) => return Poll::Ready(Ok(0)),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// What the writer thread is asked to do.
enum WriteJob {
    /// These bytes go to the socket, in order.
    Data(Vec<u8>),
    /// Everything written so far must be on the socket; answer when it is.
    Flush(oneshot::Sender<std::io::Result<()>>),
}

/// The write half: an executor side that queues, a thread that writes.
pub struct WriterPump {
    sender: mpsc::UnboundedSender<WriteJob>,
    pending: Option<oneshot::Receiver<std::io::Result<()>>>,
}

impl WriterPump {
    /// Start the writer thread.
    fn spawn(mut stream: Box<dyn Write + Send>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded::<WriteJob>();
        std::thread::Builder::new()
            .name("smart-cli-rpc-writer".to_string())
            .spawn(move || {
                // The writer thread runs its own mini-executor: the RPC
                // executor never blocks on the socket.
                while let Some(job) = futures::executor::block_on(receiver.next()) {
                    match job {
                        WriteJob::Data(bytes) => {
                            if stream.write_all(&bytes).is_err() {
                                break;
                            }
                        }
                        WriteJob::Flush(answer) => {
                            let outcome = stream.flush();
                            let failed = outcome.is_err();
                            let _ = answer.send(outcome);
                            if failed {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("the writer thread is the only thing that can fail to start here");
        Self {
            sender,
            pending: None,
        }
    }
}

impl AsyncWrite for WriterPump {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let _ = cx;
        // Order matters: a write must not overtake a flush that is still
        // outstanding.
        if this.pending.is_some() {
            match this.poll_pending(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
        }
        let count = buf.len();
        this.sender
            .unbounded_send(WriteJob::Data(buf.to_vec()))
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the write pump is closed")
            })?;
        Poll::Ready(Ok(count))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.pending.is_none() {
            let (answer, receipt) = oneshot::channel();
            this.sender
                .unbounded_send(WriteJob::Flush(answer))
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the write pump is closed")
                })?;
            this.pending = Some(receipt);
        }
        this.poll_pending(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}

impl Unpin for WriterPump {}

impl WriterPump {
    /// Await the outstanding flush, if there is one.
    fn poll_pending(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let Some(receipt) = self.pending.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match Pin::new(receipt).poll(cx) {
            Poll::Ready(Ok(outcome)) => {
                self.pending = None;
                Poll::Ready(outcome)
            }
            Poll::Ready(Err(_)) => {
                self.pending = None;
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the write pump is closed",
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A client session: the contract's own call model, from the caller's side.
///
/// The session owns the executor: a caller drives its calls with
/// [`Session::call`] (or [`Session::run`] for a future of its own), and the
/// RPC system runs underneath.
pub struct Session {
    pool: LocalPool,
    bootstrap: Box<dyn ClientHook>,
    /// How to release the socket deterministically (see
    /// [`crate::transport::Stream::closer`]).
    closer: Option<Box<dyn Fn() + Send>>,
}

impl Session {
    /// Connect to a peer over a byte stream and take its bootstrap
    /// capability.
    ///
    /// The bootstrap capability is the peer's *root* capability: any contract
    /// interface the peer serves is reachable through it, because the
    /// interface id travels with the call — exactly as the contract's
    /// generated code says.
    pub fn connect(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        closer: Option<Box<dyn Fn() + Send>>,
    ) -> Result<Self, CallError> {
        let (reader, writer) = PumpedStream::new(reader, writer).split();
        let network: Box<dyn capnp_rpc::VatNetwork<twoparty::VatId>> = Box::new(
            twoparty::VatNetwork::new(reader, writer, Side::Client, ReaderOptions::new()),
        );
        let mut rpc_system: RpcSystem<twoparty::VatId> = RpcSystem::new(network, None);
        let bootstrap = rpc_system.bootstrap::<Box<dyn ClientHook>>(Side::Server);
        let pool = LocalPool::new();
        let spawner = pool.spawner();
        spawner
            .spawn_local(async move {
                let _ = rpc_system.await;
            })
            .map_err(|error| {
                CallError::Transport(format!("cannot start the RPC session: {error:?}"))
            })?;
        Ok(Self {
            pool,
            bootstrap,
            closer,
        })
    }

    /// Release the session NOW: the socket is shut down, so the peer sees the
    /// session end and the reader thread unblocks.
    ///
    /// Dropping a session is not enough: the read half is pumped by a thread
    /// that blocks on the socket and still holds the descriptor.
    pub fn close(&mut self) {
        if let Some(closer) = self.closer.take() {
            closer();
        }
    }

    /// The peer's bootstrap capability, as a capnp client.
    pub fn client(&self) -> capnp::capability::Client {
        capnp::capability::Client::new(self.bootstrap.clone())
    }

    /// Run a future to completion on this session's executor.
    pub fn run<T>(&mut self, future: impl std::future::Future<Output = T>) -> T {
        self.pool.run_until(future)
    }

    /// One contract call: the interface id and the method id travel with the
    /// call, the params are the contract's own struct, and the answer is the
    /// contract's own results struct.
    ///
    /// `params` is the serialized params struct (the root of a capnp
    /// message); the answer is the serialized results struct, in the same
    /// shape. An empty `params` means "no params struct" — the contract's
    /// zero-parameter methods still take one, and the caller passes the empty
    /// struct it built.
    pub fn call(
        &mut self,
        interface_id: u64,
        method_id: u16,
        params: &[u8],
    ) -> Result<Vec<u8>, CallError> {
        let client = self.client();
        self.call_capability_within(
            &client,
            interface_id,
            method_id,
            params,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        )
    }

    /// The same call, with a deadline.
    pub fn call_within(
        &mut self,
        interface_id: u64,
        method_id: u16,
        params: &[u8],
        deadline: std::time::Instant,
    ) -> Result<Vec<u8>, CallError> {
        let client = self.client();
        self.call_capability_within(&client, interface_id, method_id, params, deadline)
    }

    /// A call on a **capability** the contract handed us (a subscription), by
    /// the contract's own interface and method ids.
    pub fn call_capability_within(
        &mut self,
        client: &capnp::capability::Client,
        interface_id: u64,
        method_id: u16,
        params: &[u8],
        deadline: std::time::Instant,
    ) -> Result<Vec<u8>, CallError> {
        let mut request =
            client.new_call::<capnp::any_pointer::Owned, capnp::any_pointer::Owned>(
                interface_id,
                method_id,
                None,
            );
        if !params.is_empty() {
            let reader = crate::transport::decode_message::<capnp::any_pointer::Owned>(params)
                .map_err(CallError::Transport)?;
            let root = reader
                .get()
                .map_err(|e| CallError::Transport(format!("the params have no root: {e}")))?;
            request.set(root).map_err(|e| {
                CallError::Transport(format!("the params could not be set: {e}"))
            })?;
        }
        let response: capnp::capability::Response<capnp::any_pointer::Owned> = self
            .run_until(request.send().promise, deadline)?;
        let results = response
            .get()
            .map_err(|e| CallError::Transport(format!("the results have no root: {e}")))?;
        // Lift the results struct back into a message of its own, so the
        // reader that follows owns the bytes it reads.
        let mut message = capnp::message::Builder::new_default();
        {
            let mut root = message.init_root::<capnp::any_pointer::Builder<'_>>();
            root.set_as(results).map_err(|e| {
                CallError::Transport(format!("the results could not be lifted: {e}"))
            })?;
        }
        Ok(encode_message(&message))
    }

    /// A **typed** call: the params and the results are the contract's own
    /// generated structs, and the answer keeps its capabilities (a
    /// subscription). The byte path cannot carry a capability — a capability
    /// does not survive serialization — so a subscribe takes this path.
    pub fn call_typed_within<P, R>(
        &mut self,
        interface_id: u64,
        method_id: u16,
        params: P::Reader<'_>,
        deadline: std::time::Instant,
    ) -> Result<capnp::capability::Response<R>, CallError>
    where
        P: capnp::traits::Owned,
        R: capnp::traits::Owned + capnp::traits::Pipelined + Unpin + 'static,
        <R as capnp::traits::Pipelined>::Pipeline: capnp::capability::FromTypelessPipeline,
    {
        let client = self.client();
        let mut request = client.new_call::<P, R>(interface_id, method_id, None);
        request
            .set(params)
            .map_err(|e| CallError::Transport(format!("the params could not be set: {e}")))?;
        self.run_until(request.send().promise, deadline)
    }

    /// Run a future to completion on this session's executor, under a
    /// deadline.
    fn run_until<T>(
        &mut self,
        future: impl std::future::Future<Output = Result<T, capnp::Error>>,
        deadline: std::time::Instant,
    ) -> Result<T, CallError> {
        let guard = Deadline::new(deadline);
        let mut future = std::pin::pin!(future);
        let outcome = self.pool.run_until(std::future::poll_fn(move |cx| {
            guard.remember(cx.waker());
            if guard.expired() {
                return std::task::Poll::Ready(Err(capnp::Error::failed(
                    "the call exceeded its deadline".to_string(),
                )));
            }
            future.as_mut().poll(cx)
        }));
        outcome.map_err(call_error)
    }
}

/// A deadline the executor honours while a call is in flight.
///
/// The executor has no timer of its own, so a small thread watches the
/// deadline and wakes the task when it passes: the call is then abandoned
/// with the contract's own timeout answer, never left hanging.
struct Deadline {
    at: std::time::Instant,
    fired: Arc<std::sync::atomic::AtomicBool>,
    done: Arc<std::sync::atomic::AtomicBool>,
    waker: Arc<std::sync::Mutex<Option<std::task::Waker>>>,
}

impl Deadline {
    /// Start watching `at`.
    fn new(at: std::time::Instant) -> Self {
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waker: Arc<std::sync::Mutex<Option<std::task::Waker>>> =
            Arc::new(std::sync::Mutex::new(None));
        let thread_fired = Arc::clone(&fired);
        let thread_done = Arc::clone(&done);
        let thread_waker = Arc::clone(&waker);
        std::thread::Builder::new()
            .name("smart-cli-rpc-deadline".to_string())
            .spawn(move || loop {
                if thread_done.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                if std::time::Instant::now() >= at {
                    thread_fired.store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(waker) = thread_waker.lock().ok().and_then(|mut w| w.take()) {
                        waker.wake();
                    }
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            })
            .ok();
        Self {
            at,
            fired,
            done,
            waker,
        }
    }

    /// Remember the task's waker, so the deadline can wake it.
    fn remember(&self, waker: &std::task::Waker) {
        if let Ok(mut slot) = self.waker.lock() {
            *slot = Some(waker.clone());
        }
    }

    /// True once the deadline has passed.
    fn expired(&self) -> bool {
        self.fired.load(std::sync::atomic::Ordering::SeqCst)
            || std::time::Instant::now() >= self.at
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A capnp RPC failure, read back as the contract's own vocabulary.
///
/// capnp's error channel carries a coarse kind plus a description. The
/// contract's error model is richer, so a peer that speaks the contract puts
/// the contract's code token first (`notSupported: …`). A description that
/// does not start with a known token falls back to the capnp kind — never to
/// a fabricated code.
fn call_error(error: capnp::Error) -> CallError {
    let text = error.extra.as_str();
    let text = text
        .strip_prefix("remote exception: ")
        .unwrap_or(text);
    let (token, rest) = match text.split_once(": ") {
        Some((head, tail)) => (head, tail),
        None => (text, ""),
    };
    match crate::transport::code_from_token(token) {
        Some(code) => CallError::Service(crate::transport::ServiceError {
            code,
            message: rest.to_string(),
            context: String::new(),
        }),
        None => {
            // A method the peer does not implement is the contract's
            // `notSupported` (the published table's row 3: "unavailable with a
            // reason"), not a generic failure — the peer is not broken, it
            // simply does not offer that method here.
            if error.kind == capnp::ErrorKind::Unimplemented {
                return CallError::Service(crate::transport::ServiceError {
                    code: smart_protocol::schema::types_capnp::ErrorCode::NotSupported,
                    message: text.to_string(),
                    context: String::new(),
                });
            }
            CallError::Transport(format!("{text} ({:?})", error.kind))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_channel_reads_the_contract_code_token_first() {
        let error = capnp::Error {
            kind: capnp::ErrorKind::Unimplemented,
            extra: "remote exception: notSupported: no-backend: dns".to_string(),
        };
        match call_error(error) {
            CallError::Service(service) => {
                assert_eq!(service.code, smart_protocol::schema::types_capnp::ErrorCode::NotSupported);
                assert_eq!(service.message, "no-backend: dns");
            }
            other => panic!("expected a service error, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_token_falls_back_to_the_capnp_kind() {
        let error = capnp::Error {
            kind: capnp::ErrorKind::Overloaded,
            extra: "the peer is busy".to_string(),
        };
        match call_error(error) {
            CallError::Transport(detail) => assert!(detail.contains("the peer is busy")),
            other => panic!("expected a transport error, got {other:?}"),
        }
    }
}
