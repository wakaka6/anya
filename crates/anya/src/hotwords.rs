//! Watches the merged hotwords (the config's and `hotwords.txt`'s) so a long
//! continuous session reconnects once to take a change, and says when the
//! hotwords file is there but cannot be read.
//!
//! A change counts once it has held for a whole poll, so a save caught half
//! written does not reconnect twice, and reconnects are at least [`GAP`]
//! apart, so a burst of edits makes one. A file that cannot be read counts
//! as no file, as it does when a session opens.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anya_config::{Config, HotwordsFile};
use tokio::sync::watch;

use crate::lock;

/// How often the hotwords are read.
pub const POLL: Duration = Duration::from_secs(2);

/// The least time between two reconnects for changed hotwords.
pub const GAP: Duration = Duration::from_secs(30);

/// Decides, read by read, when to reconnect and when to report.
pub struct Tracker {
    /// The words sessions were last given: at start, or at the last reconnect.
    given: Vec<String>,
    /// The words the last read found.
    seen: Option<Vec<String>>,
    /// When the last reconnect was asked for.
    reconnected: Option<Instant>,
    /// Unreadable reads in a row.
    misses: u32,
    gap: Duration,
}

impl Tracker {
    /// Starts from the words sessions open with now.
    pub fn new(given: Vec<String>, gap: Duration) -> Self {
        Self {
            given,
            seen: None,
            reconnected: None,
            misses: 0,
            gap,
        }
    }

    /// Takes the words one read at `now` found; true when the long session
    /// should reconnect to take them.
    pub fn words(&mut self, words: Vec<String>, now: Instant) -> bool {
        if self.seen.as_ref() != Some(&words) {
            self.seen = Some(words);
            return false;
        }
        if words == self.given
            || self
                .reconnected
                .is_some_and(|at| now.duration_since(at) < self.gap)
        {
            return false;
        }
        self.given = words;
        self.reconnected = Some(now);
        true
    }

    /// Takes what one read found of the file; the path and the reason once
    /// it has been unreadable twice in a row, once per such stretch.
    pub fn file(&mut self, file: HotwordsFile) -> Option<(PathBuf, String)> {
        let HotwordsFile::Unreadable { path, error } = file else {
            self.misses = 0;
            return None;
        };
        self.misses += 1;
        // One miss can be an editor between deleting and writing.
        (self.misses == 2).then_some((path, error))
    }
}

