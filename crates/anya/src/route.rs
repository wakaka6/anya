//! What the route page shows: every session on the hub, where its audio
//! comes from, the engine it goes to and where its text goes, and the
//! latest messages between them. Kept in memory only.
//!
//! Frames name a session's source and engine but not who opened it: a
//! paired app's dictation is `dictation` like the user's own. Who opened a
//! session and how many subscribers were sent it are asked of the servers
//! ([`Reach`]) each time the route is read, so it does not matter whether
//! a frame or the server's note of it comes first.

use std::collections::{HashMap, VecDeque};

use anya_core::audio::CHUNK_MS;
use anya_protocol::{EndReason, Frame, SessionId, Source};
use anya_runtime::{Delivered, LiveRoute, Origin, ROUTE_LOG, Route, RouteEvent, Sink, Step};

/// Audio and partial text are logged once per this much audio.
const AUDIO_STEP_MS: u64 = 1000;

/// What the servers know about a session.
pub trait Reach {
    /// The name of the paired app that opened `session`, if one did.
    fn client_of(&self, session: &SessionId) -> Option<String>;
    /// How many subscribers other than its opener were sent `session`'s text.
    fn readers_of(&self, session: &SessionId) -> usize;
}

/// Where the user's own dictation sends its text, as configured now.
#[derive(Debug, Clone, Copy)]
pub struct Outputs {
    /// Pasted, copied, or neither (`output = none`).
    pub delivery: Option<Delivered>,
    /// The recording overlay is on.
    pub overlay: bool,
}

struct Track {
    source: Source,
    engine: String,
    streaming: bool,
    open: bool,
    level: f32,
    text: String,
    /// Audio sent since the last audio message.
    unlogged_ms: u64,
    /// The text the last partial message had.
    logged_text: String,
    /// Where the user's own dictation's text went, once it did.
    delivered: Option<Delivered>,
}

/// A message before it is read: who it is about is resolved when it is.
struct Logged {
    seq: u64,
    at: u64,
    session: SessionId,
    step: Pending,
}

enum Pending {
    Start,
    Audio(u64),
    Partial(String),
    Final(String),
    Failed(anya_protocol::ErrorCode, Option<String>),
    Ended(EndReason),
}

/// Follows hub frames, and says what the route page shows.
#[derive(Default)]
pub struct Router {
    /// Open sessions, and ended ones the log still names.
    sessions: HashMap<SessionId, Track>,
    /// Open sessions, oldest first.
    open: Vec<SessionId>,
    devices: HashMap<Source, String>,
    log: VecDeque<Logged>,
    seq: u64,
}

impl Router {
    /// Takes one hub frame, received at `at` (Unix ms).
    pub fn frame(&mut self, frame: &Frame, at: u64) {
        match frame {
            Frame::Source { source, device, .. } => match device {
                Some(device) => {
                    self.devices.insert(*source, device.clone());
                }
                None => {
                    self.devices.remove(source);
                }
            },
            Frame::SessionStart {
                session,
                source,
                engine,
                streaming,
            } => {
                self.sessions.insert(
                    session.clone(),
                    Track {
                        source: *source,
                        engine: engine.clone(),
                        streaming: *streaming,
                        open: true,
                        level: 0.0,
                        text: String::new(),
                        unlogged_ms: 0,
                        logged_text: String::new(),
                        delivered: None,
                    },
                );
                self.open.push(session.clone());
                self.push(at, session, Pending::Start);
            }
            Frame::Level { session, level } => {
                let Some(track) = self.sessions.get_mut(session) else {
                    return;
                };
                track.level = *level;
                track.unlogged_ms += u64::from(CHUNK_MS);
                if track.unlogged_ms < AUDIO_STEP_MS {
                    return;
                }
                let audio = std::mem::take(&mut track.unlogged_ms);
                let partial = (track.text != track.logged_text).then(|| {
                    track.logged_text.clone_from(&track.text);
                    track.text.clone()
                });
                self.push(at, session, Pending::Audio(audio));
                if let Some(text) = partial {
                    self.push(at, session, Pending::Partial(text));
                }
            }
            Frame::Result {
                session,
                recognition,
            } => {
                let Some(track) = self.sessions.get_mut(session) else {
                    return;
                };
                track.text.clone_from(&recognition.text);
                if recognition.is_final {
                    self.flush_audio(at, session);
                    self.push(at, session, Pending::Final(recognition.text.clone()));
                }
            }
            Frame::SessionEnd {
                session,
                reason,
                error,
                detail,
            } => {
                let Some(track) = self.sessions.get_mut(session) else {
                    return;
                };
                track.open = false;
                track.level = 0.0;
                self.open.retain(|s| s != session);
                self.flush_audio(at, session);
                match (reason, error) {
                    (EndReason::Final, _) => {}
                    (EndReason::Error, Some(error)) => {
                        self.push(at, session, Pending::Failed(*error, detail.clone()));
                    }
                    (reason, _) => self.push(at, session, Pending::Ended(*reason)),
                }
                self.forget();
            }
            Frame::Hello { .. } | Frame::Unknown => {}
        }
    }

