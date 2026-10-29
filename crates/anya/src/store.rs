//! What a running Anya keeps on disk between runs: usage totals
//! (`<state>/stats.json`, shared by every config) and the activity list
//! (`<state>/<config name>.activity.jsonl`, one per config file).
//!
//! Both are written atomically and readable by their owner only. Writes run
//! on a background thread, so recording a session never waits on the disk;
//! when writes queue up, only the newest snapshot is written.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};

use anya_runtime::{Entry, Stats};
use serde::{Deserialize, Serialize};

/// Above this many entries the activity list keeps only the newest [`KEEP`].
pub const CAP: usize = 200;
/// What the activity list keeps when it passes [`CAP`].
pub const KEEP: usize = 100;

/// Writes `bytes` to `path` through a temp file and a rename, mode 0600,
/// creating the directory.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map_or_else(|| "anya".into(), |n| n.to_string_lossy());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// A background writer: the newest bytes sent win.
#[derive(Clone)]
struct Writer {
    tx: mpsc::Sender<Vec<u8>>,
}

impl Writer {
    fn spawn(path: PathBuf, what: &'static str) -> Writer {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name(format!("anya-{what}"))
            .spawn(move || {
                while let Ok(mut bytes) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        bytes = newer;
                    }
                    if let Err(e) = atomic_write(&path, &bytes) {
                        tracing::warn!(path = %path.display(), error = %e, "saving {what} failed");
                    }
                }
            })
            .map_or_else(
                |e| {
                    tracing::warn!(error = %e, "no {what} writer; it is kept in memory only");
                },
                drop,
            );
        Writer { tx }
    }

    fn write(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(bytes);
    }
}

#[derive(Serialize, Deserialize, Default)]
struct StatsFile {
    sessions: u64,
    chars: u64,
    audio_ms: u64,
}

/// Usage totals across restarts.
#[derive(Clone)]
pub struct StatsStore {
    stats: Arc<Mutex<Stats>>,
    writer: Option<Writer>,
}

impl StatsStore {
    /// Loads `path`; a missing or unreadable file starts at zero.
    pub fn open(path: PathBuf) -> StatsStore {
        let file: StatsFile = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        StatsStore {
            stats: Arc::new(Mutex::new(Stats {
                sessions: file.sessions,
                chars: file.chars,
                audio_ms: file.audio_ms,
                last_chars: 0,
                last_audio_ms: 0,
            })),
            writer: Some(Writer::spawn(path, "stats")),
        }
    }

