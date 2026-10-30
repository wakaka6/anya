//! Who a running server's frames reach: how many subscribers are connected,
//! which paired client opened a remote session, and how many other
//! connections were sent a session's text. The app reads it to show where
//! each session's text went; nothing here goes on the wire.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anya_protocol::SessionId;

/// Sessions remembered; the oldest is forgotten first. A session that is
/// still sending text comes back with its next result.
const KEEP: usize = 256;

/// What a server knows about where its frames went. Cheap to clone, and
/// still readable after the server stops.
#[derive(Debug, Clone)]
pub struct Audience {
    subscribers: Arc<AtomicUsize>,
    ledger: Ledger,
}

impl Audience {
    pub(crate) fn new(subscribers: Arc<AtomicUsize>, ledger: Ledger) -> Audience {
        Audience {
            subscribers,
            ledger,
        }
    }

    /// Subscribers connected now and still receiving frames.
    pub fn subscribers(&self) -> usize {
        self.subscribers.load(Ordering::SeqCst)
    }

    /// The id of the paired client that opened `session`, when a client did.
    pub fn client_of(&self, session: &SessionId) -> Option<String> {
        self.ledger.book().sessions.get(session)?.client.clone()
    }

    /// How many connections other than the one that opened `session` were
    /// sent a result of it.
    pub fn readers_of(&self, session: &SessionId) -> usize {
        self.ledger
            .book()
            .sessions
            .get(session)
            .map_or(0, |s| s.readers.len())
    }
}

/// The record behind [`Audience`], written by the connections.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ledger {
    book: Arc<Mutex<Book>>,
    next: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct Book {
    sessions: HashMap<SessionId, Reached>,
    /// Sessions in the order they were first noted.
    order: VecDeque<SessionId>,
}

#[derive(Debug, Default)]
struct Reached {
    client: Option<String>,
    readers: HashSet<u64>,
}

impl Ledger {
    /// A number for a new connection, unique within this server.
    pub(crate) fn seat(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Paired client `client` opened `session`.
    pub(crate) fn opened(&self, session: &SessionId, client: &str) {
        self.book().entry(session).client = Some(client.to_owned());
    }

    /// Connection `seat`, which did not open `session`, was sent a result of it.
    pub(crate) fn read(&self, session: &SessionId, seat: u64) {
        self.book().entry(session).readers.insert(seat);
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Book {
    fn entry(&mut self, session: &SessionId) -> &mut Reached {
        if !self.sessions.contains_key(session) {
            if self.order.len() == KEEP
                && let Some(oldest) = self.order.pop_front()
            {
                self.sessions.remove(&oldest);
            }
            self.order.push_back(session.clone());
        }
        self.sessions.entry(session.clone()).or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: usize) -> SessionId {
        SessionId(format!("s{n}"))
    }

    #[test]
    fn readers_count_connections_not_results() {
        let ledger = Ledger::default();
        let audience = Audience::new(Arc::default(), ledger.clone());
        let (a, b) = (ledger.seat(), ledger.seat());
        ledger.opened(&id(1), "phone");
        ledger.read(&id(1), a);
        ledger.read(&id(1), a);
        ledger.read(&id(1), b);
        assert_eq!(audience.client_of(&id(1)).as_deref(), Some("phone"));
        assert_eq!(audience.readers_of(&id(1)), 2);
        assert_eq!(audience.client_of(&id(2)), None);
        assert_eq!(audience.readers_of(&id(2)), 0);
    }

    #[test]
    fn the_oldest_session_is_forgotten_and_comes_back_with_its_next_result() {
        let ledger = Ledger::default();
        let audience = Audience::new(Arc::default(), ledger.clone());
        let seat = ledger.seat();
        ledger.opened(&id(0), "phone");
        for n in 1..=KEEP {
            ledger.read(&id(n), seat);
        }
        assert_eq!(
            audience.client_of(&id(0)),
            None,
            "pushed out by newer sessions"
        );
        assert_eq!(audience.readers_of(&id(1)), 1, "the next oldest stays");
        ledger.read(&id(0), seat);
        assert_eq!(audience.readers_of(&id(0)), 1);
    }
}
