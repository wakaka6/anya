//! Debug mode's timing of the user's own dictation sessions: when the engine
//! connected, when the first words came and how long the final result took
//! after the user stopped. Only times are kept, never a transcript.

use std::collections::HashMap;
use std::time::Instant;

use anya_protocol::{Frame, SessionId};
use anya_runtime::DebugEvent;

/// What one session has shown so far.
struct Run {
    began: Instant,
    connected: Option<Instant>,
    first_text: Option<Instant>,
    stopped: Option<Instant>,
}

/// Follows hub frames and dictation's status.
#[derive(Default)]
pub struct Timing {
    runs: HashMap<SessionId, Run>,
    /// Sessions that were the user's own recording at some point.
    own: Vec<SessionId>,
    recording: Option<SessionId>,
}

fn ms(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.saturating_duration_since(from).as_millis()).unwrap_or(u64::MAX)
}

impl Timing {
    /// Takes dictation's status: which session is the user's own, and when it
    /// stopped recording.
    pub fn status(&mut self, status: &anya_dictation::Status, now: Instant) {
        if let Some(id) = &status.recording {
            self.runs.entry(id.clone()).or_insert(Run {
                began: now,
                connected: None,
                first_text: None,
                stopped: None,
            });
            if !self.own.contains(id) {
                self.own.push(id.clone());
            }
        }
        let before = self.recording.take();
        if before != status.recording {
            if let Some(run) = before.and_then(|id| self.runs.get_mut(&id)) {
                run.stopped.get_or_insert(now);
            }
        }
        self.recording.clone_from(&status.recording);
    }

    /// Takes one hub frame; a finished own session gives its timing.
    pub fn frame(&mut self, frame: &Frame, now: Instant) -> Option<DebugEvent> {
        match frame {
            Frame::SessionStart { session, .. } => {
                let run = self.runs.entry(session.clone()).or_insert(Run {
                    began: now,
                    connected: None,
                    first_text: None,
                    stopped: None,
                });
                run.connected.get_or_insert(now);
                None
            }
            Frame::Result {
                session,
                recognition,
            } => {
                if let Some(run) = self.runs.get_mut(session) {
                    if !recognition.text.trim().is_empty() {
                        run.first_text.get_or_insert(now);
                    }
                }
                None
            }
            Frame::SessionEnd { session, .. } => {
                let run = self.runs.remove(session)?;
                if !self.own.contains(session) {
                    return None;
                }
                self.own.retain(|id| id != session);
                Some(DebugEvent::Timing {
                    connect_ms: run.connected.map(|c| ms(run.began, c)),
                    first_text_ms: run.first_text.map(|f| ms(run.began, f)),
                    final_ms: run.stopped.map(|s| ms(s, now)),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anya_protocol::{EndReason, Recognition, Source};

    use super::*;

    fn id(s: &str) -> SessionId {
        SessionId(s.to_owned())
    }

    fn recording(s: &str) -> anya_dictation::Status {
        anya_dictation::Status {
            state: anya_dictation::State::Recording,
            recording: Some(id(s)),
            ..Default::default()
        }
    }

    fn start(s: &str) -> Frame {
        Frame::SessionStart {
            session: id(s),
            source: Source::Dictation,
            engine: "e".to_owned(),
            streaming: true,
        }
    }

    fn words(s: &str, text: &str) -> Frame {
        Frame::Result {
            session: id(s),
            recognition: Recognition {
                text: text.to_owned(),
                utterances: Vec::new(),
                is_final: false,
            },
        }
    }

    fn end(s: &str) -> Frame {
        Frame::SessionEnd {
            session: id(s),
            reason: EndReason::Final,
            error: None,
            detail: None,
        }
    }

    #[test]
    fn an_own_session_reports_each_stage() {
        let t0 = Instant::now();
        let at = |n| t0 + Duration::from_millis(n);
        let mut timing = Timing::default();
        timing.status(&recording("mine"), at(0));
        assert_eq!(timing.frame(&start("mine"), at(210)), None);
        assert_eq!(timing.frame(&words("mine", ""), at(300)), None);
        assert_eq!(timing.frame(&words("mine", "hi"), at(830)), None);
        timing.status(&anya_dictation::Status::default(), at(2000));
        assert_eq!(
            timing.frame(&end("mine"), at(2480)),
            Some(DebugEvent::Timing {
                connect_ms: Some(210),
                first_text_ms: Some(830),
                final_ms: Some(480),
            })
        );
    }

    #[test]
    fn other_sessions_report_nothing() {
        let t0 = Instant::now();
        let mut timing = Timing::default();
        assert_eq!(timing.frame(&start("phone"), t0), None);
        assert_eq!(timing.frame(&end("phone"), t0), None);
    }
}
