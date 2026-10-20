//! smart-cli — the network-domain client of the smart-protocol contract.
//!
//! ## The architecture law (the founder's correction), obeyed here
//!
//! This crate is a **capnp client of the contract and nothing else**. It does
//! not know an implementation crate: there is no path dependency, no re-export, no link. The
//! only dependency in the graph is the contract — the `capnp` runtime plus the
//! bindings `build.rs` generates from `../smart-protocol-official/schema`
//! (read-only). `cargo tree` here names capnp/capnpc/smart-cli only; the
//! Cargo.lock carries no an implementation crate entry.
//!
//! ## AI-first (the CLI is one client among many)
//!
//! The library core — [`transport`], [`rpc`], [`json`], [`selector`],
//! [`watch`] plus the generated [`schema`] bindings — is exactly the surface an
//! agent uses to acquire a capability and drive the domain with **no shell in
//! the loop**. The [`cmd`] modules add parsing and rendering and add NO
//! capability; they are compiled only behind `cmd-*` features. The headless
//! proof (`examples/agent_client.rs`) links the core with zero `cmd` code.
//!
//! ## Zero-cost / blazing (charter law #2, Part 6 of the plan)
//!
//! A command family's schema compiles only when its `cmd-*`/`schema-*` feature
//! is on. JSON emission borrows the contract readers (no per-field allocation
//! — asserted by the counting-allocator test). Watch pulls are bounded and
//! drops are counted, never silent. The CLI adds no call at startup beyond the
//! registry read.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// ── The contract, generated at build time from the canonical schemas ──
include!(concat!(env!("OUT_DIR"), "/contract.rs"));
include!(concat!(env!("OUT_DIR"), "/schema_modules.rs"));

// ── Client core (no_std + alloc unless noted): the AI-first surface ──
pub mod json; // contract-shaped JSON: keys verbatim, values from the readers
// Runtime contract introspection: a schema the CLI never compiled. The
// generic path talks to a live service, so it is host I/O (std) like the
// transport and the RPC runtime.
#[cfg(feature = "std")]
pub mod dynamic;
pub mod present; // the presenter framework: cells, widths, box tables, glyphs
pub mod selector; // `--where field op value` with schema-generated validation
pub mod watch; // bounded subscription pulls with drop counting
pub mod registry; // the exit-code table and the availability honesty helpers
pub mod render; // dashboards/tables/bars over the SAME objects JSON emits
#[cfg(any(
    feature = "cmd-net",
    feature = "cmd-dns",
    feature = "cmd-bt",
    feature = "cmd-tunnel",
    feature = "cmd-web",
    feature = "cmd-socket",
    feature = "cmd-discovery"
))]
pub mod profile; // the CLI-local state file (TOML subset; no contract route)
// Shell completions are GENERATED from the registry table the help screen
// reads (plan Part 4.3: one registry drives help, completion, and honesty).
#[cfg(any(
    feature = "cmd-net",
    feature = "cmd-dns",
    feature = "cmd-bt",
    feature = "cmd-tunnel",
    feature = "cmd-web",
    feature = "cmd-socket",
    feature = "cmd-discovery"
))]
pub mod completions;

// The socket transports and the RPC runtime are host I/O (std); the JSON
// emitters, the selector and the watch accounting above stay no_std.
#[cfg(feature = "std")]
pub mod rpc; // the contract's own call model: capnp RPC over a byte stream
#[cfg(feature = "std")]
pub mod transport;
#[cfg(feature = "std")]
pub mod remote;
// The universal path: discovery → endpoint → ABI → call, and the authority
// question the client asks the service itself (what does it hold?).
#[cfg(feature = "std")]
pub mod service;

// ── The command surface: parsing + rendering, no added capability ──
#[cfg(any(
    feature = "cmd-net",
    feature = "cmd-dns",
    feature = "cmd-bt",
    feature = "cmd-tunnel",
    feature = "cmd-discovery",
    feature = "cmd-web",
    feature = "cmd-socket"
))]
pub mod cmd;

/// Crate name used by the architecture-law CI grep (no "an implementation crate" anywhere).
pub const CRATE_NAME: &str = "smart-cli";
