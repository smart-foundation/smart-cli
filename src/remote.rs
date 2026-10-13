//! Remote parity: `@host net …` uses the identical grammar; only the transport
//! and capability source change (grammar law #9). The transport is named in
//! `--verbose` — a remote caller must always see which transport and which
//! contract family it reached, because a TCP capability claim is weaker (the
//! conformance model's DEGRADED vocabulary).
//!
//! ## Transport selection (the connect law)
//!
//! | source | transport |
//! |---|---|
//! | `--connect <path>` | a unix socket at `<path>` |
//! | `--addr host:port` | TCP at `host:port` |
//! | `@host` where `host` is `ip:port` | TCP at `ip:port` |
//! | `@host` otherwise (a path or a name) | a unix socket at `@host`'s value |
//! | otherwise | unix `$SMART_SERVICE_SOCKET`, else `/tmp/smart-service.sock` |

use alloc::string::{String, ToString};

use crate::transport::{Target, DEFAULT_SOCKET};

/// A resolved transport, with the label `--verbose` prints (the audit trail).
#[derive(Clone, Debug)]
pub struct Spawn {
    /// The concrete target.
    pub target: Target,
    /// Whether this is a remote (`@host`) session; controls the DEGRADED note.
    pub remote: bool,
    /// The `@host` token, when present.
    pub host_token: Option<String>,
}

impl Spawn {
    /// The `--verbose` transport line, e.g. `unix:/tmp/smart-service.sock`.
    pub fn label(&self) -> String {
        match &self.target {
            Target::Unix(path) => format!("unix:{path}"),
            Target::Tcp(addr) => format!("tcp:{addr}"),
        }
    }
}

/// The overrides a command line may carry, resolved after `@host` is stripped.
#[derive(Clone, Debug, Default)]
pub struct ConnectOverrides {
    /// `--connect <path>`: forces a unix socket.
    pub connect: Option<String>,
    /// `--addr host:port`: forces TCP.
    pub addr: Option<String>,
    /// A leading `@host` token (remote parity), if any.
    pub host_token: Option<String>,
    /// The argument list with the `@host` token and connect flags removed.
    pub rest: Vec<String>,
}

/// Parse the connect selection out of `argv`: the leading `@host` token and the
/// `--connect`/`--addr` overrides, returning the remaining grammar tokens.
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
                if let Some(path) = other.strip_prefix("--connect=") {
                    overrides.connect = Some(path.to_string());
                } else if let Some(addr) = other.strip_prefix("--addr=") {
                    overrides.addr = Some(addr.to_string());
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
    // Explicit --connect / --addr win over everything.
    if let Some(path) = &overrides.connect {
        return Ok(Spawn {
            target: Target::Unix(path.clone()),
            remote: overrides.host_token.is_some(),
            host_token: overrides.host_token.clone(),
        });
    }
    if let Some(addr) = &overrides.addr {
        return Ok(Spawn {
            target: Target::Tcp(addr.clone()),
            remote: overrides.host_token.is_some(),
            host_token: overrides.host_token.clone(),
        });
    }
    // The @host token: `ip:port` is TCP; anything else is a unix socket.
    if let Some(host) = &overrides.host_token {
        if let Some((before, after)) = host.rsplit_once(':') {
            if !before.contains(':') && after.chars().all(|c| c.is_ascii_digit()) && !after.is_empty()
            {
                return Ok(Spawn {
                    target: Target::Tcp(format!("{before}:{after}")),
                    remote: true,
                    host_token: Some(host.clone()),
                });
            }
        }
        return Ok(Spawn {
            target: Target::Unix(host.clone()),
            remote: true,
            host_token: Some(host.clone()),
        });
    }
    // The default: the service socket from the environment, else the default.
    let path = std::env::var("SMART_SERVICE_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string());
    Ok(Spawn {
        target: Target::Unix(path),
        remote: false,
        host_token: None,
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
        let o = parse_overrides(&args("net --connect /run/smart.sock status"));
        let spawn = resolve(&o).unwrap();
        assert_eq!(spawn.label(), "unix:/run/smart.sock");
        assert!(!spawn.remote);
    }

    #[test]
    fn addr_flag_forces_tcp() {
        let o = parse_overrides(&args("net --addr 127.0.0.1:9000 status"));
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
