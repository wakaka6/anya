//! The paired clients: who may use the gateway and what they may do.
//!
//! A client pairs once with a code the user asks for in the settings and
//! gets a token; only the token's SHA-256 is kept. The registry is a JSON
//! file the settings and the connections share through [`Clients`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anya_protocol::{Refusal, Scope};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// How long a pairing code works.
const PAIRING_WINDOW: Duration = Duration::from_secs(120);
/// Wrong codes a pairing code survives.
const MAX_ATTEMPTS: u32 = 5;
/// The longest client name kept, in characters.
const MAX_NAME: usize = 64;
/// What a newly paired client may do.
const DEFAULT_SCOPES: [Scope; 1] = [Scope::Dictate];

/// A paired client as the settings list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// The id in Anya's list: `c-` and eight hex digits.
    pub id: String,
    /// The name the client gave when it paired.
    pub name: String,
    /// What the client may do.
    pub scopes: Vec<Scope>,
    /// When it paired, in milliseconds since the Unix epoch.
    pub paired_at: u64,
    /// When it last connected or disconnected, in milliseconds since the
    /// Unix epoch.
    pub last_seen: Option<u64>,
    /// Connections open now.
    pub connections: usize,
}

/// A one-time code to show the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    /// Six digits.
    pub code: String,
    /// When it stops working, in milliseconds since the Unix epoch.
    pub expires_at: u64,
}

/// Why changing a client failed. Display text is English, for logs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientsError {
    /// No paired client has this id.
    #[error("no paired client {0:?}")]
    UnknownClient(String),
    /// The clients file could not be written; nothing changed.
    #[error("saving the paired clients failed: {0}")]
    Write(String),
}

/// The paired clients, shared by the settings and every connection. Cheap
/// to clone; clones share state.
#[derive(Clone)]
pub struct Clients {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    window: Duration,
    state: Mutex<State>,
    /// Counts changes to scopes and membership, so open connections re-read them.
    changes: watch::Sender<u64>,
}

#[derive(Default)]
struct State {
    records: Vec<Record>,
    pairing: Option<Pairing>,
    connections: HashMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    name: String,
    token_sha256: String,
    scopes: Vec<Scope>,
    paired_at: u64,
    #[serde(default)]
    last_seen: Option<u64>,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    clients: Vec<Record>,
}

struct Pairing {
    code: String,
    expires: Instant,
    attempts: u32,
}

/// A client that has just paired.
pub(crate) struct Paired {
    pub(crate) id: String,
    pub(crate) token: String,
    pub(crate) scopes: Vec<Scope>,
}

/// A connection counted against its client; dropping it records the
/// disconnect.
pub(crate) struct Attachment {
    clients: Clients,
    id: String,
}

impl Clients {
    /// Opens the registry stored at `path` (owner-only JSON). A missing or
    /// unreadable file is an empty registry; it is written on the first
    /// change.
    pub fn open(path: PathBuf) -> Clients {
        Clients::open_with_window(path, PAIRING_WINDOW)
    }

