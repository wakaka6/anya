//! The subscriber server and the subscribe client.
//!
//! [`Server`] serves the hub's frames to other programs over a WebSocket at
//! [`PATH`], on loopback or a Unix socket: one JSON text message per frame,
//! the hub's snapshot first, then live frames, filtered to the sources each
//! subscriber is granted (docs/protocol.md, docs/decisions/0005 and 0007).
//! [`start_replay`] serves a fixture's lines the same way, byte for byte, so
//! an app is built without a microphone or an engine account. [`subscribe`] is the
//! client behind `anya subscribe`: it prints what it receives as NDJSON.

mod address;
mod clients;
mod connection;
mod discovery;
mod gateway;
mod private;
mod server;
mod socket;
mod subscribe;
mod tls;

pub use address::{Address, AddressError, DEFAULT_ADDRESS, parse_address};
pub use clients::{ClientInfo, Clients, ClientsError, PairingCode};
pub use gateway::Gateway;
pub use server::{Server, ServerError, start_replay};
pub use subscribe::{ExitReason, SubscribeError, SubscribeExit, subscribe};
pub use tls::{TlsIdentity, lan_address};

/// The WebSocket path subscribers connect to: `ws://127.0.0.1:7590/asr`.
pub const PATH: &str = "/asr";
