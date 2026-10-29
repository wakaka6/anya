//! Where the server listens and the client connects: a loopback `host:port`
//! or a Unix socket.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

/// The address used when none is configured: `127.0.0.1:7590`.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:7590";

/// A parsed subscription address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Address {
    /// A loopback TCP address; port 0 asks for an ephemeral one.
    Tcp(SocketAddr),
    /// A Unix socket path, `~` already expanded.
    Unix(PathBuf),
}

/// Why an address was refused. Display text is English, for logs; the app
/// matches on the variant to show its own words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    /// Nothing was given; the caller uses [`DEFAULT_ADDRESS`] instead.
    #[error("the address is empty")]
    Empty,
    /// A TCP address outside loopback: only `127.0.0.0/8`, `::1` and
    /// `localhost` are served.
    #[error("{0:?} is not a loopback address; only 127.0.0.1, ::1 or localhost are allowed")]
    NotLoopback(String),
    /// Neither `host:port` nor `unix:<path>`.
    #[error("{address:?} is not host:port or unix:<path>: {reason}")]
    Invalid {
        /// What was given.
        address: String,
        /// What is wrong with it.
        reason: String,
    },
    /// `unix:` with no path after it.
    #[error("{0:?} has no socket path")]
    MissingPath(String),
    /// The path starts with `~` and `HOME` is not set.
    #[error("cannot expand ~ in {0:?}: HOME is not set")]
    NoHome(String),
}

/// Parses a subscription address: a loopback `host:port` (`localhost`
/// allowed, port 0 for an ephemeral one) or `unix:<path>`, `~` expanded.
pub fn parse_address(s: &str) -> Result<Address, AddressError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(AddressError::Empty);
    }
    if let Some(path) = s.strip_prefix("unix:") {
        let path = path.trim();
        if path.is_empty() {
            return Err(AddressError::MissingPath(s.to_owned()));
        }
        return expand_home(path)
            .map(Address::Unix)
            .ok_or_else(|| AddressError::NoHome(s.to_owned()));
    }
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return if addr.ip().is_loopback() {
            Ok(Address::Tcp(addr))
        } else {
            Err(AddressError::NotLoopback(s.to_owned()))
        };
    }
    let invalid = |reason: &str| AddressError::Invalid {
        address: s.to_owned(),
        reason: reason.to_owned(),
    };
    let Some((host, port)) = s.rsplit_once(':') else {
        return Err(invalid("missing port"));
    };
    if host.contains(':') || host.contains('/') {
        return Err(invalid("malformed host"));
    }
    let port: u16 = port.parse().map_err(|_| invalid("bad port"))?;
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(Address::Tcp(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
        )));
    }
    Err(AddressError::NotLoopback(s.to_owned()))
}

/// `~` and `~/…` become the home directory; anything else stays as written.
fn expand_home(path: &str) -> Option<PathBuf> {
    let rest = match path.strip_prefix('~') {
        None => return Some(PathBuf::from(path)),
        Some("") => "",
        Some(rest) => match rest.strip_prefix('/') {
            Some(rest) => rest,
            None => return Some(PathBuf::from(path)),
        },
    };
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    if rest.is_empty() {
        return Some(PathBuf::from(home));
    }
    Some(Path::new(&home).join(rest))
}

impl Address {
    /// What a subscriber connects to: `ws://127.0.0.1:7590/asr`, or
    /// `unix:<path>` for a socket (the WebSocket path is [`crate::PATH`]).
    pub fn url(&self) -> String {
        match self {
            Address::Tcp(addr) => format!("ws://{addr}{}", crate::PATH),
            Address::Unix(path) => format!("unix:{}", path.display()),
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Address::Tcp(addr) => write!(f, "{addr}"),
            Address::Unix(path) => write!(f, "unix:{}", path.display()),
        }
    }
}
