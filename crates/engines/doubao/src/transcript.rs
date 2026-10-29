//! Turns the service's responses into [`Recognition`]s that keep the
//! subscriber promises.
//!
//! Each response carries the session's whole text and its sentences, each
//! marked `definite` once the service (after its second pass, when on) will
//! not change it. A sentence reported definite is settled here: it keeps its
//! place and its text in every later result even if the service sends it
//! differently, and only an unbroken run of definite sentences from the start
//! is reported definite, so a sentence that could still split or merge never
//! is.

use anya_protocol::{Recognition, Utterance};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct Response {
    #[serde(default)]
    result: Option<Body>,
}

#[derive(Debug, Default, Deserialize)]
struct Body {
    #[serde(default)]
    text: String,
    #[serde(default)]
    utterances: Vec<Sentence>,
}

#[derive(Debug, Deserialize)]
struct Sentence {
    #[serde(default)]
    text: String,
    #[serde(default)]
    definite: bool,
}

/// One session's results so far.
#[derive(Debug, Default)]
pub(crate) struct Transcript {
    /// Sentences reported definite, in order.
    settled: Vec<Utterance>,
    /// The last result reported.
    latest: Option<Recognition>,
}

impl Transcript {
    /// Maps one response's JSON payload; `last` is the service's last packet.
    /// `None` when there is nothing to report: an acknowledgement, or empty
    /// text before the end.
    pub(crate) fn apply(&mut self, payload: &[u8], last: bool) -> Option<Recognition> {
        let body = serde_json::from_slice::<Response>(payload)
            .ok()
            .and_then(|r| r.result)
            .unwrap_or_default();
        if last && body.text.is_empty() && body.utterances.is_empty() {
            // An empty last packet ends the session on what was said so far.
            let mut done = self.latest.take().unwrap_or_else(|| Recognition {
                text: String::new(),
                utterances: Vec::new(),
                is_final: false,
            });
            done.utterances.iter_mut().for_each(|u| u.definite = true);
            done.is_final = true;
            return Some(done);
        }

        let kept = body.utterances.len() >= self.settled.len()
            && body
                .utterances
                .iter()
                .zip(&self.settled)
                .all(|(s, u)| s.text == u.text);
        let mut utterances = self.settled.clone();
        let mut open = false;
        for s in body.utterances.iter().skip(self.settled.len()) {
            let definite = s.definite && !open;
            open |= !definite;
            let u = Utterance {
                text: s.text.clone(),
                definite: definite || last,
            };
            if definite {
                self.settled.push(u.clone());
            }
            utterances.push(u);
        }
        let text = if kept {
            body.text
        } else {
            utterances.iter().map(|u| u.text.as_str()).collect()
        };
        if text.is_empty() && !last {
            return None;
        }
        let recognition = Recognition {
            text,
            utterances,
            is_final: last,
        };
        self.latest = Some(recognition.clone());
        Some(recognition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(text: &str, utterances: &[(&str, bool)]) -> Vec<u8> {
        let utterances: Vec<_> = utterances
            .iter()
            .map(|(t, d)| serde_json::json!({ "text": t, "definite": d, "start_time": 0 }))
            .collect();
        serde_json::json!({
            "audio_info": { "duration": 1000 },
            "result": { "text": text, "utterances": utterances },
        })
        .to_string()
        .into_bytes()
    }

    fn sentences(r: &Recognition) -> Vec<(&str, bool)> {
        r.utterances
            .iter()
            .map(|u| (u.text.as_str(), u.definite))
            .collect()
    }

    #[test]
    fn acknowledgements_and_empty_partials_report_nothing() {
        let mut t = Transcript::default();
        assert_eq!(t.apply(br#"{"audio_info":{"duration":0}}"#, false), None);
        assert_eq!(t.apply(&payload("", &[]), false), None);
        assert_eq!(t.apply(b"", false), None);
    }

    #[test]
    fn definite_sentences_stay_as_first_reported() {
        let mut t = Transcript::default();
        let r = t.apply(&payload("a", &[("a", false)]), false);
        assert_eq!(r.as_ref().map(sentences), Some(vec![("a", false)]));
        let r = t.apply(&payload("A. b", &[("A.", true), ("b", false)]), false);
        assert_eq!(
            r.as_ref().map(sentences),
            Some(vec![("A.", true), ("b", false)])
        );
        assert_eq!(r.map(|r| r.text), Some("A. b".into()));

        // The service sends the settled sentence revised: it is kept.
        let r = t.apply(&payload("Ah. be", &[("Ah.", true), ("be", false)]), false);
        assert_eq!(
            r.as_ref().map(sentences),
            Some(vec![("A.", true), ("be", false)])
        );
        assert_eq!(r.map(|r| r.text), Some("A.be".into()));
    }

    #[test]
    fn a_definite_sentence_after_an_open_one_is_not_definite_yet() {
        let mut t = Transcript::default();
        let r = t.apply(
            &payload("ab.c.", &[("a", false), ("b.", true), ("c.", true)]),
            false,
        );
        assert_eq!(
            r.as_ref().map(sentences),
            Some(vec![("a", false), ("b.", false), ("c.", false)])
        );
        let r = t.apply(
            &payload("a.b.c.", &[("a.", true), ("b.", true), ("c.", true)]),
            false,
        );
        assert_eq!(
            r.as_ref().map(sentences),
            Some(vec![("a.", true), ("b.", true), ("c.", true)])
        );
    }

    #[test]
    fn the_last_packet_is_final_and_settles_everything() {
        let mut t = Transcript::default();
        let r = t.apply(&payload("a. b", &[("a.", true), ("b", false)]), true);
        assert_eq!(r.as_ref().map(|r| r.is_final), Some(true));
        assert_eq!(
            r.as_ref().map(sentences),
            Some(vec![("a.", true), ("b", true)])
        );
    }

    #[test]
    fn an_empty_last_packet_finalizes_the_latest_result() {
        let mut t = Transcript::default();
        let _ = t.apply(&payload("a. b", &[("a.", true), ("b", false)]), false);
        let r = t.apply(b"", true);
        assert_eq!(r.as_ref().map(|r| r.text.as_str()), Some("a. b"));
        assert_eq!(
            r.as_ref().map(sentences),
            Some(vec![("a.", true), ("b", true)])
        );
        assert_eq!(r.map(|r| r.is_final), Some(true));

        let mut silent = Transcript::default();
        let r = silent.apply(&payload("", &[]), true);
        assert_eq!(
            r,
            Some(Recognition {
                text: String::new(),
                utterances: Vec::new(),
                is_final: true
            })
        );
    }
}
