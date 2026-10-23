//! The **universal path**: the CLI's configured endpoint addresses the
//! DISCOVERY service; the CLI looks a domain up through `discovery.capnp`,
//! receives that domain's `ServiceEndpoint`, connects to it, negotiates the
//! ABI, and calls the method.
//!
//! # Why this exists
//!
//! The contract's discovery service is the authority on what exists and
//! where. A client that knows ONE endpoint — the discovery service — can
//! reach every domain the service publishes, and it learns each domain's
//! endpoint, call model and carrier from the contract itself, not from a
//! configuration file and not from a hard-coded address.
//!
//! The hops, each one proved by the wire:
//!
//! 1. **the lookup** — `Discovery.lookup` with the domain's name;
//! 2. **the endpoint** — the `ServiceEndpoint` the service published;
//! 3. **the ABI** — the handshake on that endpoint, and the schema
//!    INTERSECTION both sides agreed on;
//! 4. **the call** — the method the registry named, addressed with the
//!    ordinal the registry published.
//!
//! # The call model, and the carrier
//!
//! `ServiceEndpoint.transport` carries ONE token, and the contract's own
//! comment for that field names the CALL MODEL ("capnp", "rest", "grpc") —
//! while a transport registry is keyed on the CARRIER ("unix", "tcp"). The
//! contract has one field and two vocabularies, so this client reads BOTH
//! from the contract's open metadata:
//!
//! * the CALL MODEL, from the endpoint's `callModel` metadata (the
//!   descriptor's `callModel` is the fallback);
//! * the CARRIER, from the endpoint's `carrier` metadata (the endpoint's
//!   `transport` token is the fallback).
//!
//! An endpoint whose call model this client does not speak is **unavailable
//! with a reason** — never a crash, never a silent wrong protocol.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[cfg(feature = "cmd-discovery")]
use smart_protocol::schema::discovery_capnp::discovery as wire;
#[cfg(feature = "cmd-discovery")]
use crate::transport::Endpoint;
use crate::transport::{CallError, Connection};

/// The CALL MODEL this client speaks: the contract's own (capnp RPC).
pub const CALL_MODEL: &str = "capnp";

/// The metadata key the **call model** is published under.
///
/// The same key, with the same meaning, in the service that publishes it and
/// in the client that reads it (the contract's open metadata map is the
/// contract-legal place for it).
pub const METADATA_CALL_MODEL: &str = "callModel";

/// The metadata key the **carrier** is published under.
pub const METADATA_CARRIER: &str = "carrier";

/// What discovery published, and what this client made of it.
#[cfg(feature = "cmd-discovery")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The service name discovery published.
    pub name: String,
    /// The service's schema id.
    pub schema_id: u64,
    /// The endpoint this client will dial.
    pub endpoint: Endpoint,
    /// The CALL MODEL the service published.
    pub call_model: String,
    /// The CARRIER the service published.
    pub carrier: String,
}

#[cfg(feature = "cmd-discovery")]
impl Resolved {
    /// The hops of the universal path, as the audit trail prints them.
    pub fn hops(&self) -> Vec<String> {
        vec![
            format!(
                "discovery: `{}` published the endpoint {}",
                self.name,
                self.endpoint.label()
            ),
            format!(
                "call model: {} (this client speaks {})",
                self.call_model, CALL_MODEL
            ),
            format!("carrier: {}", self.carrier),
        ]
    }
}

