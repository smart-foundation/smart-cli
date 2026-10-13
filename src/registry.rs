//! The registry and the honesty law — the exit-code table, and the per-host
//! availability read that makes `help` list nothing that cannot run.
//!
//! ## The exit-code table (grammar law #7, published in `net --help`)
//!
//! | code | meaning |
//! |---|---|
//! | 0 | ok |
//! | 1 | the call failed (the contract `T.Error` kind is in the JSON `error`) |
//! | 2 | usage (bad grammar / unknown flag / bad selector field) |
//! | 3 | unavailable-with-reason (the command exists, this host cannot run it) |
//! | 4 | permission (capability not held — `permissionDenied`) |
//! | 5 | timeout |
//!
//! ## The honesty law (grammar law #8)
//!
//! `help` lists nothing that cannot run on this host. Availability comes from
//! the service's own answer through cli.capnp's registry (`listUnavailable`
//! @13) — a registered command that is not currently runnable, with the reason
//! it cannot run and why. A command that is not runnable is HIDDEN from help
//! and, if invoked, answers exit `3` with the reason printed and in the JSON
//! `error` object. `net registry list-unavailable` shows the hidden ones.
//!
//! When the service publishes no registry (a bare network daemon answers
//! `unknown-interface` for cli.capnp), the CLI is HONEST that it cannot verify
//! availability: it lists the grammar it implements and says so in `--verbose`,
//! and every command still enforces the service's answer at call time. It
//! never fakes a "WiFi enabled" the way the old CLI did (the named anti-pattern).

use alloc::format;
#[cfg_attr(not(feature = "std"), allow(unused_imports))]
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[cfg(feature = "std")]
use crate::schema::cli_capnp::cli_service;
#[cfg(feature = "std")]
use crate::transport::{CallError, Connection};

pub const EXIT_OK: i32 = 0;
pub const EXIT_CALL_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_UNAVAILABLE: i32 = 3;
pub const EXIT_PERMISSION: i32 = 4;
pub const EXIT_TIMEOUT: i32 = 5;

/// The exit-code table, as the lines `net --help` prints.
pub const EXIT_TABLE: &[(i32, &str)] = &[
    (EXIT_OK, "ok"),
    (EXIT_CALL_FAILED, "the call failed (see the JSON error object)"),
    (EXIT_USAGE, "usage: bad grammar, unknown flag, or bad selector field"),
    (EXIT_UNAVAILABLE, "unavailable with a reason on this host"),
    (EXIT_PERMISSION, "permission denied: the capability is not held"),
    (EXIT_TIMEOUT, "the service did not answer in time"),
];

/// The exit code for a completed call, per the table. Total over `CallError`.
#[cfg(feature = "std")]
pub fn exit_code_for(result: &Result<Vec<u8>, CallError>) -> i32 {
    match result {
        Ok(_) => EXIT_OK,
        Err(CallError::Service(error)) => {
            if error.is_permission() {
                EXIT_PERMISSION
            } else if error.is_unavailable() {
                EXIT_UNAVAILABLE
            } else if error.is_timeout() {
                EXIT_TIMEOUT
            } else {
                EXIT_CALL_FAILED
            }
        }
        Err(CallError::Transport(_)) => EXIT_CALL_FAILED,
    }
}

/// One registered command the service reports as not currently runnable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    /// The command path the user types (e.g. "wifi connect").
    pub path: String,
    /// Why it cannot run, as a stable class token + the service's reason text.
    pub reason_class: String,
    /// The service's human reason (empty when it declares none).
    pub reason: String,
}

impl Unavailable {
    /// The single line `net registry list-unavailable` prints.
    pub fn line(&self) -> String {
        if self.reason.is_empty() {
            format!("{}: {}", self.path, self.reason_class)
        } else {
            format!("{}: {} — {}", self.path, self.reason_class, self.reason)
        }
    }
}