/// Reads the hotwords in `file` every `poll`, bumps `params` for a reconnect
/// and calls `report` with the path and the reason when the file is there
/// but cannot be read.
pub async fn watch(
    config: Arc<Mutex<Config>>,
    file: PathBuf,
    params: watch::Sender<u64>,
    report: impl Fn(PathBuf, String) + Send + 'static,
    poll: Duration,
    gap: Duration,
) {
    let mut tracker: Option<Tracker> = None;
    let mut tick = tokio::time::interval(poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let shared = config.clone();
        let path = file.clone();
        let Ok((words, found)) = tokio::task::spawn_blocking(move || {
            let recognition = lock(&shared).recognition.clone();
            anya_config::read_hotwords(&recognition, &path)
        })
        .await
        else {
            return;
        };
        let tracker = tracker.get_or_insert_with(|| Tracker::new(words.clone(), gap));
        if let Some((path, error)) = tracker.file(found) {
            tracing::warn!(path = %path.display(), %error, "cannot read the hotwords file");
            report(path, error);
        }
        if tracker.words(words, Instant::now()) {
            tracing::info!("hotwords changed, reconnecting the continuous session");
            params.send_modify(|value| *value = value.wrapping_add(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|&w| w.to_owned()).collect()
    }

    fn unreadable() -> HotwordsFile {
        HotwordsFile::Unreadable {
            path: PathBuf::from("/home/u/.config/anya/hotwords.txt"),
            error: "Is a directory (os error 21)".to_owned(),
        }
    }

    #[test]
    fn an_edit_reconnects_once_after_it_held_for_a_poll() {
        let at = Instant::now();
        let mut t = Tracker::new(words(&["Anya"]), GAP);
        assert!(!t.words(words(&["Anya"]), at));
        assert!(!t.words(words(&["Anya", "Bond"]), at));
        assert!(t.words(words(&["Anya", "Bond"]), at));
        for _ in 0..5 {
            assert!(!t.words(words(&["Anya", "Bond"]), at));
        }
    }

    #[test]
    fn a_save_caught_half_written_reconnects_once() {
        let at = Instant::now();
        let mut t = Tracker::new(words(&["Anya", "Bond"]), Duration::ZERO);
        let reads: [&[&str]; 6] = [
            &["Anya", "Bond"],
            &["Anya"],
            &["Anya", "Bond", "Loid"],
            &["Anya", "Bond", "Loid"],
            &["Anya", "Bond", "Loid"],
            &["Anya", "Bond", "Loid"],
        ];
        let reconnects = reads.iter().filter(|w| t.words(words(w), at)).count();
        assert_eq!(reconnects, 1);
    }

    #[test]
    fn edits_within_the_gap_make_one_more_reconnect_after_it() {
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_secs(s);
        let mut t = Tracker::new(words(&["Anya"]), GAP);
        t.words(words(&["Anya", "Bond"]), at(0));
        assert!(t.words(words(&["Anya", "Bond"]), at(2)));
        let mut reconnects = vec![];
        for s in (4..=40).step_by(2) {
            let w: &[&str] = if s < 20 {
                &["Anya", "Bond", "Loid"]
            } else {
                &["Anya", "Bond", "Loid", "Yor"]
            };
            if t.words(words(w), at(s)) {
                reconnects.push(s);
            }
        }
        assert_eq!(reconnects, [32], "the first poll past the gap");
    }

    #[test]
    fn an_unreadable_file_is_reported_once_a_stretch() {
        let mut t = Tracker::new(vec![], GAP);
        // A delete-then-write save: one miss is not reported.
        assert_eq!(t.file(unreadable()), None);
        assert_eq!(t.file(HotwordsFile::Read), None);
        let reports = (0..10).filter_map(|_| t.file(unreadable())).count();
        assert_eq!(reports, 1);
        assert_eq!(t.file(HotwordsFile::None), None);
        t.file(unreadable());
        assert!(t.file(unreadable()).is_some(), "a new stretch is reported");
    }

    #[tokio::test]
    async fn the_watcher_follows_the_file_and_reports_one_it_cannot_read() {
        let dir = std::env::temp_dir().join(format!("anya-hotwords-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let file = anya_config::hotwords_path(&dir.join("config.toml"));
        let mut cfg = Config::default();
        cfg.recognition.hotwords = words(&["Anya"]);
        let (tx, mut rx) = watch::channel(0_u64);
        let reports: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let seen = reports.clone();
        let task = tokio::spawn(watch(
            Arc::new(Mutex::new(cfg)),
            file.clone(),
            tx,
            move |path, _| lock(&seen).push(path),
            Duration::from_millis(10),
            Duration::ZERO,
        ));
        let reported = async |n: usize| {
            tokio::time::timeout(Duration::from_secs(5), async {
                while lock(&reports).len() < n {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the unreadable file is reported");
        };
        async fn bumped(rx: &mut watch::Receiver<u64>, n: u64) {
            tokio::time::timeout(Duration::from_secs(5), rx.changed())
                .await
                .expect("the change is noticed")
                .expect("open");
            assert_eq!(*rx.borrow_and_update(), n);
        }

        // Reported, so the watcher has read once: what comes next is a change.
        std::fs::create_dir(&file).expect("a directory in its place");
        reported(1).await;
        assert!(!rx.has_changed().expect("open"), "same words as at start");
        std::fs::remove_dir(&file).expect("remove");
        std::fs::write(&file, "Bond\n").expect("write");
        bumped(&mut rx, 1).await;
        // Unreadable is as good as none: back to the config's words alone.
        std::fs::remove_file(&file).expect("remove");
        std::fs::create_dir(&file).expect("a directory in its place");
        bumped(&mut rx, 2).await;
        reported(2).await;
        assert_eq!(lock(&reports).as_slice(), [file.clone(), file]);
        task.abort();
        let _ = task.await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
