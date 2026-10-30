//! What the recording overlay shows: the user's own dictation and nothing
//! else (docs/decisions/0005). Gateway clients' sessions and continuous
//! transcription travel the same hub as dictation's, so frames alone cannot
//! tell them apart; the dictation controller names its own sessions in its
//! status, and only those are shown.

use std::collections::HashMap;

use anya_dictation::State;
use anya_i18n::Catalog;
use anya_overlay::{Labels, Position, Style, View};
use anya_protocol::ErrorCode;
use anya_protocol::{Frame, SessionId};
use anya_runtime::{Delivered, PreviewView};

/// What a session has told subscribers so far.
#[derive(Default)]
struct Heard {
    level: f32,
    text: String,
}

/// Follows hub frames and dictation's status, and says what to show.
#[derive(Default)]
pub struct Indicator {
    sessions: HashMap<SessionId, Heard>,
    local: anya_dictation::Status,
    done: Option<(String, Delivered)>,
}

impl Indicator {
    /// Takes one hub frame. Sessions are tracked all the same; which of them
    /// is shown is decided by [`Indicator::status`].
    pub fn frame(&mut self, frame: &Frame) {
        match frame {
            Frame::SessionStart { session, .. } => {
                self.sessions.insert(session.clone(), Heard::default());
            }
            Frame::Result {
                session,
                recognition,
            } => {
                if let Some(heard) = self.sessions.get_mut(session) {
                    heard.text.clone_from(&recognition.text);
                }
            }
            Frame::Level { session, level } => {
                if let Some(heard) = self.sessions.get_mut(session) {
                    heard.level = *level;
                }
            }
            Frame::SessionEnd { session, .. } => {
                self.sessions.remove(session);
            }
            _ => {}
        }
    }

    /// Takes dictation's status: which sessions are the user's own.
    pub fn status(&mut self, status: anya_dictation::Status) {
        let busy = status.recording.is_some()
            || !status.waiting.is_empty()
            || matches!(status.state, State::Failed { .. });
        if busy {
            self.done = None;
        }
        self.local = status;
    }

    /// The text of a dictation session went to `delivered`.
    pub fn done(&mut self, text: String, delivered: Delivered) {
        self.done = Some((text, delivered));
    }

    /// The input level of the user's own session being recorded, if there is one.
    pub fn level(&self) -> Option<f32> {
        let session = self.local.recording.as_ref()?;
        Some(self.sessions.get(session).map_or(0.0, |h| h.level))
    }

    fn text_of(&self, session: &SessionId) -> String {
        self.sessions
            .get(session)
            .map(|h| h.text.clone())
            .unwrap_or_default()
    }

    /// What to show now, in `catalog`'s language.
    pub fn view(&self, catalog: &Catalog) -> View {
        if let Some(session) = &self.local.recording {
            let heard = self.sessions.get(session);
            return View::Listening {
                level: heard.map_or(0.0, |h| h.level),
                text: self.text_of(session),
            };
        }
        if let Some(session) = self.local.waiting.first() {
            return View::Finishing {
                text: self.text_of(session),
            };
        }
        if let State::Failed { error, detail } = &self.local.state {
            return View::Error {
                message: catalog.error(*error, detail.as_deref()),
                hint: String::new(),
            };
        }
        if let Some((text, delivered)) = &self.done {
            let label = match delivered {
                Delivered::Pasted => "overlay.pasted",
                Delivered::Copied => "overlay.copied",
                Delivered::Kept => "overlay.transcribed",
            };
            return View::Done {
                text: text.clone(),
                label: catalog.t(label).to_owned(),
            };
        }
        View::Idle
    }
}

/// A sample of `preview` in `catalog`'s language, for trying the overlay's
/// position and size without recording.
pub fn sample(preview: PreviewView, catalog: &Catalog) -> View {
    match preview {
        PreviewView::Listening => View::Listening {
            level: 0.6,
            text: catalog.t("overlay.sample_text").to_owned(),
        },
        PreviewView::Done => View::Done {
            text: catalog.t("overlay.sample_text").to_owned(),
            label: catalog.t("overlay.pasted").to_owned(),
        },
        PreviewView::Error => View::Error {
            message: catalog.error(ErrorCode::Connect, None),
            hint: String::new(),
        },
    }
}

