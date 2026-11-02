//! Hotwords from the config and from `hotwords.txt` next to the config file.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::Recognition;

/// The hotwords file's name, in the config file's directory.
pub const HOTWORDS_FILE: &str = "hotwords.txt";

/// The hotwords file that goes with the config file at `config_path`.
pub fn hotwords_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(HOTWORDS_FILE)
}

/// What reading the hotwords file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotwordsFile {
    /// There is none: the config's own words are used alone.
    None,
    /// Read; its words are merged in.
    Read,
    /// It is there but could not be read; used as if there were none.
    Unreadable {
        /// Where it is.
        path: PathBuf,
        /// Why, for the log.
        error: String,
    },
}

/// The words to favour: the config's, then those in `file`, trimmed, `#`
/// comments and blank lines dropped, duplicates removed keeping the first.
pub fn read_hotwords(recognition: &Recognition, file: &Path) -> (Vec<String>, HotwordsFile) {
    let mut words = Vec::new();
    let mut push = |w: &str| {
        let w = w.trim();
        if !w.is_empty() && !words.iter().any(|x| x == w) {
            words.push(w.to_owned());
        }
    };
    recognition.hotwords.iter().for_each(|w| push(w));
    match std::fs::read_to_string(file) {
        Ok(text) => {
            text.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .for_each(push);
            (words, HotwordsFile::Read)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => (words, HotwordsFile::None),
        Err(e) => (
            words,
            HotwordsFile::Unreadable {
                path: file.to_owned(),
                error: e.to_string(),
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_adds_to_the_config_without_repeats() {
        let dir = std::env::temp_dir().join(format!("anya-hotwords-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = hotwords_path(&dir.join("config.toml"));
        assert_eq!(path, dir.join("hotwords.txt"));
        std::fs::write(&path, "# team names\nAnya\n\n  Bond \nLoid\n").expect("write");
        let r = Recognition {
            hotwords: vec!["Loid".into(), " ".into()],
            ..Recognition::default()
        };
        assert_eq!(
            read_hotwords(&r, &path),
            (
                vec!["Loid".into(), "Anya".into(), "Bond".into()],
                HotwordsFile::Read
            )
        );

        std::fs::remove_file(&path).expect("remove");
        assert_eq!(
            read_hotwords(&r, &path),
            (vec!["Loid".into()], HotwordsFile::None),
            "no file is not an error"
        );

        std::fs::create_dir(&path).expect("a directory in its place");
        let (words, file) = read_hotwords(&r, &path);
        assert_eq!(words, ["Loid"]);
        assert!(matches!(file, HotwordsFile::Unreadable { .. }), "{file:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
