# Building on Anya: the subscriber protocol, version 0

For anyone writing a program that reads what Anya hears: meeting notes, live captions, a translator, an agent that listens, voice commands. Version 0 is unreleased and may still change; the [CHANGELOG](../CHANGELOG.md) marks every protocol change.

## Plugin or app

| | Plugin | App (this page) |
|---|---|---|
| Runs | inside Anya's Bun host, installed with `anya plugin add` | as its own program, any language |
| Can | add an engine, a trigger or a sink | read frames; later, ask for sessions through the control API |
| Talks | JSON lines over stdin/stdout (docs/decisions/0004) | WebSocket or `anya subscribe` |

## Connecting

Planned with `anya-server` (docs/decisions/0007): a WebSocket on loopback or a Unix socket, one JSON text message per frame, and `anya subscribe` printing the same frames as NDJSON for programs that would rather read a pipe. Connecting is enough to receive continuous transcription; dictation needs the user's grant for your app.

## Frames

Every frame is a JSON object with a `type`.

| `type` | When | Fields |
|---|---|---|
| `hello` | first, once | `name` (`"anya"`), `version`, `protocol` |
| `source` | after `hello` once per source you receive, then on every change | `source` (`dictation`, `continuous`), `state` (`listening`, `idle`), `device` when known |
| `session_start` | the engine connected | `session`, `source`, `engine` |
| `result` | per recognition | `session`, `text`, `utterances` (`text`, `definite`), `final` |
| `level` | per audio chunk while the session is open, about 25 a second | `session`, `level` (0.0 silence to 1.0 full scale) |
| `session_end` | the session is over | `session`, `reason` (`final`, `error`, `closed`, `cancelled`), `detail` with `error`: what went wrong, in words to show |

Exact JSON for each is pinned in [`crates/protocol/tests/wire.rs`](../crates/protocol/tests/wire.rs).

## Promises

Anya keeps these on every connection; [`anya_protocol::contract::check`](../crates/protocol/src/contract.rs) is the same list as code.

1. `hello` is the first frame, and only the first.
2. Each session starts once, and its frames follow its `session_start`.
3. No frame names a session after its `session_end`.
4. The final result (`"final": true`) is the session's last result.
5. A definite utterance keeps its place and its text in every later result of its session.
6. A session ends with reason `final` exactly when its final result was sent.
7. `text` is the session's whole text so far: each `result` replaces the one before.
8. Sessions of different sources may interleave; frames of one session arrive in order.
9. A session already running when you connect is sent as its `session_start` and its latest `result`, right after the `source` frames.
10. Anya never waits for you. A subscriber that falls more than 256 frames behind is disconnected; reconnect and you continue from promise 9.

## Reading results

- **Live captions**: show the latest `text` of each session; clear it at `session_end`.
- **Notes, translation, summaries**: handle each utterance once, when it turns definite. Remember how many definite utterances a session has had; promise 5 makes everything before that count settled.
- **The finished text**: the result with `"final": true`. A session can end without one (`error`, `closed`); keep what was definite.
- **`cancelled`**: the user threw the session away. Drop its text.
- **A quiet stream**: the latest `source` frame says whether the source is `listening` (nobody is talking) or `idle` (switched off). A refused connection means Anya is not running.

## Compatibility

Within a protocol version Anya only adds: new frame types, new fields, new values of `source`, `state` and `reason`. Your program ignores a frame type it does not know, a field it does not know, and treats an unknown `reason` as the end of the session. The Rust types in `anya-protocol` already do this (`Unknown` variants). Removing or renaming anything bumps `protocol` in `hello`.

## Fixtures

[`crates/protocol/fixtures/`](../crates/protocol/fixtures/) holds streams to test your program against, one frame per line, exactly as Anya sends them:

| File | What it shows |
|---|---|
| `dictation.jsonl` | one dictated sentence, partials to final |
| `continuous.jsonl` | a meeting through a loopback device: two sessions, utterances turning definite, the source switched off |
| `errors.jsonl` | a session that failed (with its `detail` and a `level`), one cancelled, one closed at shutdown |
| `future.jsonl` | what a newer Anya may send within version 0; your program must read it without failing |
