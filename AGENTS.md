# AGENTS.md

Anya is a local speech-to-text service for Linux and macOS. A small core captures audio and runs recognition; engines, triggers and sinks plug in around it, and other programs subscribe to the same frames the built-ins read.

Read ARCHITECTURE.md before adding a crate, a dependency between crates, or a type that crosses a seam. Read the record in docs/decisions/ before changing what it decided.

## Commands

```bash
cargo xtask ci       # the gate CI runs: fmt, clippy -D warnings, tests (with the layer rules), cargo-deny
cargo xtask check    # layer rules only
cargo test --workspace
```

## Rules the build enforces

`xtask/src/arch.rs` and the workspace lints fail the build on:

- a dependency against the layer order, or a crate with no layer;
- anything beyond `serde` in `anya-protocol`, or beyond `serde`/`thiserror`/`tokio` in `anya-core`;
- platform bindings or `allow(unsafe_code)` outside adapters and the app;
- an engine id named outside that engine's crate and the app;
- Windows crates;
- `unwrap`, `todo!`, `unimplemented!`, `dbg!`, printing from library code, undocumented public items.

Fix the code. A rule changes only together with ARCHITECTURE.md and a new decision record.

## Rules review enforces

- **Wire formats live in `anya-protocol` only.** While `PROTOCOL_VERSION` stays, a change only adds; removing or renaming bumps it. Each frame's JSON is pinned in `crates/protocol/tests/`: update the pin in the same change and note it in CHANGELOG.md.
- **Privacy** (docs/decisions/0005): audio goes only to the engine the user chose, and a failing engine is an error the user sees. Dictation frames reach an outside subscriber only with the user's grant for that app. Transcripts and secrets are logged at debug level at most.
- **Hotkey callbacks return at once.** Stopping capture, waiting for the final result and pasting run on a task. Final text is dispatched once per session.
- **Every `unsafe` block** carries a `// SAFETY:` comment saying why its invariant holds.
- **A seam gets a trait when a second adapter needs it.** Until then it is a role described in ARCHITECTURE.md.
- **Tests cover what a consumer sees**: frames, ordering, errors, state transitions. Wiring and a dependency's own behaviour stay untested.
- **User-facing text is product text**: errors shown in a UI and the docs read as finished.

## Licence

Anya has no licence yet (docs/decisions/0006). Code is written for Anya; code from another project comes in only when its authors agreed or its licence leaves the choice open. Comments and docs describe Anya alone.

## Brand

The name is about reading minds. Brand assets are drawn for Anya; the Spy×Family character's likeness, colours and catchphrases stay out of the repo.

## Docs and commits

README.md (中文) and README.en.md change together. User-visible changes go in CHANGELOG.md. Commit subjects are `<scope>: <what changed>`, the scope a crate or area (`protocol`, `core`, `xtask`, `docs`).