/// Whether the service publishes a registry we could read. A bare network
/// daemon does not serve cli.capnp, so the CLI is honest that it cannot verify
/// availability rather than pretending.
#[derive(Clone, Debug)]
pub enum Availability {
    /// The service published which commands cannot run right now.
    Published(Vec<Unavailable>),
    /// The service does not expose a registry (unknown-interface). The CLI
    /// lists what it implements and says so; each command still enforces at
    /// call time.
    Unpublished,
}

#[cfg(feature = "std")]
/// Read the service's unavailable-command list through cli.capnp `listUnavailable`
/// @13. This is the ONLY startup query, and only for help/registry views — the
/// CLI adds no call to a normal command's hot path.
pub fn list_unavailable(conn: &mut Connection) -> Availability {
    use crate::transport::{decode_message, encode_message};
    let cli_id = match crate::contract::schema_id("cli") {
        Some(id) => id,
        None => return Availability::Unpublished,
    };

    let mut message = capnp::message::Builder::new_default();
    {
        let mut params = message.init_root::<cli_service::list_unavailable_params::Builder<'_>>();
        let mut page = params.reborrow().init_pagination();
        page.set_offset(0);
        page.set_limit(u32::MAX);
    }
    let payload = encode_message(&message);
    let result = conn.call(cli_id, 13, &payload);
    let bytes = match result {
        Ok(bytes) => bytes,
        // A daemon that does not run the registry service answers
        // unknown-interface here; honest, not a failure to list.
        Err(_) => return Availability::Unpublished,
    };
    let Ok(reader) = decode_message::<cli_service::list_unavailable_results::Owned>(&bytes) else {
        return Availability::Unpublished;
    };
    let Ok(results) = reader.get() else {
        return Availability::Unpublished;
    };
    let mut list = Vec::new();
    for registration in results.get_commands().into_iter().flatten() {
        let path = registration
            .get_command_path()
            .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned())
            .unwrap_or_default();
    use crate::schema::cli_capnp::AvailabilityReason as R;
    if let Ok(reason) = registration.get_unavailable_reason() {
        let name = match reason {
            R::Available => "available",
            R::CapabilityDenied => "capabilityDenied",
            R::ServiceUnavailable => "serviceUnavailable",
            R::UnsupportedPlatform => "unsupportedPlatform",
            R::Unknown => "unknown",
            R::DisabledByPolicy => "disabledByPolicy",
            R::Custom => "custom",
        };
        let mut reason_text = String::new();
        if let Ok(metadata) = registration.get_metadata() {
            for entry in metadata.get_entries().into_iter().flatten() {
                if let (Ok(key), Ok(value)) = (entry.get_key(), entry.get_value()) {
                    if key.to_str().unwrap_or("") == "reason" {
                        reason_text = value.to_str().unwrap_or("").to_string();
                    }
                }
            }
        }
        list.push(Unavailable {
            path,
            reason_class: name.to_string(),
            reason: reason_text,
        });
    }
    }
    Availability::Published(list)
}

/// True when `command_path` is reported not runnable by `availability`.
pub fn is_hidden(availability: &Availability, command_path: &str) -> bool {
    match availability {
        Availability::Published(list) => list.iter().any(|u| u.path == command_path),
        Availability::Unpublished => false,
    }
}

/// The reason a hidden command is unavailable, for the exit-3 message.
pub fn hidden_reason(availability: &Availability, command_path: &str) -> Option<String> {
    match availability {
        Availability::Published(list) => list
            .iter()
            .find(|u| u.path == command_path)
            .map(Unavailable::line),
        Availability::Unpublished => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_table_is_total_and_stable() {
        assert_eq!(exit_code_for(&Ok(Vec::new())), EXIT_OK);
        let failed = Err(CallError::Transport("x".into()));
        assert_eq!(exit_code_for(&failed), EXIT_CALL_FAILED);
    }
}
