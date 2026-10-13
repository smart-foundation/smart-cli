#![cfg(feature = "schema-network")]
// Needs the network schema bindings (schema-network).
//! The zero-cost hot path (plan Part 6): JSON emission borrows the contract
//! readers and writes into a caller buffer with NO per-field heap allocation.
//! A counting global allocator asserts that emitting an object allocates
//! nothing beyond a pre-reserved output buffer — the "no heap growth per read
//! op" law, measured.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::SeqCst);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Build + serialize a get_status result with many populated fields.
fn status_bytes() -> Vec<u8> {
    use smart_cli::schema::network_capnp::network as w;
    let mut message = capnp::message::Builder::new_default();
    {
        let mut r = message.init_root::<w::get_status_results::Builder<'_>>();
        let mut s = r.reborrow().init_status();
        s.set_connected(true);
        s.reborrow().init_interface().set_id(1);
        s.reborrow().set_type("wifi");
        s.set_strength(-52);
        s.set_speed(866_000_000);
        s.set_quality(87);
        s.reborrow().set_host_state("managed");
        s.set_metered(true);
        let mut latency = s.reborrow().init_latency();
        latency.set_nanoseconds(12_000_000);
        let mut wifi = s.reborrow().init_wifi();
        wifi.reborrow().set_ssid("Mock-Network-with-a-long-ssid");
    }
    let mut out = Vec::new();
    capnp::serialize::write_message(&mut out, &message).unwrap();
    out
}

#[test]
fn json_emission_borrows_and_performs_no_per_field_allocation() {
    let bytes = status_bytes();
    // Parse (this allocates, but NOT in the window we measure).
    let reader = smart_cli::transport::decode_message::<
        smart_cli::schema::network_capnp::network::get_status_results::Owned,
    >(&bytes)
    .unwrap();
    let root = reader.get().unwrap();
    let status = root.get_status().unwrap();

    // A pre-reserved output buffer: growth would be the ONLY allowance.
    let mut out = String::with_capacity(8192);
    ALLOCS.store(0, Ordering::SeqCst);
    smart_cli::json::write_connection_status(&mut out, status);
    let allocations = ALLOCS.load(Ordering::SeqCst);

    assert_eq!(allocations, 0, "emitting JSON allocated {allocations} times (borrow law)");
    assert!(out.contains("\"connected\":true"));
    assert!(out.contains("\"hostState\":\"managed\""));
    assert!(out.contains("\"wifi\""));
    assert!(out.contains("Mock-Network-with-a-long-ssid"));
}
