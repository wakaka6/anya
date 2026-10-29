# 0007: Downstream apps are users

Accepted, 2026-10-30.

## Context

Anya's community has two sides. Plugins extend Anya from inside. Apps sit downstream: separate programs that subscribe to what Anya hears and build meeting notes, captions, translators, agents on it. An app breaks silently when a frame changes shape, when an ordering it relied on was never promised, or when it cannot be tested without a microphone and an API key.

A probe of the first protocol types found the promise already broken: "consumers ignore what they do not know" was written down, yet the Rust types refused a new frame type, source or end reason outright.

## Decision

- **The contract is written, versioned and tested.** docs/protocol.md is the subscriber's guide. `anya_protocol::contract::check` holds its ordering promises as code. The fixtures in `crates/protocol/fixtures/` are exactly what Anya sends; a test round-trips each line and runs the checker on it.
- **Additive means additive for every reader.** Every enum on the wire has an `Unknown` variant, so a reader built on one release reads the next. `future.jsonl` is the test.
- **Results are built for downstream use.** `text` is cumulative, so the latest result is a session's whole state and a late subscriber catches up from one frame. A definite utterance never changes, so an app handles each sentence once.
- **Reading needs no setup.** Connecting is enough to receive continuous transcription, with any WebSocket client or `anya subscribe`. No SDK is required to start.
- **Apps are tested against Anya, not just fixtures.** When `anya-server` lands, its tests run the checker on what it sends.

Planned with the server and the binary:

- `anya replay <fixture>`: serves a fixture over the real transport, so an app is built without a microphone or an engine account.
- Discovery: Anya writes where it listens, its version and protocol to a file in the user's runtime directory, so an app finds the instance instead of guessing a port.
- Client SDKs, `@anyahq/client` for JS first: thin, tested against the same fixtures.
- Additive fields apps will want: wall-clock time on `session_start`, utterance timings, the session's language.

## Open

How an app identifies itself and gets the user's grant for dictation (a prompt on first request, a list in the settings, or both) is for the owner to decide before `anya-server` is built.

## Consequences

- A frame change touches four places in one commit: the types, the pins, the fixtures and docs/protocol.md, plus the CHANGELOG.
- Core's session controller has to keep promises 5 and 9; its tests use the checker.
