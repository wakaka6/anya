# AGENTS.md

Anya is a local speech-to-text service for Linux and macOS. A small core captures audio and runs recognition; engines, triggers and sinks plug in around it, and other programs subscribe to the same frames the built-ins read.

Read ARCHITECTURE.md before adding a crate, a dependency between crates, or a type that crosses a seam. Read docs/protocol.md before touching anything a subscriber receives. Read the record in docs/decisions/ before changing what it decided.

Read LESSONS.md before changing code: what recent merged work got wrong and the rule that would have caught it. Write and review changes against docs/code-standards.md.

## Commands

```bash
cargo xtask ci       # the gate CI runs: fmt, clippy -D warnings, tests (with the layer rules), cargo-deny, bun test in plugins/
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

- **Downstream apps are users** (docs/decisions/0007). Wire formats live in `anya-protocol` only. While `PROTOCOL_VERSION` stays, a change only adds, and every wire enum keeps its `Unknown` variant; removing or renaming bumps the version. A frame change updates the types, the pins in `crates/protocol/tests/`, the fixtures, docs/protocol.md and CHANGELOG.md in one commit. Anything that emits frames is tested with `anya_protocol::contract::check`.
- **Privacy** (docs/decisions/0005): audio goes only to the engine the user chose, and a failing engine is an error the user sees. Dictation frames reach an outside subscriber only with the user's grant for that app. Transcripts and secrets are logged at debug level at most.
- **Hotkey callbacks return at once.** Stopping capture, waiting for the final result and pasting run on a task. Final text is dispatched once per session.
- **Every `unsafe` block** carries a `// SAFETY:` comment saying why its invariant holds.
- **A seam gets a trait when a second adapter needs it.** Until then it is a role described in ARCHITECTURE.md.
- **Tests cover what a consumer sees**: frames, ordering, errors, state transitions. Wiring and a dependency's own behaviour stay untested.
- **Every string a user sees comes from `anya-i18n`** (docs/decisions/0008), in every locale it ships. Libraries below the app return codes and typed errors with English Display text for logs; frames carry codes, manifests carry `Text`. Add a key to the area file you own, in every locale, in the same change.
- **User-facing text is product text**: errors shown in a UI and the docs read as finished.

## Licence

Anya is MIT licensed (docs/decisions/0006). Code from another project comes in only when its licence is compatible with MIT or its authors agreed. Comments and docs describe Anya alone.

## Docs and commits

README.md (中文) and README.en.md change together. User-visible changes go in CHANGELOG.md.

Every commit follows [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/), with no exceptions:

- The subject is `<type>(<scope>): <what changed>`, imperative and lower case, no trailing period, at most 72 characters.
- The type is one of `feat`, `fix`, `docs`, `build`, `chore`, `refactor`, `test`, `perf` or `ci`.
- The scope is a crate or area (`protocol`, `core`, `gui`, `xtask`, `docs`); several are joined with a comma (`config,runtime`), and a change across the whole repo has none.
- A breaking change, such as a `PROTOCOL_VERSION` bump, adds `!` after the scope and a `BREAKING CHANGE:` footer.
- The body, after a blank line, says why.
