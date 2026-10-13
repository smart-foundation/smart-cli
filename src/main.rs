//! net — the smart-protocol network client. A thin parsing + rendering shell
//! over the contract; it adds NO capability (the AI-first surface is the
//! library core + `examples/agent_client.rs`, with this shell absent).
//!
//! Exit codes are the published table in [`smart_cli::registry`]: 0 ok, 1 call
//! failed, 2 usage, 3 unavailable-with-reason, 4 permission, 5 timeout.

use std::process::ExitCode;

use smart_cli::cmd;
use smart_cli::remote;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let overrides = remote::parse_overrides(&argv);
    let code = cmd::run(&overrides);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
