//! The `discovery` family over `Discovery` (`discovery.capnp`) — the
//! smart-protocol SERVICE registry peers (doc 04 §4). Lookup is ungated: a
//! caller can learn what exists before it holds authority.

use alloc::string::{String, ToString};

use crate::json;
use smart_protocol::schema::discovery_capnp::discovery as wire;
use crate::transport as tp;

use super::{go, App, Handler};

fn msg<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut capnp::message::Builder<capnp::message::HeapAllocator>),
{
    let mut message = capnp::message::Builder::new_default();
    f(&mut message);
    tp::encode_message(&message)
}

pub fn run(sub: &[String], app: &mut App) -> i32 {
    let task = sub.first().map(String::as_str).unwrap_or("peers");
    let rest = &sub[1..];
    match task {
        "peers" | "scan" | "list" => {
            let params = msg(|m| {
                let mut p = m.init_root::<wire::lookup_params::Builder<'_>>();
                let _ = p.reborrow().init_filter();
            });
            go(app, "Discovery", "lookup", &params, Handler { json: discovery_peers, human: discovery_peers_human })
        }
        "resolve" => {
            let Some(name) = rest.first().cloned() else {
                super::usage_error_app(app, "usage: discovery resolve <name>");
                return crate::registry::EXIT_USAGE;
            };
            let params = msg(|m| { m.init_root::<wire::resolve_params::Builder<'_>>().set_name(&name); });
            go(app, "Discovery", "resolve", &params, Handler { json: discovery_resolve, human: discovery_resolve_human })
        }
        // The universal path: the configured endpoint addresses the DISCOVERY
        // service; the domain is looked up, its endpoint resolved, the ABI
        // negotiated on it, and the call made there.
        "endpoint" => {
            let Some(domain) = rest.first().cloned() else {
                super::usage_error_app(app, "usage: discovery endpoint <domain>");
                return crate::registry::EXIT_USAGE;
            };
            endpoint(app, &domain)
        }
        "call" => {
            let Some(domain) = rest.first().cloned() else {
                super::usage_error_app(app, "usage: discovery call <domain> <command> [key=value ...]");
                return crate::registry::EXIT_USAGE;
            };
            let Some(command) = rest.get(1).cloned() else {
                super::usage_error_app(app, "usage: discovery call <domain> <command> [key=value ...]");
                return crate::registry::EXIT_USAGE;
            };
            let args: Vec<String> = rest[2..].to_vec();
            call(app, &domain, &command, &args)
        }
        // The authority question, asked of the service itself: what does it
        // hold? The CLI renders the class the service published.
        "capability" => capability(app),
        other => {
            super::usage_error_app(app, &format!("unknown discovery task '{other}' (peers|resolve|endpoint|call|capability)"));
            crate::registry::EXIT_USAGE
        }
    }
}

/// The resolved endpoint of a domain: the lookup, the endpoint it published,
/// the call model and the carrier.
fn endpoint(app: &mut App, domain: &str) -> i32 {
    let resolved = match crate::service::resolve_domain(app.conn(), domain) {
        Ok(resolved) => resolved,
        Err(error) => return super::emit_error(app, &error),
    };
    if app.json() {
        let mut out = String::from("{\"service\":");
        crate::json::write_key(&mut out, &resolved.name);
        out.push_str(&resolved.name.clone());
        out.push_str(",\"endpoint\":");
        crate::json::write_key(&mut out, &resolved.endpoint.scheme);
        out.push_str(&resolved.endpoint.scheme.clone());
        out.push_str(",\"address\":");
        crate::json::write_key(&mut out, &resolved.endpoint.address);
        out.push_str(&resolved.endpoint.address.clone());
        out.push_str(",\"callModel\":");
        crate::json::write_key(&mut out, &resolved.call_model);
        out.push_str(&resolved.call_model.clone());
        out.push_str(",\"carrier\":");
        crate::json::write_key(&mut out, &resolved.carrier);
        out.push_str(&resolved.carrier.clone());
        out.push('}');
        println!("{out}");
    } else {
        for hop in resolved.hops() {
            println!("{hop}");
        }
    }
    crate::registry::EXIT_OK
}

/// The full universal path: resolve the domain, dial it, negotiate the ABI,
/// and call the method the service's own registry names.
fn call(app: &mut App, domain: &str, command: &str, args: &[String]) -> i32 {
    let resolved = match crate::service::resolve_domain(app.conn(), domain) {
        Ok(resolved) => resolved,
        Err(error) => return super::emit_error(app, &error),
    };
    // The discovery session has done its work: release it before dialling the
    // endpoint discovery named. A service that publishes its OWN socket as a
    // domain's endpoint serves one session at a time, and a client that
    // holds both would wait for itself.
    app.conn().close();
    if app.verbose() {
        for hop in resolved.hops() {
            eprintln!("verbose: {hop}");
        }
    }
    let mut conn = match crate::service::connect_resolved(&resolved) {
        Ok(conn) => conn,
        Err(error) => return super::emit_error(app, &error),
    };
    if app.verbose() {
        eprintln!(
            "verbose: abi: negotiated schema intersection: {:?}",
            conn.schema_ids()
        );
    }
    // The registry on the DOMAIN's service names the command and publishes
    // the method's identity and the schema's node tree.
    let Some(service) = crate::registry::fetch(&mut conn) else {
        super::usage_error_app(
            app,
            "the resolved service publishes no command registry",
        );
        return crate::registry::EXIT_UNAVAILABLE;
    };
    let Some(published) = service.command(command) else {
        super::usage_error_app(
            app,
            &format!("the resolved service publishes no command `{command}`"),
        );
        return crate::registry::EXIT_USAGE;
    };
    let published = published.clone();
    let Some(tree_hex) = published
        .metadata
        .iter()
        .find(|(key, _)| key == crate::dynamic::SCHEMA_METADATA_KEY)
        .map(|(_, value)| value.clone())
    else {
        super::usage_error_app(
            app,
            "the resolved service published no schema tree for this command",
        );
        return crate::registry::EXIT_UNAVAILABLE;
    };
    let Some(bytes) = decode_hex(&tree_hex) else {
        super::usage_error_app(app, "the published schema tree is not hex");
        return crate::registry::EXIT_UNAVAILABLE;
    };
    let Some(tree) = crate::dynamic::SchemaTree::decode(&bytes) else {
        super::usage_error_app(app, "the published schema tree does not decode");
        return crate::registry::EXIT_UNAVAILABLE;
    };
    let Some(interface) = tree.interface(&published.interface).cloned() else {
        super::usage_error_app(
            app,
            &format!("the published tree declares no interface `{}`", published.interface),
        );
        return crate::registry::EXIT_UNAVAILABLE;
    };
    let Some(method) = tree
        .method(&published.interface, &published.method)
        .cloned()
    else {
        super::usage_error_app(
            app,
            &format!(
                "the published tree declares no method `{}.{}`",
                published.interface, published.method
            ),
        );
        return crate::registry::EXIT_UNAVAILABLE;
    };
    // The method's identity: what the service published, else the tree's own
    // ordinal.
    let method_id = published.method_id().unwrap_or(method.id);
    let key_values: Vec<(String, String)> = args
        .iter()
        .filter_map(|arg| arg.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    let params = match crate::dynamic::build_params(&tree, &method, &key_values) {
        Ok(params) => params,
        Err(error) => {
            super::usage_error_app(app, &error.message);
            return crate::registry::EXIT_USAGE;
        }
    };
    app.trace_generic(
        &published.domain,
        &published.interface,
        &published.method,
        method_id,
    );
    match conn.call(interface.type_id, method_id, &params) {
        Ok(bytes) => {
            let values = crate::dynamic::read_results(&tree, method.results_type_id, &bytes)
                .unwrap_or_default();
            if app.json() {
                let mut out = String::from("{");
                for (index, (name, value)) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    crate::json::write_key(&mut out, name);
                    crate::dynamic::write_json(value, &mut out);
                }
                out.push('}');
                println!("{out}");
            } else {
                crate::dynamic::render_fields(&values);
            }
            crate::registry::EXIT_OK
        }
        Err(error) => super::emit_error(app, &error),
    }
}

/// What the service holds: the capability it published, and the class that
/// answer names.
fn capability(app: &mut App) -> i32 {
    let capabilities = match crate::service::capabilities(app.conn()) {
        Ok(capabilities) => capabilities,
        Err(error) => return super::emit_error(app, &error),
    };
    if app.json() {
        let mut out = String::from("{\"capabilities\":[");
        for (index, capability) in capabilities.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('{');
            crate::json::write_key(&mut out, "id");
            crate::json::write_str(&mut out, &capability.id);
            out.push(',');
            crate::json::write_key(&mut out, "target");
            crate::json::write_str(&mut out, &capability.target);
            out.push(',');
            crate::json::write_key(&mut out, "class");
            crate::json::write_str(&mut out, capability.class());
            out.push(',');
            crate::json::write_key(&mut out, "operations");
            out.push('[');
            for (op_index, operation) in capability.operations.iter().enumerate() {
                if op_index > 0 {
                    out.push(',');
                }
                crate::json::write_str(&mut out, operation);
            }
            out.push(']');
            out.push(',');
            crate::json::write_key(&mut out, "issuer");
            crate::json::write_str(&mut out, &capability.issuer);
            out.push('}');
        }
        out.push_str("]}");
        println!("{out}");
    } else {
        for capability in &capabilities {
            println!("{}", capability.line());
        }
    }
    crate::registry::EXIT_OK
}

/// Decode a hex string into bytes (the metadata carries the schema's node
/// tree as hex, because the contract's metadata is a string map).
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
}

