//! The lines the app writes to the helper's stdin: one JSON object per
//! line, tagged by `cmd`. The helper answers once, with [`READY`] on stdout,
//! when AppKit is up.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::{Labels, Position, Style, View};

/// The helper's only stdout line, written once AppKit is running.
pub(super) const READY: &str = "ready";

/// One line on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub(super) enum Message<'a> {
    Style {
        position: Cow<'a, str>,
        scale: f64,
        idle_visible: bool,
        show_text: bool,
    },
    Labels {
        listening: Cow<'a, str>,
        recognizing: Cow<'a, str>,
        idle: Cow<'a, str>,
    },
    Idle,
    Listening {
        level: f32,
        text: Cow<'a, str>,
    },
    Finishing {
        text: Cow<'a, str>,
    },
    Done {
        text: Cow<'a, str>,
        label: Cow<'a, str>,
    },
    Error {
        message: Cow<'a, str>,
        hint: Cow<'a, str>,
    },
}

/// What a decoded line changes in the helper.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Update {
    Style(Style),
    Labels(Labels),
    View(View),
}

/// A line the helper could not read.
#[derive(Debug, thiserror::Error)]
pub(super) enum DecodeError {
    #[error("bad overlay line: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Position(#[from] crate::UnknownPosition),
}

/// A level the meter can draw: finite, 0 to 1.
pub(super) fn level(level: f32) -> f32 {
    if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A usable scale: anything not positive and finite is 1.
pub(super) fn scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

pub(super) fn style(style: &Style) -> Message<'static> {
    Message::Style {
        position: Cow::Borrowed(style.position.as_str()),
        scale: scale(style.scale),
        idle_visible: style.idle_visible,
        show_text: style.show_text,
    }
}

pub(super) fn labels(labels: &Labels) -> Message<'_> {
    Message::Labels {
        listening: Cow::Borrowed(&labels.listening),
        recognizing: Cow::Borrowed(&labels.recognizing),
        idle: Cow::Borrowed(&labels.idle),
    }
}

pub(super) fn view(view: &View) -> Message<'_> {
    match view {
        View::Idle => Message::Idle,
        View::Listening { level: l, text } => Message::Listening {
            level: level(*l),
            text: Cow::Borrowed(text),
        },
        View::Finishing { text } => Message::Finishing {
            text: Cow::Borrowed(text),
        },
        View::Done { text, label } => Message::Done {
            text: Cow::Borrowed(text),
            label: Cow::Borrowed(label),
        },
        View::Error { message, hint } => Message::Error {
            message: Cow::Borrowed(message),
            hint: Cow::Borrowed(hint),
        },
    }
}

/// Appends `message` and its newline to `out`. JSON escapes every newline
/// inside strings, so a line is always one message.
pub(super) fn encode(out: &mut Vec<u8>, message: &Message<'_>) -> Result<(), serde_json::Error> {
    serde_json::to_writer(&mut *out, message)?;
    out.push(b'\n');
    Ok(())
}

