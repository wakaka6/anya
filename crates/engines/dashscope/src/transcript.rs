//! A session's sentences, turned into [`Recognition`]s.

use anya_protocol::{Recognition, Utterance};

/// The sentences the service has ended, which never change again, and the
/// one it is still revising.
#[derive(Debug, Default)]
pub(crate) struct Transcript {
    ended: Vec<String>,
    pending: String,
}

impl Transcript {
    /// Takes one `result-generated` sentence. Returns the session's result
    /// when something changed.
    pub(crate) fn update(&mut self, text: &str, sentence_end: bool) -> Option<Recognition> {
        let text = text.trim();
        if sentence_end {
            if text.is_empty() && self.pending.is_empty() {
                return None;
            }
            if !text.is_empty() {
                self.ended.push(text.to_owned());
            }
            self.pending.clear();
        } else {
            if text.is_empty() || text == self.pending {
                return None;
            }
            text.clone_into(&mut self.pending);
        }
        Some(self.recognition(false))
    }

    /// The final result: every sentence, the unfinished one included, definite.
    pub(crate) fn finish(&self) -> Recognition {
        self.recognition(true)
    }

    fn recognition(&self, is_final: bool) -> Recognition {
        let mut utterances: Vec<Utterance> = self
            .ended
            .iter()
            .map(|text| Utterance {
                text: text.clone(),
                definite: true,
            })
            .collect();
        if !self.pending.is_empty() {
            utterances.push(Utterance {
                text: self.pending.clone(),
                definite: is_final,
            });
        }
        let mut text = String::new();
        for u in &utterances {
            if needs_space(&text, &u.text) {
                text.push(' ');
            }
            text.push_str(&u.text);
        }
        Recognition {
            text,
            utterances,
            is_final,
        }
    }
}

/// Sentences in scripts written with spaces (`Hello.` `How are you?`) get one
/// between them; Chinese and Japanese sentences join directly.
fn needs_space(before: &str, next: &str) -> bool {
    match (before.chars().last(), next.chars().next()) {
        (Some(a), Some(b)) => a.is_ascii_graphic() && b.is_ascii_alphanumeric(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(r: &Recognition) -> (&str, Vec<(&str, bool)>, bool) {
        let u = r
            .utterances
            .iter()
            .map(|u| (u.text.as_str(), u.definite))
            .collect();
        (r.text.as_str(), u, r.is_final)
    }

    #[test]
    fn ended_sentences_are_definite_and_kept() {
        let mut t = Transcript::default();
        let r = t.update("你好", false).unwrap();
        assert_eq!(shape(&r), ("你好", vec![("你好", false)], false));
        let r = t.update("你好。", true).unwrap();
        assert_eq!(shape(&r), ("你好。", vec![("你好。", true)], false));
        let r = t.update(" 今天 ", false).unwrap();
        assert_eq!(
            shape(&r),
            ("你好。今天", vec![("你好。", true), ("今天", false)], false)
        );
        let r = t.update("今天天气", false).unwrap();
        assert_eq!(
            shape(&r),
            (
                "你好。今天天气",
                vec![("你好。", true), ("今天天气", false)],
                false
            )
        );
    }

    #[test]
    fn a_sentence_ending_as_it_was_last_heard_is_kept() {
        let mut t = Transcript::default();
        t.update("好的", false);
        let r = t.update("好的", true).unwrap();
        assert_eq!(shape(&r), ("好的", vec![("好的", true)], false));
    }

    #[test]
    fn nothing_new_gives_no_result() {
        let mut t = Transcript::default();
        assert!(t.update("", false).is_none());
        assert!(t.update("", true).is_none());
        t.update("a", false);
        assert!(t.update("a", false).is_none());
        assert!(t.update("  ", false).is_none());
        // An ended sentence with no text drops what was pending.
        let r = t.update("", true).unwrap();
        assert_eq!(shape(&r), ("", vec![], false));
    }

    #[test]
    fn finish_makes_everything_definite() {
        let mut t = Transcript::default();
        t.update("One.", true);
        t.update("two", false);
        let r = t.finish();
        assert_eq!(
            shape(&r),
            ("One. two", vec![("One.", true), ("two", true)], true)
        );
        assert_eq!(shape(&Transcript::default().finish()), ("", vec![], true));
    }

    #[test]
    fn spaces_only_between_spaced_scripts() {
        let mut t = Transcript::default();
        t.update("Hello world.", true);
        t.update("你好。", true);
        t.update("OK", true);
        let r = t.update("3 apples", true).unwrap();
        assert_eq!(r.text, "Hello world.你好。OK 3 apples");
    }
}