fn discovery_peers(bytes: &[u8], out: &mut String) {
    let peers = tp::with_root::<wire::lookup_results::Owned, _, _>(bytes, |r| {
        let mut s = String::from("[");
        let mut first = true;
        for svc in r.get_services().into_iter().flatten() {
            json::sep(&mut s, first);
            first = false;
            json::write_service_descriptor(&mut s, svc);
        }
        s.push(']');
        s
    })
    .unwrap_or_else(|| "[]".to_string());
    out.push_str("{\"peers\":");
    out.push_str(&peers);
    out.push('}');
}
fn discovery_peers_human(bytes: &[u8]) -> String {
    let rows = tp::with_root::<wire::lookup_results::Owned, _, _>(bytes, |r| {
        let mut rows = Vec::new();
        for svc in r.get_services().into_iter().flatten() {
            rows.push(vec![
                svc.get_name().map(|n| n.to_str().unwrap_or("")).ok().unwrap_or("").to_string(),
                svc.get_category().map(|c| c.to_str().unwrap_or("")).ok().unwrap_or("").to_string(),
            ]);
        }
        rows
    })
    .unwrap_or_default();
    crate::render::table(&["service", "category"], &rows)
}

fn discovery_resolve(bytes: &[u8], out: &mut String) {
    // Both contract fields: `present` says whether the name resolved, and the
    // descriptor is the answer itself (a key is never dropped — the JSON law
    // forbids added fields AND silently dropped ones).
    let (present, descriptor) = tp::with_root::<wire::resolve_results::Owned, _, _>(bytes, |r| {
        let present = r.get_present();
        let descriptor = match r.get_descriptor() {
            Ok(descriptor) => {
                let mut one = String::new();
                json::write_service_descriptor(&mut one, descriptor);
                Some(one)
            }
            Err(_) => None,
        };
        (present, descriptor)
    })
    .unwrap_or((false, None));
    out.push_str("{\"present\":");
    out.push_str(if present { "true" } else { "false" });
    match descriptor {
        Some(descriptor) => {
            out.push_str(",\"descriptor\":");
            out.push_str(&descriptor);
        }
        None => out.push_str(",\"descriptor\":null"),
    }
    out.push('}');
}
fn discovery_resolve_human(bytes: &[u8]) -> String {
    tp::with_root::<wire::resolve_results::Owned, _, _>(bytes, |r| {
        let present = r.get_present();
        let name = r
            .get_descriptor()
            .ok()
            .and_then(|d| d.get_name().ok())
            .and_then(|n| n.to_str().ok())
            .unwrap_or("")
            .to_string();
        format!("present: {present} service: {name}\n")
    })
    .unwrap_or_default()
}