    /// The user's own dictation's text of `session` went to `delivered`.
    pub fn delivered(&mut self, session: &SessionId, delivered: Delivered) {
        if let Some(track) = self.sessions.get_mut(session) {
            track.delivered = Some(delivered);
        }
    }

    /// The engine `session` was opened on, while it is remembered.
    pub fn engine_of(&self, session: &SessionId) -> Option<String> {
        self.sessions.get(session).map(|t| t.engine.clone())
    }

    /// What the route page shows now.
    pub fn route(&self, reach: &dyn Reach, outputs: Outputs) -> Route {
        let live = self
            .open
            .iter()
            .filter_map(|session| {
                let track = self.sessions.get(session)?;
                Some(LiveRoute {
                    session: session.clone(),
                    origin: self.origin(session, track, reach),
                    engine: track.engine.clone(),
                    streaming: track.streaming,
                    level: track.level,
                    text: track.text.clone(),
                    sinks: sinks(session, track, reach, outputs),
                })
            })
            .collect();
        let log = self
            .log
            .iter()
            .filter_map(|logged| {
                let track = self.sessions.get(&logged.session)?;
                let step = match &logged.step {
                    Pending::Start => Step::Start {
                        streaming: track.streaming,
                    },
                    Pending::Audio(ms) => Step::Audio { ms: *ms },
                    Pending::Partial(text) => Step::Partial { text: text.clone() },
                    Pending::Final(text) => Step::Final {
                        text: text.clone(),
                        sinks: sinks(&logged.session, track, reach, outputs),
                    },
                    Pending::Failed(error, detail) => Step::Failed {
                        error: *error,
                        detail: detail.clone(),
                    },
                    Pending::Ended(reason) => Step::Ended { reason: *reason },
                };
                Some(RouteEvent {
                    seq: logged.seq,
                    at: logged.at,
                    session: logged.session.clone(),
                    origin: self.origin(&logged.session, track, reach),
                    engine: track.engine.clone(),
                    step,
                })
            })
            .collect();
        Route { live, log }
    }

    fn origin(&self, session: &SessionId, track: &Track, reach: &dyn Reach) -> Origin {
        let device = self.devices.get(&track.source).cloned();
        match track.source {
            Source::Continuous => Origin::Continuous { device },
            _ => match reach.client_of(session) {
                Some(client) => Origin::Remote { client },
                None => Origin::Dictation { device },
            },
        }
    }

    fn flush_audio(&mut self, at: u64, session: &SessionId) {
        let Some(track) = self.sessions.get_mut(session) else {
            return;
        };
        let audio = std::mem::take(&mut track.unlogged_ms);
        if audio > 0 {
            self.push(at, session, Pending::Audio(audio));
        }
    }

    fn push(&mut self, at: u64, session: &SessionId, step: Pending) {
        self.seq += 1;
        self.log.push_back(Logged {
            seq: self.seq,
            at,
            session: session.clone(),
            step,
        });
        if self.log.len() > ROUTE_LOG {
            self.log.pop_front();
            self.forget();
        }
    }