    /// Totals kept in memory only, for tests and `--tui` previews.
    pub fn in_memory() -> StatsStore {
        StatsStore {
            stats: Arc::default(),
            writer: None,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Stats> {
        self.stats.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The totals now.
    pub fn get(&self) -> Stats {
        *self.lock()
    }

    /// Counts one dictated session: its text (characters other than
    /// whitespace) and the audio it took from key press to delivery.
    pub fn record(&self, text: &str, audio_ms: u64) {
        let chars = text.chars().filter(|c| !c.is_whitespace()).count() as u64;
        let snapshot = {
            let mut s = self.lock();
            s.sessions += 1;
            s.chars += chars;
            s.audio_ms += audio_ms;
            s.last_chars = chars;
            s.last_audio_ms = audio_ms;
            *s
        };
        if let Some(w) = &self.writer {
            let file = StatsFile {
                sessions: snapshot.sessions,
                chars: snapshot.chars,
                audio_ms: snapshot.audio_ms,
            };
            if let Ok(bytes) = serde_json::to_vec_pretty(&file) {
                w.write(bytes);
            }
        }
    }
}

/// The activity list across restarts.
#[derive(Clone)]
pub struct ActivityStore {
    entries: Arc<Mutex<Vec<Entry>>>,
    writer: Option<Writer>,
}

/// The activity file for the config at `config`: its name without the
/// extension, `config` when it has none.
pub fn activity_path(state: &Path, config: &Path) -> PathBuf {
    let stem = config
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty() && s != ".")
        .unwrap_or_else(|| "config".to_owned());
    state.join(format!("{stem}.activity.jsonl"))
}

impl ActivityStore {
    /// Loads `path`, skipping lines that do not parse.
    pub fn open(path: PathBuf) -> ActivityStore {
        let mut entries: Vec<Entry> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        trim(&mut entries);
        ActivityStore {
            entries: Arc::new(Mutex::new(entries)),
            writer: Some(Writer::spawn(path, "activity")),
        }
    }

    /// An activity list kept in memory only.
    pub fn in_memory() -> ActivityStore {
        ActivityStore {
            entries: Arc::default(),
            writer: None,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Every entry, oldest first.
    pub fn get(&self) -> Vec<Entry> {
        self.lock().clone()
    }

    /// Adds `entry` and saves the list in the background.
    pub fn push(&self, entry: Entry) {
        let snapshot = {
            let mut e = self.lock();
            e.push(entry);
            trim(&mut e);
            e.clone()
        };
        if let Some(w) = &self.writer {
            let mut bytes = Vec::new();
            for e in &snapshot {
                if serde_json::to_writer(&mut bytes, e).is_ok() {
                    bytes.push(b'\n');
                }
            }
            w.write(bytes);
        }
    }
}

fn trim(entries: &mut Vec<Entry>) {
    if entries.len() > CAP {
        entries.drain(..entries.len() - KEEP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anya_protocol::Source;
    use anya_runtime::{Delivered, Notice};
    use std::time::{Duration, Instant};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("anya-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn eventually(path: &Path, ok: impl Fn(&str) -> bool) -> String {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if ok(&text) || Instant::now() > until {
                return text;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn final_entry(n: u64) -> Entry {
        Entry::Final {
            at: n,
            source: Source::Dictation,
            text: format!("t{n}"),
            delivered: Delivered::Pasted,
        }
    }

    #[test]
    fn stats_survive_a_restart_but_the_last_session_does_not() {
        let d = dir("stats");
        let path = d.join("stats.json");
        let s = StatsStore::open(path.clone());
        s.record("你好 世界", 3000);
        s.record("ab c", 1000);
        assert_eq!(s.get().chars, 7);
        assert_eq!((s.get().last_chars, s.get().last_audio_ms), (3, 1000));
        eventually(&path, |t| t.contains("\"sessions\": 2"));
        let again = StatsStore::open(path.clone()).get();
        assert_eq!((again.sessions, again.chars, again.audio_ms), (2, 7, 4000));
        assert_eq!((again.last_chars, again.last_audio_ms), (0, 0));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn the_activity_list_keeps_the_newest_and_skips_broken_lines() {
        let d = dir("activity");
        let path = d.join("config.activity.jsonl");
        std::fs::create_dir_all(&d).expect("dir");
        let notice = serde_json::to_string(&Entry::Notice {
            at: 1,
            notice: Notice::ContinuousOn,
            detail: None,
        })
        .expect("json");
        std::fs::write(&path, format!("{notice}\nnot json\n{{\"kind\":\"final\"")).expect("write");
        let a = ActivityStore::open(path.clone());
        assert_eq!(a.get().len(), 1);
        for n in 0..CAP as u64 {
            a.push(final_entry(n));
        }
        let got = a.get();
        assert_eq!(got.len(), KEEP);
        assert_eq!(got.last(), Some(&final_entry(CAP as u64 - 1)));
        let text = eventually(&path, |t| t.lines().count() == KEEP && t.contains("t199"));
        assert_eq!(ActivityStore::open(path).get(), got, "{text}");
    }

    #[test]
    fn each_config_has_its_own_activity_file() {
        let state = Path::new("/s");
        assert_eq!(
            activity_path(state, Path::new("/c/work.toml")),
            Path::new("/s/work.activity.jsonl")
        );
        assert_eq!(
            activity_path(state, Path::new("/")),
            Path::new("/s/config.activity.jsonl")
        );
    }
}