/// The overlay's own words in `catalog`'s language.
pub fn labels(catalog: &Catalog) -> Labels {
    Labels {
        listening: catalog.t("overlay.listening").to_owned(),
        recognizing: catalog.t("overlay.recognizing").to_owned(),
        idle: catalog.t("overlay.idle").to_owned(),
    }
}

/// The config's `[overlay]` as a style; an unknown position takes the
/// platform's default (the settings already flag it).
pub fn style(overlay: &anya_config::Overlay) -> Style {
    Style {
        position: overlay
            .position
            .parse()
            .unwrap_or_else(|_| Position::platform_default()),
        scale: overlay.scale,
        idle_visible: overlay.idle_visible,
        show_text: overlay.show_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anya_protocol::{Recognition, Source};

    fn start(id: &str, source: Source) -> Frame {
        Frame::SessionStart {
            session: SessionId(id.to_owned()),
            source,
            engine: "e".to_owned(),
            streaming: true,
        }
    }

    fn heard(id: &str, text: &str) -> Frame {
        Frame::Result {
            session: SessionId(id.to_owned()),
            recognition: Recognition {
                text: text.to_owned(),
                utterances: Vec::new(),
                is_final: false,
            },
        }
    }

    fn level(id: &str, level: f32) -> Frame {
        Frame::Level {
            session: SessionId(id.to_owned()),
            level,
        }
    }

    fn recording(id: &str) -> anya_dictation::Status {
        anya_dictation::Status {
            state: State::Recording,
            recording: Some(SessionId(id.to_owned())),
            ..Default::default()
        }
    }

    #[test]
    fn only_the_users_own_dictation_is_shown() {
        let c = Catalog::new("en");
        let mut i = Indicator::default();
        for f in [
            start("remote", Source::Dictation),
            level("remote", 0.9),
            heard("remote", "from the phone"),
            start("meeting", Source::Continuous),
            heard("meeting", "notes"),
        ] {
            i.frame(&f);
        }
        assert_eq!(i.view(&c), View::Idle, "other sessions show nothing");

        i.frame(&start("mine", Source::Dictation));
        i.frame(&level("mine", 0.4));
        i.frame(&heard("mine", "hello"));
        i.status(recording("mine"));
        i.frame(&level("remote", 1.0));
        assert_eq!(
            i.view(&c),
            View::Listening {
                level: 0.4,
                text: "hello".to_owned()
            }
        );
    }

    #[test]
    fn a_recording_runs_through_finishing_to_done() {
        let c = Catalog::new("zh-CN");
        let mut i = Indicator::default();
        i.frame(&start("mine", Source::Dictation));
        i.frame(&heard("mine", "你好"));
        i.status(anya_dictation::Status {
            state: State::Idle,
            finishing: 1,
            recording: None,
            waiting: vec![SessionId("mine".to_owned())],
        });
        assert_eq!(
            i.view(&c),
            View::Finishing {
                text: "你好".to_owned()
            }
        );
        i.status(anya_dictation::Status::default());
        i.done("你好".to_owned(), Delivered::Pasted);
        assert_eq!(
            i.view(&c),
            View::Done {
                text: "你好".to_owned(),
                label: "已上屏".to_owned()
            }
        );
        i.status(recording("next"));
        assert!(
            matches!(i.view(&c), View::Listening { .. }),
            "a new recording replaces Done"
        );
    }

    #[test]
    fn a_failure_shows_its_translated_reason_and_no_key_hint() {
        let mut i = Indicator::default();
        i.status(anya_dictation::Status {
            state: State::Failed {
                error: anya_protocol::ErrorCode::Connect,
                detail: Some("refused".to_owned()),
            },
            ..Default::default()
        });
        let View::Error { message, hint } = i.view(&Catalog::new("en")) else {
            panic!("not an error view")
        };
        assert!(message.contains("refused"), "{message}");
        assert_eq!(hint, "", "no key answers while the overlay shows a failure");
    }

    #[test]
    fn the_level_is_the_users_own_recording_and_only_while_it_lasts() {
        let mut i = Indicator::default();
        assert_eq!(i.level(), None);
        i.frame(&start("remote", Source::Dictation));
        i.frame(&level("remote", 0.9));
        i.frame(&start("mine", Source::Dictation));
        i.frame(&level("mine", 0.4));
        assert_eq!(i.level(), None, "nothing recorded yet");
        i.status(recording("mine"));
        assert_eq!(i.level(), Some(0.4));
        i.status(anya_dictation::Status::default());
        assert_eq!(i.level(), None);
    }
}
