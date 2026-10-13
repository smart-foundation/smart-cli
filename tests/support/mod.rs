// Shared test support: the mock service (speaks the frame protocol) and the
// headless scenario. Every CLI integration test drives the real `net` binary
// against the mock over a unix socket — no daemon, no live host (law #8).

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
