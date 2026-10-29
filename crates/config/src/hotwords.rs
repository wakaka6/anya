//! Hotwords from the config and from `[recognition] hotwords_file`.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::{Recognition, expand_home};

/// What reading the hotwords file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotwordsFile {
    /// No file is configured.
    None,
    /// Read; its words are merged in.
    Read,
    /// It could not be read; the config's own words are used alone.
    Unreadable {
        /// The expanded path.
        path: PathBuf,
        /// Why, for the log.
        error: String,
    },
}

/// The words to favour: the config's, then the file's, trimmed, `#` comments
/// and blank lines dropped, duplicates removed keeping the first.
pub fn read_hotwords(recognition: &Recognition) -> (Vec<String>, HotwordsFile) {
    let mut words = Vec::new();
    let mut push = |w: &str| {
        let w = w.trim();
        if !w.is_empty() && !words.iter().any(|x| x == w) {
            words.push(w.to_owned());
        }
    };
    recognition.hotwords.iter().for_each(|w| push(w));
    let file = recognition.hotwords_file.trim();
    if file.is_empty() {
        return (words, HotwordsFile::None);
    }
    let path = expand_home(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            text.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .for_each(push);
            (words, HotwordsFile::Read)
        }
        Err(e) => (
            words,
            HotwordsFile::Unreadable {
                path,
                error: e.to_string(),
            },
        ),
    }
}

impl Recognition {
    /// When the hotwords file last changed, so a long session can tell it
    /// needs to reconnect; `None` without a readable file.
    pub fn hotwords_stamp(&self) -> Option<SystemTime> {
        let file = self.hotwords_file.trim();
        if file.is_empty() {
            return None;
        }
        std::fs::metadata(expand_home(file))
            .and_then(|m| m.modified())
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_adds_to_the_config_without_repeats() {
        let dir = std::env::temp_dir().join(format!("anya-hotwords-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("words.txt");
        std::fs::write(&path, "# team names\nAnya\n\n  Bond \nLoid\n").expect("write");
        let r = Recognition {
            hotwords: vec!["Loid".into(), " ".into()],
            hotwords_file: path.display().to_string(),
            ..Recognition::default()
        };
        assert_eq!(
            read_hotwords(&r),
            (
                vec!["Loid".into(), "Anya".into(), "Bond".into()],
                HotwordsFile::Read
            )
        );
        assert!(r.hotwords_stamp().is_some());

        let gone = Recognition {
            hotwords_file: dir.join("gone").display().to_string(),
            ..r
        };
        let (words, file) = read_hotwords(&gone);
        assert_eq!(words, ["Loid"]);
        assert!(matches!(file, HotwordsFile::Unreadable { .. }));
    }
}