/// Reads one line, without its newline.
pub(super) fn decode(line: &str) -> Result<Update, DecodeError> {
    Ok(match serde_json::from_str::<Message<'_>>(line)? {
        Message::Style {
            position,
            scale: s,
            idle_visible,
            show_text,
        } => Update::Style(Style {
            position: position.parse::<Position>()?,
            scale: scale(s),
            idle_visible,
            show_text,
        }),
        Message::Labels {
            listening,
            recognizing,
            idle,
        } => Update::Labels(Labels {
            listening: listening.into_owned(),
            recognizing: recognizing.into_owned(),
            idle: idle.into_owned(),
        }),
        Message::Idle => Update::View(View::Idle),
        Message::Listening { level: l, text } => Update::View(View::Listening {
            level: level(l),
            text: text.into_owned(),
        }),
        Message::Finishing { text } => Update::View(View::Finishing {
            text: text.into_owned(),
        }),
        Message::Done { text, label } => Update::View(View::Done {
            text: text.into_owned(),
            label: label.into_owned(),
        }),
        Message::Error { message, hint } => Update::View(View::Error {
            message: message.into_owned(),
            hint: hint.into_owned(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(message: &Message<'_>) -> String {
        let mut out = Vec::new();
        encode(&mut out, message).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches('\n').count(), 1, "one line: {text:?}");
        assert!(text.ends_with('\n'));
        text.trim_end_matches('\n').to_owned()
    }

    #[test]
    fn every_view_survives_the_round_trip() {
        let views = [
            View::Idle,
            View::Listening {
                level: 0.4,
                text: String::new(),
            },
            View::Listening {
                level: 1.0,
                text: "第一行\n第二行 \"quoted\" \\ tab\t".into(),
            },
            View::Finishing {
                text: "nearly done".into(),
            },
            View::Done {
                text: "最终结果 👩‍💻".into(),
                label: "已粘贴".into(),
            },
            View::Done {
                text: String::new(),
                label: "Copied".into(),
            },
            View::Error {
                message: "认证失败".into(),
                hint: "Press R to retry".into(),
            },
        ];
        for v in views {
            assert_eq!(decode(&line(&view(&v))).unwrap(), Update::View(v));
        }
    }

    #[test]
    fn style_and_labels_survive_the_round_trip() {
        for position in Position::ALL {
            let s = Style {
                position,
                scale: 1.25,
                idle_visible: true,
                show_text: false,
            };
            assert_eq!(decode(&line(&style(&s))).unwrap(), Update::Style(s));
        }
        let l = Labels {
            listening: "正在聆听…".into(),
            recognizing: "正在识别…".into(),
            idle: "Anya".into(),
        };
        assert_eq!(decode(&line(&labels(&l))).unwrap(), Update::Labels(l));
    }

    #[test]
    fn the_wire_shape_is_tagged_by_cmd() {
        let v = View::Listening {
            level: 0.5,
            text: "hi".into(),
        };
        assert_eq!(
            line(&view(&v)),
            r#"{"cmd":"listening","level":0.5,"text":"hi"}"#
        );
        assert_eq!(line(&view(&View::Idle)), r#"{"cmd":"idle"}"#);
        let s = Style {
            position: Position::BottomRight,
            scale: 1.0,
            idle_visible: false,
            show_text: true,
        };
        assert_eq!(
            line(&style(&s)),
            r#"{"cmd":"style","position":"bottom-right","scale":1.0,"idle_visible":false,"show_text":true}"#
        );
    }

    #[test]
    fn levels_and_scales_are_made_drawable() {
        let v = View::Listening {
            level: f32::NAN,
            text: String::new(),
        };
        assert_eq!(
            decode(&line(&view(&v))).unwrap(),
            Update::View(View::Listening {
                level: 0.0,
                text: String::new()
            })
        );
        let loud = r#"{"cmd":"listening","level":7.5,"text":""}"#;
        assert_eq!(
            decode(loud).unwrap(),
            Update::View(View::Listening {
                level: 1.0,
                text: String::new()
            })
        );
        for bad in [0.0, -2.0, f64::INFINITY, f64::NAN] {
            let s = Style {
                position: Position::Notch,
                scale: bad,
                idle_visible: false,
                show_text: true,
            };
            let Update::Style(back) = decode(&line(&style(&s))).unwrap() else {
                panic!("not a style");
            };
            assert_eq!(back.scale, 1.0);
        }
    }

    #[test]
    fn unreadable_lines_are_errors() {
        assert!(matches!(decode("not json"), Err(DecodeError::Json(_))));
        assert!(matches!(
            decode(r#"{"cmd":"dance"}"#),
            Err(DecodeError::Json(_))
        ));
        assert!(matches!(
            decode(r#"{"cmd":"done","text":"x"}"#),
            Err(DecodeError::Json(_))
        ));
        assert!(matches!(
            decode(
                r#"{"cmd":"style","position":"middle","scale":1,"idle_visible":false,"show_text":true}"#
            ),
            Err(DecodeError::Position(_))
        ));
    }
}
