//! The replacement list: `replacements.txt` next to the config file.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The replacement list's name, in the config file's directory.
pub const REPLACEMENTS_FILE: &str = "replacements.txt";

/// The replacement list that goes with the config file at `config_path`.
pub fn replacements_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(REPLACEMENTS_FILE)
}

/// One line of the list: what is recognised and what is written instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// The words as the engine writes them.
    pub spoken: String,
    /// What takes their place; `\n` and `\t` are already a newline and a tab.
    pub written: String,
}

/// What reading the replacement list found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplacementsFile {
    /// There is none: nothing is replaced.
    None,
    /// Read.
    Read,
    /// It is there but could not be read; used as if there were none.
    Unreadable {
        /// Where it is.
        path: PathBuf,
        /// Why, for the log.
        error: String,
    },
}

/// The rules in `text`: `spoken => written` per line, `#` comments, blank
/// lines and lines with no `=>` or an empty spoken side dropped. The spoken
/// side is trimmed; the written side is trimmed, then `\n`, `\t` and `\\`
/// become the character they name.
pub fn parse_replacements(text: &str) -> Vec<Replacement> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|line| {
            let (spoken, written) = line.split_once("=>")?;
            let spoken = spoken.trim();
            (!spoken.is_empty()).then(|| Replacement {
                spoken: spoken.to_owned(),
                written: unescape(written.trim()),
            })
        })
        .collect()
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The rules in `file`; a missing file has none.
pub fn read_replacements(file: &Path) -> (Vec<Replacement>, ReplacementsFile) {
    match std::fs::read_to_string(file) {
        Ok(text) => (parse_replacements(&text), ReplacementsFile::Read),
        Err(e) if e.kind() == ErrorKind::NotFound => (Vec::new(), ReplacementsFile::None),
        Err(e) => (
            Vec::new(),
            ReplacementsFile::Unreadable {
                path: file.to_owned(),
                error: e.to_string(),
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(spoken: &str, written: &str) -> Replacement {
        Replacement {
            spoken: spoken.into(),
            written: written.into(),
        }
    }

    #[test]
    fn lines_become_rules_and_the_rest_is_ignored() {
        let text = "# names\nanya => Anya\n\n  new line => \\n \nno arrow\n => nothing\ntab=>a\\tb\nback => \\\\n\nkeep => \\q\n";
        assert_eq!(
            parse_replacements(text),
            [
                rule("anya", "Anya"),
                rule("new line", "\n"),
                rule("tab", "a\tb"),
                rule("back", "\\n"),
                rule("keep", "\\q"),
            ]
        );
    }

    #[test]
    fn a_rule_may_delete_what_it_matches() {
        assert_eq!(parse_replacements("um =>\n"), [rule("um", "")]);
    }

    #[test]
    fn the_file_sits_beside_the_config_and_may_be_missing_or_unreadable() {
        let dir = std::env::temp_dir().join(format!("anya-replacements-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = replacements_path(&dir.join("config.toml"));
        assert_eq!(path, dir.join("replacements.txt"));
        assert_eq!(
            read_replacements(&path),
            (Vec::new(), ReplacementsFile::None)
        );
        std::fs::write(&path, "a => b\n").expect("write");
        assert_eq!(
            read_replacements(&path),
            (vec![rule("a", "b")], ReplacementsFile::Read)
        );
        std::fs::remove_file(&path).expect("remove");
        std::fs::create_dir(&path).expect("a directory in its place");
        let (rules, file) = read_replacements(&path);
        assert!(rules.is_empty());
        assert!(
            matches!(file, ReplacementsFile::Unreadable { .. }),
            "{file:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
