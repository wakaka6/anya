# Changelog

User-visible changes, newest first.

## Unreleased

- Protocol 0: subscriber frames `hello`, `source`, `session_start`, `result` and `session_end`, and the engine `Manifest`. Version 0 may still change without notice.
- docs/protocol.md is the guide for apps that subscribe: the frames, ten ordering promises (a definite utterance never changes, a late subscriber catches up from the latest result…), how to read results for captions, notes and final text, and the compatibility rule. `crates/protocol/fixtures/` holds streams to test against, including `future.jsonl` with what a newer Anya may add.
- The Rust protocol types read frame types, sources, states and end reasons a newer Anya adds as `Unknown` instead of failing on the whole frame.
