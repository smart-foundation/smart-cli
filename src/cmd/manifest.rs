use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::transport as tp;

use super::domain::{self, Presenter};
use super::App;

const STEM: &str = "manifest";
const INTERFACE: &str = "ManifestService";

pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != crate::contract::schema_id(STEM)? {
        return None;
    }
    // Generic fallback handles every published method against a service
    // that publishes its schema tree (the real engine does).
    let _ = method;
    None
}