    /// Drops ended sessions no message names any more.
    fn forget(&mut self) {
        let log = &self.log;
        self.sessions
            .retain(|id, track| track.open || log.iter().any(|l| &l.session == id));
    }
}

/// Where `session`'s text goes, as far as is known now.
fn sinks(session: &SessionId, track: &Track, reach: &dyn Reach, outputs: Outputs) -> Vec<Sink> {
    let mut sinks = Vec::new();
    match (track.source, reach.client_of(session)) {
        (Source::Continuous, _) => {}
        (_, Some(name)) => sinks.push(Sink::Client { name }),
        (_, None) => {
            match track
                .delivered
                .or(if track.open { outputs.delivery } else { None })
            {
                Some(Delivered::Pasted) => sinks.push(Sink::Paste),
                Some(Delivered::Copied) => sinks.push(Sink::Clipboard),
                Some(Delivered::Kept) | None => {}
            }
            if outputs.overlay {
                sinks.push(Sink::Overlay);
            }
        }
    }
    let count = reach.readers_of(session);
    if count > 0 {
        sinks.push(Sink::Subscribers { count });
    }
    sinks
}

#[cfg(test)]
mod tests {
    use super::*;
    use anya_protocol::{ErrorCode, Recognition};

    #[derive(Default)]
    struct Servers {
        clients: HashMap<SessionId, String>,
        readers: HashMap<SessionId, usize>,
    }

    impl Reach for Servers {
        fn client_of(&self, session: &SessionId) -> Option<String> {
            self.clients.get(session).cloned()
        }
        fn readers_of(&self, session: &SessionId) -> usize {
            self.readers.get(session).copied().unwrap_or(0)
        }
    }

    const PASTE: Outputs = Outputs {
        delivery: Some(Delivered::Pasted),
        overlay: false,
    };

    fn id(s: &str) -> SessionId {
        SessionId(s.to_owned())
    }

    fn start(r: &mut Router, s: &str, source: Source) {
        r.frame(
            &Frame::SessionStart {
                session: id(s),
                source,
                engine: "doubao".to_owned(),
                streaming: true,
            },
            1,
        );
    }

    fn levels(r: &mut Router, s: &str, chunks: usize) {
        for _ in 0..chunks {
            r.frame(
                &Frame::Level {
                    session: id(s),
                    level: 0.5,
                },
                2,
            );
        }
    }

    fn result(r: &mut Router, s: &str, text: &str, last: bool) {
        r.frame(
            &Frame::Result {
                session: id(s),
                recognition: Recognition {
                    text: text.to_owned(),
                    utterances: Vec::new(),
                    is_final: last,
                },
            },
            3,
        );
    }

    fn end(r: &mut Router, s: &str, reason: EndReason, error: Option<ErrorCode>) {
        r.frame(
            &Frame::SessionEnd {
                session: id(s),
                reason,
                error,
                detail: None,
            },
            4,
        );
    }

    fn steps(route: &Route) -> Vec<Step> {
        route.log.iter().map(|e| e.step.clone()).collect()
    }

    #[test]
    fn a_dictation_logs_a_second_of_audio_then_its_text_and_ends_where_it_was_pasted() {
        let mut r = Router::default();
        let servers = Servers::default();
        start(&mut r, "a", Source::Dictation);
        result(&mut r, "a", "hel", false);
        levels(&mut r, "a", 24);
        assert_eq!(
            steps(&r.route(&servers, PASTE)).len(),
            1,
            "not a second yet"
        );
        levels(&mut r, "a", 1);
        result(&mut r, "a", "hello", true);

        let live = r.route(&servers, PASTE);
        assert_eq!(live.live.len(), 1, "open until its end");
        assert_eq!(live.live[0].origin, Origin::Dictation { device: None });
        assert_eq!(live.live[0].text, "hello");
        assert_eq!(
            live.live[0].sinks,
            [Sink::Paste],
            "the configured output while open"
        );

        r.delivered(&id("a"), Delivered::Copied);
        end(&mut r, "a", EndReason::Final, None);
        let done = r.route(&servers, PASTE);
        assert!(done.live.is_empty());
        assert_eq!(
            steps(&done),
            [
                Step::Start { streaming: true },
                Step::Audio { ms: 1000 },
                Step::Partial {
                    text: "hel".to_owned()
                },
                Step::Final {
                    text: "hello".to_owned(),
                    sinks: vec![Sink::Clipboard],
                },
            ],
            "where it actually went, not where it was meant to"
        );
    }

