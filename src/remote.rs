//! Remote parity: `@host net …` uses the identical grammar; only the
//! transport and capability source change (grammar law #9). The transport is
//! named in `--verbose` — a remote caller must always see which transport and
//! which contract family it reached, because a TCP capability claim is weaker
//! (the conformance model's DEGRADED vocabulary).
//!
//! ## Transport selection (the connect law)
//!
//! | source | endpoint |
//! |---|---|
//! | `--endpoint <scheme>://<address>` | exactly that endpoint |
//! | `--connect <path>` | `unix://<path>` |
//! | `--addr host:port` | `tcp://host:port` |
//! | `@host` where `host` is `ip:port` | `tcp://ip:port` |
//! | `@host` otherwise | `unix://<host>` |
//! | otherwise | the endpoint resolution law (see [`crate::transport`]) |
//!
//! The last row is the bootstrap law: `--endpoint`, then `$SMART_ENDPOINT`,
//! then the configuration file, then the platform's runtime directory. No
//! path literal is baked into this file, and no implementation is named.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::transport::{resolve_endpoint, Endpoint, Resolution, SCHEME_TCP, SCHEME_UNIX};

/// A resolved transport, with the label `--verbose` prints (the audit trail).
#[derive(Clone, Debug)]
pub struct Spawn {
    /// The concrete endpoint.
    pub endpoint: Endpoint,
    /// Whether this is a remote (`@host`) session; controls the DEGRADED note.
    pub remote: bool,
    /// The `@host` token, when present.
    pub host_token: Option<String>,
    /// Where the endpoint came from (the audit trail names it).
    pub source: Option<String>,
}

impl Spawn {
    /// The `--verbose` transport line, e.g. `unix:/tmp/the-service.sock`.
    pub fn label(&self) -> String {
        self.endpoint.label()
    }
}

/// The overrides a command line may carry, resolved after `@host` is stripped.
#[derive(Clone, Debug, Default)]
pub struct ConnectOverrides {
    /// `--endpoint <scheme>://<address>`: the contract's own spelling.
    pub endpoint: Option<String>,
    /// `--connect <path>`: forces a unix socket.
    pub connect: Option<String>,
    /// `--addr host:port`: forces TCP.
    pub addr: Option<String>,
    /// A leading `@host` token (remote parity), if any.
    pub host_token: Option<String>,
    /// The argument list with the `@host` token and connect flags removed.
    pub rest: Vec<String>,
}

/// Parse the connect selection out of `argv`: the leading `@host` token and
/// the `--endpoint`/`--connect`/`--addr` overrides, returning the remaining
/// grammar tokens.
pub fn parse_overrides(argv: &[String]) -> ConnectOverrides {
    let mut overrides = ConnectOverrides::default();
    let mut i = 0;
    // A leading `@host` token selects the remote transport.
    if let Some(first) = argv.first() {
        if let Some(host) = first.strip_prefix('@') {
            if !host.is_empty() {
                overrides.host_token = Some(host.to_string());
                i = 1;
            }
        }
    }
    while i < argv.len() {
        let arg = &argv[i];
        match arg.as_str() {
            "--endpoint" => {
                if i + 1 < argv.len() {
                    overrides.endpoint = Some(argv[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--connect" => {
                if i + 1 < argv.len() {
                    overrides.connect = Some(argv[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--addr" => {
                if i + 1 < argv.len() {
                    overrides.addr = Some(argv[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            other => {
                if let Some(v) = other.strip_prefix("--endpoint=") {
                    overrides.endpoint = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--connect=") {
                    overrides.connect = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--addr=") {
                    overrides.addr = Some(v.to_string());
                } else {
                    overrides.rest.push(other.to_string());
                }
                i += 1;
            }
        }
    }
    overrides
}

/// Resolve the transport from the overrides and the `@host` token.
pub fn resolve(overrides: &ConnectOverrides) -> Result<Spawn, String> {
    // Explicit --endpoint / --connect / --addr win over everything.
    if let Some(text) = &overrides.endpoint {
        return Ok(Spawn {
            endpoint: Endpoint::parse(text)?,
            remote: overrides.host_token.is_some(),
            host_token: overrides.host_token.clone(),
            source: Some("--endpoint".to_string()),
        });
    }
    if let Some(path) = &overrides.connect {
        return Ok(Spawn {
            endpoint: Endpoint::new(SCHEME_UNIX, path),
            remote: overrides.host_token.is_some(),
            host_token: overrides.host_token.clone(),
            source: Some("--connect".to_string()),
        });
    }
    if let Some(addr) = &overrides.addr {
        return Ok(Spawn {
            endpoint: Endpoint::new(SCHEME_TCP, addr),
            remote: overrides.host_token.is_some(),
            host_token: overrides.host_token.clone(),
            source: Some("--addr".to_string()),
        });
    }
    // The @host token: `ip:port` is TCP; anything else is a unix socket.
    if let Some(host) = &overrides.host_token {
        if let Some((before, after)) = host.rsplit_once(':') {
            if !before.contains(':') && after.chars().all(|c| c.is_ascii_digit()) && !after.is_empty()
            {
                return Ok(Spawn {
                    endpoint: Endpoint::new(SCHEME_TCP, &format!("{before}:{after}")),
                    remote: true,
                    host_token: overrides.host_token.clone(),
                    source: Some("@host".to_string()),
                });
            }
        }
        return Ok(Spawn {
            endpoint: Endpoint::new(SCHEME_UNIX, host),
            remote: true,
            host_token: overrides.host_token.clone(),
            source: Some("@host".to_string()),
        });
    }
    // The default: the endpoint resolution law (no path literal, no
    // implementation named).
    let Resolution {
        endpoint,
        source,
        origin,
    } = resolve_endpoint(None)?;
    Ok(Spawn {
        endpoint,
        remote: false,
        host_token: None,
        source: Some(format!("{} ({origin})", source.name())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn remote_host_token_is_stripped_and_resolved() {
        let o = parse_overrides(&args("@server1 net status"));
        assert_eq!(o.host_token.as_deref(), Some("server1"));
        assert_eq!(o.rest, args("net status"));
        let spawn = resolve(&o).unwrap();
        assert!(spawn.remote);
        assert_eq!(spawn.label(), "unix:server1");
    }

    #[test]
    fn remote_tcp_hostport_resolves_to_tcp() {
        let o = parse_overrides(&args("@10.0.0.5:7777 net status"));
        let spawn = resolve(&o).unwrap();
        assert_eq!(spawn.label(), "tcp:10.0.0.5:7777");
    }

    #[test]
    fn connect_flag_forces_unix_path() {
        let o = parse_overrides(&args("net --connect /run/agent.sock status"));
        let spawn = resolve(&o).unwrap();
        assert_eq!(spawn.label(), "unix:/run/agent.sock");
        assert!(!spawn.remote);
    }

    #[test]
    fn addr_flag_forces_tcp() {
        let o = parse_overrides(&args("net --addr 127.0.0.1:9000 status"));
        let spawn = resolve(&o).unwrap();
        assert_eq!(spawn.label(), "tcp:127.0.0.1:9000");
    }

    #[test]
    fn the_endpoint_flag_carries_the_scheme() {
        let o = parse_overrides(&args("net --endpoint tcp://127.0.0.1:9000 status"));
        let spawn = resolve(&o).unwrap();
        assert_eq!(spawn.label(), "tcp:127.0.0.1:9000");
    }

    #[test]
    fn bare_command_has_no_host_token() {
        let o = parse_overrides(&args("net status"));
        assert!(o.host_token.is_none());
        assert_eq!(o.rest, args("net status"));
    }
}
