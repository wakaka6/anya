//! The ordering promises a subscriber builds on, as a checker. The fixtures in
//! `fixtures/` pass it, and Anya's tests run it on what the server sends.
//! docs/protocol.md states the same promises in prose.

use std::collections::HashMap;
use std::fmt;

use crate::{EndReason, Frame, SessionId, Utterance};

const HELLO_FIRST: &str = "hello is the first frame, and only the first";
const ONE_START: &str = "each session starts once";
const START_FIRST: &str = "a session's frames follow its session_start";
const NOTHING_AFTER_END: &str = "no frame names a session after its session_end";
const FINAL_IS_LAST: &str = "the final result is the session's last result";
const DEFINITE_STAYS: &str = "a definite utterance keeps its place and its text";
const FINAL_MEANS_FINAL: &str =
    "a session ends with reason final exactly when its final result was sent";

/// A broken promise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breach {
    /// Position of the frame that breaks it.
    pub index: usize,
    /// The promise, worded as in docs/protocol.md.
    pub promise: &'static str,
}

impl fmt::Display for Breach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "frame {}: {}", self.index, self.promise)
    }
}

#[derive(Default)]
struct Session {
    last: Vec<Utterance>,
    finished: bool,
    ended: bool,
}

/// Checks the frames one connection received, in order. A stream may stop
/// mid-session: a subscriber can disconnect at any time.
pub fn check(frames: &[Frame]) -> Result<(), Breach> {
    let mut sessions: HashMap<&SessionId, Session> = HashMap::new();
    for (index, frame) in frames.iter().enumerate() {
        let breach = |promise| Err(Breach { index, promise });
        if (index == 0) != matches!(frame, Frame::Hello { .. }) {
            return breach(HELLO_FIRST);
        }
        match frame {
            Frame::SessionStart { session, .. } => {
                if sessions.insert(session, Session::default()).is_some() {
                    return breach(ONE_START);
                }
            }
            Frame::Result {
                session,
                recognition,
            } => {
                let Some(s) = sessions.get_mut(session) else {
                    return breach(START_FIRST);
                };
                if s.ended {
                    return breach(NOTHING_AFTER_END);
                }
                if s.finished {
                    return breach(FINAL_IS_LAST);
                }
                let kept = s
                    .last
                    .iter()
                    .enumerate()
                    .filter(|(_, u)| u.definite)
                    .all(|(i, u)| recognition.utterances.get(i) == Some(u));
                if !kept {
                    return breach(DEFINITE_STAYS);
                }
                s.last.clone_from(&recognition.utterances);
                s.finished = recognition.is_final;
            }
            Frame::Level { session, .. } => match sessions.get(session) {
                None => return breach(START_FIRST),
                Some(s) if s.ended => return breach(NOTHING_AFTER_END),
                Some(_) => {}
            },
            Frame::SessionEnd {
                session, reason, ..
            } => {
                let Some(s) = sessions.get_mut(session) else {
                    return breach(START_FIRST);
                };
                if s.ended {
                    return breach(NOTHING_AFTER_END);
                }
                if *reason != EndReason::Unknown && (*reason == EndReason::Final) != s.finished {
                    return breach(FINAL_MEANS_FINAL);
                }
                s.ended = true;
            }
            Frame::Hello { .. } | Frame::Source { .. } | Frame::Unknown => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Recognition, Source};

    fn hello() -> Frame {
        Frame::Hello {
            name: "anya".into(),
            version: "0.1.0".into(),
            protocol: 0,
        }
    }

    fn id(n: u32) -> SessionId {
        SessionId(format!("s{n}"))
    }

    fn start(n: u32) -> Frame {
        Frame::SessionStart {
            session: id(n),
            source: Source::Continuous,
            engine: "example".into(),
        }
    }

    fn result(n: u32, utterances: &[(&str, bool)], is_final: bool) -> Frame {
        let utterances: Vec<Utterance> = utterances
            .iter()
            .map(|(text, definite)| Utterance {
                text: (*text).into(),
                definite: *definite,
            })
            .collect();
        let text = utterances.iter().map(|u| u.text.as_str()).collect();
        Frame::Result {
            session: id(n),
            recognition: Recognition {
                text,
                utterances,
                is_final,
            },
        }
    }

    fn end(n: u32, reason: EndReason) -> Frame {
        Frame::SessionEnd {
            session: id(n),
            reason,
            error: None,
            detail: None,
        }
    }

    fn broken(frames: Vec<Frame>) -> Option<(usize, &'static str)> {
        check(&frames).err().map(|b| (b.index, b.promise))
    }

    #[test]
    fn interleaved_sessions_keep_their_promises() {
        let frames = vec![
            hello(),
            start(1),
            result(1, &[("a", false)], false),
            start(2),
            result(1, &[("a.", true), ("b", false)], false),
            result(2, &[("x.", true)], true),
            end(2, EndReason::Final),
            result(1, &[("a.", true), ("b.", true)], true),
            end(1, EndReason::Final),
            start(3),
            result(3, &[("y", false)], false),
            end(3, EndReason::Cancelled),
            Frame::Unknown,
            start(4),
        ];
        assert_eq!(check(&frames), Ok(()));
    }

    #[test]
    fn hello_comes_first_and_once() {
        assert_eq!(broken(vec![start(1)]), Some((0, HELLO_FIRST)));
        assert_eq!(broken(vec![hello(), hello()]), Some((1, HELLO_FIRST)));
    }

    #[test]
    fn sessions_start_once_and_first() {
        assert_eq!(
            broken(vec![hello(), start(1), start(1)]),
            Some((2, ONE_START))
        );
        assert_eq!(
            broken(vec![hello(), result(1, &[], false)]),
            Some((1, START_FIRST))
        );
        assert_eq!(
            broken(vec![hello(), end(1, EndReason::Closed)]),
            Some((1, START_FIRST))
        );
    }

    #[test]
    fn nothing_follows_the_end_or_the_final_result() {
        let level = |n| Frame::Level {
            session: id(n),
            level: 0.5,
        };
        assert_eq!(broken(vec![hello(), level(1)]), Some((1, START_FIRST)));
        let late_level = vec![hello(), start(1), end(1, EndReason::Closed), level(1)];
        assert_eq!(broken(late_level), Some((3, NOTHING_AFTER_END)));
        let ended = vec![
            hello(),
            start(1),
            end(1, EndReason::Closed),
            result(1, &[], false),
        ];
        assert_eq!(broken(ended), Some((3, NOTHING_AFTER_END)));
        let twice = vec![
            hello(),
            start(1),
            end(1, EndReason::Closed),
            end(1, EndReason::Closed),
        ];
        assert_eq!(broken(twice), Some((3, NOTHING_AFTER_END)));
        let after_final = vec![
            hello(),
            start(1),
            result(1, &[], true),
            result(1, &[], false),
        ];
        assert_eq!(broken(after_final), Some((3, FINAL_IS_LAST)));
    }

    #[test]
    fn a_definite_utterance_never_changes() {
        let base = || {
            vec![
                hello(),
                start(1),
                result(1, &[("a.", true), ("b", false)], false),
            ]
        };
        let mut edited = base();
        edited.push(result(1, &[("A.", true), ("b", false)], false));
        assert_eq!(broken(edited), Some((3, DEFINITE_STAYS)));
        let mut dropped = base();
        dropped.push(result(1, &[], false));
        assert_eq!(broken(dropped), Some((3, DEFINITE_STAYS)));
        let mut revised = base();
        revised.push(result(1, &[("a.", true), ("be", true)], false));
        assert_eq!(check(&revised), Ok(()));
    }

    #[test]
    fn reason_final_matches_the_final_result() {
        let unsent = vec![
            hello(),
            start(1),
            result(1, &[], false),
            end(1, EndReason::Final),
        ];
        assert_eq!(broken(unsent), Some((3, FINAL_MEANS_FINAL)));
        let unsaid = vec![
            hello(),
            start(1),
            result(1, &[], true),
            end(1, EndReason::Closed),
        ];
        assert_eq!(broken(unsaid), Some((3, FINAL_MEANS_FINAL)));
        let newer = vec![
            hello(),
            start(1),
            result(1, &[], true),
            end(1, EndReason::Unknown),
        ];
        assert_eq!(check(&newer), Ok(()));
    }
}