    #[test]
    fn a_paired_apps_session_goes_back_to_it_and_to_the_subscribers_sent_it() {
        let mut r = Router::default();
        let mut servers = Servers::default();
        start(&mut r, "p", Source::Dictation);
        servers.clients.insert(id("p"), "iPhone".to_owned());
        servers.readers.insert(id("p"), 2);
        let route = r.route(
            &servers,
            Outputs {
                delivery: Some(Delivered::Pasted),
                overlay: true,
            },
        );
        assert_eq!(
            route.live[0].origin,
            Origin::Remote {
                client: "iPhone".to_owned()
            }
        );
        assert_eq!(
            route.live[0].sinks,
            [
                Sink::Client {
                    name: "iPhone".to_owned()
                },
                Sink::Subscribers { count: 2 }
            ],
            "never pasted here nor shown on the overlay"
        );
    }

    #[test]
    fn continuous_text_goes_only_to_subscribers_and_the_device_is_named() {
        let mut r = Router::default();
        let mut servers = Servers::default();
        r.frame(
            &Frame::Source {
                source: Source::Continuous,
                state: anya_protocol::SourceState::Listening,
                device: Some("MacBook Speakers".to_owned()),
            },
            0,
        );
        start(&mut r, "c", Source::Continuous);
        assert_eq!(r.route(&servers, PASTE).live[0].sinks, []);
        servers.readers.insert(id("c"), 1);
        let route = r.route(&servers, PASTE);
        assert_eq!(
            route.live[0].origin,
            Origin::Continuous {
                device: Some("MacBook Speakers".to_owned())
            }
        );
        assert_eq!(route.live[0].sinks, [Sink::Subscribers { count: 1 }]);
    }

    #[test]
    fn a_failure_and_a_cancel_end_with_their_own_message() {
        let mut r = Router::default();
        let servers = Servers::default();
        start(&mut r, "f", Source::Dictation);
        levels(&mut r, "f", 3);
        end(&mut r, "f", EndReason::Error, Some(ErrorCode::Connect));
        start(&mut r, "x", Source::Dictation);
        end(&mut r, "x", EndReason::Cancelled, None);
        assert_eq!(
            steps(&r.route(&servers, PASTE)),
            [
                Step::Start { streaming: true },
                Step::Audio { ms: 120 },
                Step::Failed {
                    error: ErrorCode::Connect,
                    detail: None
                },
                Step::Start { streaming: true },
                Step::Ended {
                    reason: EndReason::Cancelled
                },
            ]
        );
    }

    #[test]
    fn the_log_keeps_the_latest_messages_and_forgets_sessions_it_no_longer_names() {
        let mut r = Router::default();
        let servers = Servers::default();
        for n in 0..ROUTE_LOG {
            let s = format!("s{n}");
            start(&mut r, &s, Source::Dictation);
            end(&mut r, &s, EndReason::Closed, None);
        }
        let route = r.route(&servers, PASTE);
        assert_eq!(route.log.len(), ROUTE_LOG);
        assert_eq!(route.log[0].session, id(&format!("s{}", ROUTE_LOG / 2)));
        assert!(route.log.windows(2).all(|w| w[0].seq + 1 == w[1].seq));
        assert_eq!(r.engine_of(&id("s0")), None, "no message names it");
        assert!(r.sessions.len() <= ROUTE_LOG / 2);
    }
}