/// Look a domain up through `discovery.capnp` and read the endpoint it
/// published.
///
/// The lookup is a call on the DISCOVERY connection the caller already
/// holds. The endpoint it returns is NOT dialled here: the caller dials it
/// with [`connect_resolved`], so the ABI negotiation happens on the domain's
/// own connection.
#[cfg(feature = "cmd-discovery")]
pub fn resolve_domain(conn: &mut Connection, domain: &str) -> Result<Resolved, CallError> {
    let interface = smart_protocol::contract::interface_by_name("discovery", "Discovery").ok_or_else(|| {
        CallError::Transport("this build does not compile the discovery schema".into())
    })?;
    let method = smart_protocol::contract::method_id("discovery", "Discovery", "lookup")
        .ok_or_else(|| CallError::Transport("this build has no discovery lookup method".into()))?;
    let mut message = capnp::message::Builder::new_default();
    {
        let mut params = message.init_root::<wire::lookup_params::Builder<'_>>();
        let mut filter = params.reborrow().init_filter();
        filter.set_name(domain);
    }
    let payload = crate::transport::encode_message(&message);
    let bytes = conn.call(interface.type_id, method, &payload)?;
    let reader = crate::transport::decode_message::<wire::lookup_results::Owned>(&bytes)
        .map_err(CallError::Transport)?;
    let results = reader
        .get()
        .map_err(|e| CallError::Transport(format!("discovery results have no root: {e}")))?;

    // The descriptor for the domain the caller named. A service that
    // publishes one descriptor under another name is NOT the domain asked
    // for: the lookup is by name.
    let mut chosen: Option<(String, u64, String, String, String, String)> = None;
    for service in results.get_services().into_iter().flatten() {
        let name = service
            .get_name()
            .map(|t| t.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let schema_id = service.get_id().map(|id| id.get_id()).unwrap_or_default();
        let descriptor_call_model = metadata_of(service.get_metadata().ok(), METADATA_CALL_MODEL);
        let endpoints = service.get_endpoints().ok();
        let endpoint = endpoints
            .as_ref()
            .and_then(|list| if list.len() == 0 { None } else { Some(list.get(0)) });
        let (address, transport, endpoint_call_model, endpoint_carrier) = match endpoint {
            Some(endpoint) => (
                endpoint
                    .get_address()
                    .map(|t| t.to_str().unwrap_or("").to_string())
                    .unwrap_or_default(),
                endpoint
                    .get_transport()
                    .map(|t| t.to_str().unwrap_or("").to_string())
                    .unwrap_or_default(),
                metadata_of(endpoint.get_metadata().ok(), METADATA_CALL_MODEL),
                metadata_of(endpoint.get_metadata().ok(), METADATA_CARRIER),
            ),
            None => (
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
        };
        let call_model = if endpoint_call_model.is_empty() {
            descriptor_call_model.clone()
        } else {
            endpoint_call_model.clone()
        };
        let carrier = if endpoint_carrier.is_empty() {
            transport.clone()
        } else {
            endpoint_carrier.clone()
        };
        if name == domain {
            chosen = Some((name, schema_id, address, carrier, call_model, transport));
            break;
        }
    }
    let Some((name, schema_id, address, carrier, call_model, _transport)) = chosen else {
        return Err(CallError::Unavailable {
            what: format!("the service `{domain}`"),
            reason: "discovery published no service under that name".to_string(),
        });
    };
    // The call model: the one thing this client must not guess. An endpoint
    // this client does not speak is unavailable WITH A REASON — never a
    // crash, never a silent wrong protocol.
    if call_model.is_empty() {
        return Err(CallError::Unavailable {
            what: format!("the service `{name}`"),
            reason: format!(
                "it published no call model; this client speaks only `{CALL_MODEL}` and \
                 will not dial an endpoint whose protocol it cannot name"
            ),
        });
    }
    if call_model != CALL_MODEL {
        return Err(CallError::Unavailable {
            what: format!("the service `{name}`"),
            reason: format!(
                "it speaks the call model `{call_model}`; this client speaks `{CALL_MODEL}`"
            ),
        });
    }
    if address.is_empty() || carrier.is_empty() {
        return Err(CallError::Unavailable {
            what: format!("the service `{name}`"),
            reason: "it published no endpoint to dial".to_string(),
        });
    }
    Ok(Resolved {
        name,
        schema_id,
        endpoint: Endpoint::new(&carrier, &address),
        call_model,
        carrier,
    })
}

/// Dial a resolved endpoint and negotiate the ABI with it.
///
/// The connection comes back with the schema INTERSECTION both sides agreed
/// on (`Connection::schema_ids`): the caller declared the schemas it
/// speaks, the service answered with the ones it speaks, and the agreed set
/// is what is left.
#[cfg(feature = "cmd-discovery")]
pub fn connect_resolved(resolved: &Resolved) -> Result<Connection, CallError> {
    Connection::connect(&resolved.endpoint)
}

/// Read one metadata entry out of an open metadata map.
#[cfg(feature = "cmd-discovery")]
fn metadata_of(
    map: Option<smart_protocol::schema::types_capnp::string_map::Reader<'_>>,
    key: &str,
) -> String {
    let Some(map) = map else {
        return String::new();
    };
    for entry in map.get_entries().into_iter().flatten() {
        let entry_key = entry.get_key().map(|t| t.to_str().unwrap_or("")).unwrap_or("");
        if entry_key == key {
            return entry.get_value().map(|t| t.to_str().unwrap_or("")).unwrap_or("").to_string();
        }
    }
    String::new()
}

/// What a service holds: the contract's own `Capability` structs, published
/// through `capability.capnp`.
///
/// The class is the service's own answer, not the client's guess: the
/// operations the service published ARE the class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedCapability {
    /// The capability's id, as the service published it (hex).
    pub id: String,
    /// The target the capability covers.
    pub target: String,
    /// The operations it grants.
    pub operations: Vec<String>,
    /// Who issued it.
    pub issuer: String,
}

impl PublishedCapability {
    /// The class the service's own answer names.
    ///
    /// A capability that grants a mutating operation is a control capability;
    /// one that grants only reads is a read-only capability. The word comes
    /// from the contract's own `Operation` enum — never from a local policy.
    pub fn class(&self) -> &'static str {
        let mutating = self.operations.iter().any(|op| {
            matches!(
                op.as_str(),
                "write" | "execute" | "publish" | "create" | "destroy" | "attenuate"
                    | "transfer" | "revoke"
            )
        });
        if mutating {
            "Control"
        } else {
            "ReadOnly"
        }
    }

    /// The line the CLI prints for one capability.
    pub fn line(&self) -> String {
        format!(
            "{} — class {} — operations: {} — issuer: {}",
            self.target,
            self.class(),
            if self.operations.is_empty() {
                "(none)".to_string()
            } else {
                self.operations.join(", ")
            },
            if self.issuer.is_empty() {
                "(unnamed)".to_string()
            } else {
                self.issuer.clone()
            }
        )
    }
}

