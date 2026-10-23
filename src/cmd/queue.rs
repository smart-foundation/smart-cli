use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::domain::{self, Presenter};
use super::App;

const STEM: &str = "queue";
const INTERFACE: &str = "Queue";

pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != crate::contract::schema_id(STEM)? {
        return None;
    }
    let _ = method;
    None
}
