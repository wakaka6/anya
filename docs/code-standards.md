# Code standards

Rules a change is written and reviewed against. A reviewer checks each one,
and a change that misses one gets a comment saying which. They come from what
reviews kept finding in [LESSONS.md](../LESSONS.md); a lesson seen on 3 or
more separate days moves here. What the build already refuses (layer order,
`unwrap`, undocumented public items, platform bindings outside adapters) is in
AGENTS.md and `xtask`, and is not repeated.

## What a change contains

| Rule | What a reviewer looks at |
| --- | --- |
| A change does only what its goal needs. | Every change in behavior has a reason. Unrelated refactors, features and policy changes go in a commit of their own. |
| A fix covers exactly the cases it means to. | Which cases change, and which neighboring ones stay the same. Different errors are not handled as one. |
| A change holds on the whole path. | Follow it from the user's input (a hotkey, a paired app's audio) through the session, the frames and the sink to what the user sees. One correct function does not make the operation correct. |
| A signal needs a test that makes it fire. | A value that tells a task to act is wired only when something sends it on the path the user takes. Drive the trigger; reading that the receiver exists proves nothing. The continuous session reconnected on a `params` bump sent only when the dictation `finished` channel closed, which happens at shutdown, so a changed hotwords file never reached a long session (190033b). |
| A literal where a measured value belongs is a bug that compiles. | When a function takes a number, grep its callers for literals. `stats.record(&text, 0)` passed 0 while `Finished.audio_ms` was in hand, and the TUI, the settings page and the tray panel all showed speed and minutes as 0 with no error anywhere (190033b). |
| A fix reaches every sibling of the bug. | Grep for the same pattern: every engine, every backend (evdev, X11, CGEventTap), every UI (TUI, settings page, tray panel) that reads the changed value. The audio time above was read in three UIs; a fix in one would have left two wrong. |
| A change re-derives everything built on what it changes. | When a change alters a default, a list, a frame field or the timing of an async step, find every caller, saved copy, fixture and test that assumed the old meaning, and run it. |
| A failed read, parse or hash means "unknown". | It never means "empty" or "equal". An unreadable hotwords file keeps the config's own words and says so (`HotwordsFile::Unreadable`); it must not read as "no hotwords" and trigger a reconnect with a smaller list. A config that does not parse keeps the old config (`Reloaded::Broken`). |
| Another app's values come from that app. | When Anya speaks an engine's or a plugin's protocol, take each field's name, type and unit from that service's own docs or traffic, not from a guess that fits. Doubao's `resource_id`, `model_version` and second-pass flags are the vendor's; a similar-looking value is a silent accuracy loss. |
| Anya's default never overrides a value the user set. | Before writing a key, list everywhere it is read, and test with a user-set value present. A reload, a migration or a UI save that rewrites a setting says so, or leaves the user's value alone. |
| Nothing that can block runs where an OS callback or a lock's reader waits. | A hotkey callback records the edge and returns (a blocked event tap stalls every keystroke on the machine). Stopping capture, waiting for the final result and pasting run on a task. No send on a channel or call that can block while holding a lock the receiver takes first. |
| Privacy defaults hold (docs/decisions/0005). | Audio goes only to the engine the user chose, and a failing engine is an error the user sees, never a switch to another engine. Dictation frames reach an outside subscriber only with that app's grant. Transcripts and secrets are logged at debug level at most. |
| Downstream apps are users (docs/decisions/0007). | A frame change updates the types, the pins in `crates/protocol/tests/`, the fixtures, docs/protocol.md and CHANGELOG.md in one commit; every wire enum keeps its `Unknown` variant. Anything that emits frames is tested with `anya_protocol::contract::check`. |
| A preview, a dry run and a check write nothing. | `--doctor`, the overlay preview and a plugin search have no side effect on config, state or the clipboard. |
| Checks cover the edges the change touches. | For numbers, the boundaries. For platforms, each of macOS, Linux Wayland and Linux X11: a key macOS never reports (CapsLock, F21–F24) is refused with a reason, not silently ignored. For concurrency, races and stale state. |

## Tests