/// Ask a service what it holds (capability.capnp's `CapabilityService.list`).
///
/// This is the authority question asked of the service itself: the CLI does
/// not decide what the caller may do — it renders what the service published.
pub fn capabilities(conn: &mut Connection) -> Result<Vec<PublishedCapability>, CallError> {
    let interface = smart_protocol::contract::interface_by_name("capability", "CapabilityService")
        .ok_or_else(|| {
            CallError::Transport("this build does not compile the capability schema".into())
        })?;
    let method = smart_protocol::contract::method_id("capability", "CapabilityService", "list")
        .ok_or_else(|| CallError::Transport("this build has no capability list method".into()))?;
    let mut message = capnp::message::Builder::new_default();
    {
        let mut params =
            message.init_root::<smart_protocol::schema::capability_capnp::capability_service::list_params::Builder<'_>>();
        params.reborrow().init_filter();
        let mut page = params.reborrow().init_pagination();
        page.set_offset(0);
        page.set_limit(u32::MAX);
    }
    let payload = crate::transport::encode_message(&message);
    let bytes = conn.call(interface.type_id, method, &payload)?;
    let reader = crate::transport::decode_message::<
        smart_protocol::schema::capability_capnp::capability_service::list_results::Owned,
    >(&bytes)
    .map_err(CallError::Transport)?;
    let results = reader
        .get()
        .map_err(|e| CallError::Transport(format!("capability results have no root: {e}")))?;
    let mut out = Vec::new();
    for capability in results.get_caps().into_iter().flatten() {
        let id = capability
            .get_id()
            .map(|data| {
                data.iter()
                    .map(|byte| {
                        char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0').to_string()
                            + &char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0').to_string()
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        let target = capability
            .get_target()
            .map(|t| t.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let mut operations = Vec::new();
        for operation in capability.get_operations().into_iter().flatten() {
            operations.push(operation_token(operation));
        }
        let issuer = capability
            .get_audit()
            .ok()
            .and_then(|audit| audit.get_issuer().ok())
            .map(|t| t.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        out.push(PublishedCapability {
            id,
            target,
            operations,
            issuer,
        });
    }
    Ok(out)
}

/// The stable token of a capability operation (the contract's own enum).
fn operation_token(
    operation: Result<smart_protocol::schema::capability_capnp::Operation, capnp::NotInSchema>,
) -> String {
    use smart_protocol::schema::capability_capnp::Operation as Op;
    match operation {
        Ok(Op::Read) => "read".to_string(),
        Ok(Op::Write) => "write".to_string(),
        Ok(Op::Execute) => "execute".to_string(),
        Ok(Op::Subscribe) => "subscribe".to_string(),
        Ok(Op::Publish) => "publish".to_string(),
        Ok(Op::Create) => "create".to_string(),
        Ok(Op::Destroy) => "destroy".to_string(),
        Ok(Op::Attenuate) => "attenuate".to_string(),
        Ok(Op::Transfer) => "transfer".to_string(),
        Ok(Op::Revoke) => "revoke".to_string(),
        Ok(Op::Custom) => "custom".to_string(),
        Err(_) => "unknown".to_string(),
    }
}