    /// As [`Clients::open`], with pairing codes valid for `window` instead
    /// of two minutes.
    pub fn open_with_window(path: PathBuf, window: Duration) -> Clients {
        let records = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<File>(&bytes) {
                Ok(file) => file.clients,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "the clients file is not readable; starting with none");
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "reading the clients file failed; starting with none");
                Vec::new()
            }
        };
        Clients {
            inner: Arc::new(Inner {
                path,
                window,
                state: Mutex::new(State {
                    records,
                    ..State::default()
                }),
                changes: watch::channel(0).0,
            }),
        }
    }

    /// The paired clients, in the order they paired.
    pub fn list(&self) -> Vec<ClientInfo> {
        let state = self.state();
        state
            .records
            .iter()
            .map(|r| ClientInfo {
                id: r.id.clone(),
                name: r.name.clone(),
                scopes: r.scopes.clone(),
                paired_at: r.paired_at,
                last_seen: r.last_seen,
                connections: state.connections.get(&r.id).copied().unwrap_or(0),
            })
            .collect()
    }

    /// Shows a fresh code: six digits, valid for two minutes, good for one
    /// pairing. It replaces the previous code, and five wrong tries void it.
    pub fn start_pairing(&self) -> PairingCode {
        let code = format!("{:06}", random_below(1_000_000));
        let expires_at = now_ms() + millis(self.inner.window);
        self.state().pairing = Some(Pairing {
            code: code.clone(),
            expires: Instant::now() + self.inner.window,
            attempts: 0,
        });
        PairingCode { code, expires_at }
    }

    /// Replaces a client's scopes. Its open connections apply them at once.
    pub fn set_scopes(&self, id: &str, scopes: Vec<Scope>) -> Result<(), ClientsError> {
        let mut scopes = scopes;
        let mut seen = Vec::new();
        scopes.retain(|s| {
            let fresh = !seen.contains(s);
            seen.push(*s);
            fresh
        });
        let mut state = self.state();
        let mut records = state.records.clone();
        let record = records
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| ClientsError::UnknownClient(id.to_owned()))?;
        record.scopes = scopes;
        self.save(&records)?;
        state.records = records;
        drop(state);
        self.changed();
        Ok(())
    }

    /// Forgets a client; its token stops working and its open connections
    /// are closed.
    pub fn remove(&self, id: &str) -> Result<(), ClientsError> {
        let mut state = self.state();
        let mut records = state.records.clone();
        let before = records.len();
        records.retain(|r| r.id != id);
        if records.len() == before {
            return Err(ClientsError::UnknownClient(id.to_owned()));
        }
        self.save(&records)?;
        state.records = records;
        drop(state);
        self.changed();
        Ok(())
    }

    /// The id of the client holding `token`, if any.
    pub(crate) fn authenticate(&self, token: &str) -> Option<String> {
        let hash = token_hash(token);
        let state = self.state();
        // Every record is compared, so the time taken does not say how far
        // down the list a near match was.
        let mut found = None;
        for record in &state.records {
            if equal(record.token_sha256.as_bytes(), hash.as_bytes()) {
                found = Some(record.id.clone());
            }
        }
        found
    }

    /// The scopes of client `id`; `None` once it was removed.
    pub(crate) fn scopes_of(&self, id: &str) -> Option<Vec<Scope>> {
        self.state()
            .records
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.scopes.clone())
    }

    /// Counts a connection of client `id`; `None` when it was removed.
    pub(crate) fn attach(&self, id: &str) -> Option<Attachment> {
        let mut state = self.state();
        let now = now_ms();
        state.records.iter_mut().find(|r| r.id == id)?.last_seen = Some(now);
        *state.connections.entry(id.to_owned()).or_default() += 1;
        self.save_best_effort(&state.records);
        Some(Attachment {
            clients: self.clone(),
            id: id.to_owned(),
        })
    }

    /// Trades `code` for a new client named `name`.
    pub(crate) fn pair(&self, code: &str, name: &str) -> Result<Paired, Refusal> {
        let mut state = self.state();
        let Some(pairing) = state.pairing.as_mut() else {
            return Err(Refusal::PairingFailed);
        };
        if Instant::now() >= pairing.expires {
            state.pairing = None;
            return Err(Refusal::PairingFailed);
        }
        if pairing.attempts >= MAX_ATTEMPTS {
            return Err(Refusal::TooManyAttempts);
        }
        if !equal(pairing.code.as_bytes(), code.trim().as_bytes()) {
            pairing.attempts += 1;
            return Err(if pairing.attempts >= MAX_ATTEMPTS {
                Refusal::TooManyAttempts
            } else {
                Refusal::PairingFailed
            });
        }
        let token = random_hex(32);
        let id = loop {
            let id = format!("c-{}", random_hex(4));
            if state.records.iter().all(|r| r.id != id) {
                break id;
            }
        };
        let name: String = name.trim().chars().take(MAX_NAME).collect();
        let record = Record {
            name: if name.is_empty() { id.clone() } else { name },
            id: id.clone(),
            token_sha256: token_hash(&token),
            scopes: DEFAULT_SCOPES.to_vec(),
            paired_at: now_ms(),
            last_seen: None,
        };
        let scopes = record.scopes.clone();
        let mut records = state.records.clone();
        records.push(record);
        if let Err(e) = self.save(&records) {
            tracing::warn!(error = %e, "saving a new client failed; pairing refused");
            return Err(Refusal::PairingFailed);
        }
        state.records = records;
        state.pairing = None;
        Ok(Paired { id, token, scopes })
    }

    /// A receiver that changes whenever a client's scopes change or one is removed.
    pub(crate) fn changes(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    fn changed(&self) {
        self.inner.changes.send_modify(|n| *n += 1);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-update; the registry is still
        // consistent enough to serve.
        self.inner.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save(&self, records: &[Record]) -> Result<(), ClientsError> {
        let json = serde_json::json!({ "clients": records });
        let mut bytes =
            serde_json::to_vec_pretty(&json).map_err(|e| ClientsError::Write(e.to_string()))?;
        bytes.push(b'\n');
        crate::private::write(&self.inner.path, &bytes)
            .map_err(|e| ClientsError::Write(e.to_string()))
    }

    fn save_best_effort(&self, records: &[Record]) {
        if let Err(e) = self.save(records) {
            tracing::debug!(error = %e, "recording a client's last seen failed");
        }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let mut state = self.clients.state();
        if let Some(n) = state.connections.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                state.connections.remove(&self.id);
            }
        }
        if let Some(record) = state.records.iter_mut().find(|r| r.id == self.id) {
            record.last_seen = Some(now_ms());
            self.clients.save_best_effort(&state.records);
        }
    }
}

impl Attachment {
    /// The client's id.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

fn token_hash(token: &str) -> String {
    hex(digest(&SHA256, token.as_bytes()).as_ref())
}

/// Constant-time equality for secrets of public length.
fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// `n` random bytes as hex. Panics only if the system has no randomness,
/// where no secret can be made safely.
fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    fill(&mut bytes);
    hex(&bytes)
}

/// A uniform number below `limit` (at most 2^32).
fn random_below(limit: u32) -> u32 {
    // Rejecting the tail keeps every residue equally likely.
    let ceiling = u32::MAX - u32::MAX % limit;
    loop {
        let mut bytes = [0u8; 4];
        fill(&mut bytes);
        let n = u32::from_le_bytes(bytes);
        if n < ceiling {
            return n % limit;
        }
    }
}

fn fill(bytes: &mut [u8]) {
    SystemRandom::new()
        .fill(bytes)
        .expect("the system has no random source");
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, millis)
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
