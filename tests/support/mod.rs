// Shared test support: the mock service (speaks the contract's own call
// model) and the headless scenario. Every CLI integration test drives the real
// `net` binary against the mock over a unix socket — no daemon, no live host
// (law #8).

#[allow(dead_code)]
pub mod mock;
/// The headless scenario is network-schema shaped; a build without
/// `schema-network` (the cmd-dns isolation config) does not compile it.
#[cfg(feature = "schema-network")]
#[allow(dead_code, unused_imports)]
pub mod agent_scenario;

/// The test-local structural JSON validator (no JSON crate: the only
/// dependency of this crate is the contract).
pub mod json;

/// The test-local JSON SHAPE helper (the key paths a document carries).
pub mod shape;

/// The FIXTURE schema's generated bindings — for the TEST SUPPORT only.
///
/// This is the proof of the generic path: the CLI's LIBRARY has no binding for
/// this schema (its build never puts it into `crate::schema`), so the CLI can
/// only reach it through the contract's own introspection. The mock compiles
/// it for itself, the way a real service compiles its own contract.
#[allow(dead_code, unused_imports, clippy::all)]
pub mod fixture {
    #[allow(dead_code, unused_imports, clippy::all)]
    pub mod fixture_capnp {
        include!(concat!(env!("OUT_DIR"), "/fixture_capnp.rs"));
    }
}
