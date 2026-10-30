<!-- reviewed-through: 190033b (2026-10-30) -->
# Lessons from merged work

Anya's code is written and reviewed largely by agents. A review of recent
commits asks three things: what had to be fixed again, what should not have
merged, and what was verified too thinly. What it finds is kept here, so the
next change starts from it. Read this before changing code.

Each lesson is a rule, then why, then the evidence. "Seen" counts the separate
days a review found it. A lesson seen on 3 or more days moves into
[docs/code-standards.md](docs/code-standards.md) as a rule (and leaves this file's list, keeping one line under "Moved").

## Wiring

**A signal needs a test that makes it fire.** A value that tells a task to
act is only wired when something sends it on the path the user takes.
- Continuous transcription reconnected on a `params` bump that was sent only
  after the dictation `finished` channel closed, which happens at shutdown.
  Changing a hotwords file never reached a long session (190033b).
- Prove it by driving the trigger, not by reading that the receiver exists.
- Seen 1× (2026-10-30).

**A literal where a measured value belongs is a bug that compiles.**
- `stats.record(&done.text, 0)` passed 0 for the audio time while
  `Finished.audio_ms` was available. The TUI, the settings page and the tray
  panel all rendered the totals, so every speed and minute count read 0
  without an error anywhere (190033b).
- When a function takes a number, grep its callers for literals.
- Seen 1× (2026-10-30).

## Working in a shared tree

**Commit what you changed, and look at what the commit contains first.**
- `crates/anya/src/main.rs` was untracked, so committing it took the whole
  1355-line file, and `CHANGELOG.md` carried 26 lines another session had
  written. The commit does not build alone: it needs `indicator.rs`,
  `plugins.rs` and `timing.rs` (190033b).
- Run `git status` and `git diff --stat` for the files, name what else comes
  with them, and say so in the report.
- Seen 1× (2026-10-30).

## Moved to the code standards

Nothing yet.