| Rule | What a reviewer looks at |
| --- | --- |
| A fix comes with a test that fails without it. | The reviewer puts the old code back and runs the test; it must fail. The description says what it failed with. A test named for a guarantee goes red when that guarantee is broken by hand. |
| Tests check what a consumer sees. | Frames, their ordering, errors, state transitions. Wiring and a dependency's own behaviour stay untested; formatting changes and internal refactors do not break a test, unless exact bytes are the requirement (frame JSON is pinned on purpose). |
| Tests use real inputs. | Fixtures look like real frames, real engine replies and real files on disk, including missing fields, empty values and defaults. Engine tests are built from bytes saved from the real service, not a similar sample made up. |
| A test never reaches the network. | An engine or the plugin registry is served by a local mock (`tests/mock/`). Break the change and watch where the test's requests go. |
| A timing test leaves room on both sides. | Never bound a test by wall-clock time a loaded runner can exceed. Wait for the state, or count what the test is about, not a duration. `a_changed_hotwords_file_bumps_the_params_once` sleeps a fixed 60 ms to show "nothing changed yet" and "no second bump": it passes on a quiet machine and is the kind of test this rule is about. |
| Whatever a test starts ends when the test does. | A task, a fake server or a timer a test sets going is stopped or awaited before it returns, or it writes into the next test's files. |
| Tests stay out of the real machine. | A temp directory, never the real `~/.config/anya` or the user's state directory. The hotwords test writes under `std::env::temp_dir()` and removes the file. |
| A test that fails on `main` is a bug to fix now. | "It fails the same on main" is not a baseline. Fix it at its cause, or file an issue naming the cause, in a commit of its own, before building on it. |

## Acceptance

A change is done only when it shows each of the following. Say which were run
and what they printed. A check that was not run is listed as not run, never
left out.

1. **A test that fails without the change.** Put the old code back and run
   the new test. It must fail with a real failure that names the behavior; a
   compile error does not count. Say which test and what it failed with, then
   restore the change.
2. **The whole gate.** `cargo xtask ci` (fmt, clippy `-D warnings`, tests with
   the layer rules, cargo-deny), not the crates you think you touched. Guard
   tests live in crates a change does not touch: frames are pinned in
   `protocol` and read by `server`, settings are read by every UI. In a chain
   of commands, `set -o pipefail`, so a failing test stops the push.
3. **The real surface.** A change to hotkeys, capture, output or the overlay
   is run on the platform it concerns, with the real hardware, permission and
   desktop (macOS: Accessibility and microphone grants; Linux: `input` group,
   `wtype` or uinput). A build for another target is not a test. A change to
   an engine is tried against the real service with a real key in a scratch
   config, never the user's running Anya. If it was not possible, say so; what
   was not tried is not told to users as working.
4. **Every string and doc follows.** New user-facing text is in every locale
   of `anya-i18n`; README.md and README.en.md change together; a user-visible
   change is in CHANGELOG.md; a reference in docs is checked against the code
   it names.
5. **The commit builds alone.** Its files compile and pass together at that
   commit, and `git show --stat HEAD` lists the files you changed and only
   those.

## Reviewing a change

A reviewer checks each pattern that applies:

- **Run it.** Pull the head, put it on current `main` and run the acceptance
  checks. Reading the diff alone is not a review.
- **Break it on purpose.** Revert the fix, or break the code it guards in
  several ways, and check the new tests catch each break.
- **Check the service, not the change's account of it.** For an engine,
  read the vendor's documentation or capture its traffic.
- **Check each platform and backend.** What is safe on macOS can wrap or fail
  on Linux; a key an evdev backend sees may be invisible to X11.
- **Classify narrowly.** An error class or a status covers only the case it
  describes; a plugin failing is not an engine being unavailable.
- **Do not reverse a deliberate decision.** `docs/decisions/` holds the
  reasons; changing one needs a new record that supersedes it.
- **Keep the change to its goal.** Unrelated tests and refactors go in their
  own commit. Comments name functions, not line numbers.

## Rust

- A comment says what the code is for, in a sentence or two. Name functions
  instead of giving line numbers, which go stale.
- Every `unsafe` block carries a `// SAFETY:` comment saying why its invariant
  holds.
- A seam gets a trait when a second adapter needs it. Until then it is a role
  described in ARCHITECTURE.md.
- Errors a UI shows are product text from `anya-i18n`; libraries below the app
  return codes and typed errors with English `Display` text for logs.

## Commits

A commit subject is `<scope>: <what changed>`, the scope a crate or area, and
says what the user now sees in plain behavior: `anya: reconnect continuous
sessions when hotwords change, count audio in usage stats`. Give the issue or
reporter when there is one. The body says how it was verified and which test
fails without the change.

The record of what was run is in the commit body, where a later review can
read it, not in a chat or only in a PR. A large commit with no body cannot be
reviewed afterwards.

One commit has one goal. Two fixes together are two commits, each with its own
record, so a revert takes back one and keeps the other, and a review can tell
which test guards which change. A fix found on the way goes in a commit of its
own.

A checkout and its working tree belong to the session that made them. In a
shared tree, read `git status` and `git diff --stat` for the files before
committing, name what else comes with them, and never stage another session's
work. 190033b committed `crates/anya/src/main.rs` while it was still untracked,
which took the whole 1355-line file and 26 lines of another session's
CHANGELOG entries, and it does not build without `indicator.rs`, `plugins.rs`
and `timing.rs`, which were not in it.
