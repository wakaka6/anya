# Building on Anya: the gateway protocol, version 0

For anyone writing a program that reads what Anya hears or uses Anya as an ASR gateway: meeting notes, live captions, a translator, an agent that listens, voice commands and mobile dictation clients. Version 0 is unreleased and may still change; the [CHANGELOG](../CHANGELOG.md) marks every protocol change.

## Plugin or app

| | Plugin | App (this page) |
|---|---|---|
| Runs | inside Anya's Bun host, installed with `anya plugin add` | as its own program, any language |
| Can | add an engine, a trigger or a sink | read frames; send its own audio for dictation when its token allows it |
| Talks | JSON lines over stdin/stdout (docs/decisions/0004) | WebSocket or `anya subscribe` |

## Connecting

Anya listens on loopback (`ws://127.0.0.1:7590/asr` by default) or a Unix socket. A program there may connect without a token and receives continuous transcription. When the user turns on LAN sharing, Anya also listens on `wss://<address>:7591/asr` for the local network or the user's own VPN; there every connection needs a paired token. `anya subscribe` prints the frames as NDJSON for programs that would rather read a pipe.

Offer the WebSocket subprotocol `anya.v0`. Send the token as `Authorization: Bearer <token>`, or, from a browser page that cannot set headers, as a second subprotocol `anya.token.<token>`; Anya answers with `anya.v0` only. A token Anya does not know is refused at the handshake.

## Pairing and scopes

The user presses **Add app** in Anya's settings and sees a six-digit code and a QR code. The QR code holds JSON: `{"url": "wss://192.168.1.23:7591/asr", "code": "482913", "fingerprint": "3f:9a:…"}`. The certificate is self-signed: accept the one whose SHA-256 (of its DER) is `fingerprint` and no other. Connect without a token and send `{"type": "pair", "code": "482913", "name": "iPhone"}`; the answer is `{"type": "paired", "client": "c-…", "token": "…", "scopes": ["dictate"]}` and the same connection now acts as that client. Keep the token: it is shown once. A code works once, for two minutes; five wrong codes void it.

| Scope | Allows |
|---|---|
| `dictate` | start dictation sessions with your own audio and receive their frames |
| `subscribe_continuous` | receive continuous transcription |
| `subscribe_dictation` | receive every dictation session, the host's own and other clients' |

A new client holds `dictate`; the user grants the rest in the settings, and a change applies to open connections at once. Removing a client closes its connections.

## Remote dictation

Your app records, Anya recognizes with the engine the user chose for remote dictation (`[recognition] remote`) and its credentials; your app never talks to an ASR vendor.

| You send | Fields | Anya replies |
|---|---|---|
| `dictation_start` | `request_id`, optional `language` (BCP-47), optional `hotwords` | `dictation_accepted` with `request_id`, `session`, `streaming` |
| binary messages | signed 16-bit little-endian mono PCM at 16 kHz, any whole number of samples (40 ms = 1280 bytes is a good size) | `level` frames, and `result` frames if `streaming` |
| `dictation_stop` | `session` | the final `result`, then `session_end` |
| `dictation_cancel` | `session` | `session_end` with reason `cancelled` |

`dictation_accepted` arrives before any frame of its session. One recording per connection at a time; a recording ends by itself after ten minutes, as if stopped. Closing the connection cancels your unfinished sessions. Results go to you only (and to clients holding `subscribe_dictation`); nothing is pasted on the computer Anya runs on.

**Streaming and batch engines.** With `streaming: true`, `result` frames with `"final": false` arrive while you speak; show the latest `text`. With `streaming: false` the engine only answers whole recordings: keep uploading while the user speaks, show "recognizing" after `dictation_stop`, and the one result is the final one. Anya never makes up partial results.

A message Anya does not act on is answered with `{"type": "refused", "reason": …}`, carrying the `request_id` or `session` it is about and sometimes an untranslated `detail`:

| `reason` | Means |
|---|---|
| `unauthorized` | no token on this connection; pair first |
| `forbidden` | your scopes do not allow it |
| `busy` | this connection is already recording |
| `not_configured` | the user's engine lacks a setting |
| `unknown_session` | no session of yours has that id |
| `bad_request` | not a message Anya understands, or audio with no recording open |
| `pairing_failed` | wrong, used or expired code |
| `too_many_attempts` | the code is void; ask the user for a new one |

Treat an unknown `reason` as a refusal. Replies go only to the connection that caused them; a reader that only knows frames skips them as unknown types.

## Frames

Every frame is a JSON object with a `type`.

| `type` | When | Fields |
|---|---|---|
| `hello` | first, once | `name` (`"anya"`), `version`, `protocol` |
| `source` | after `hello` once per source you receive, then on every change | `source` (`dictation`, `continuous`), `state` (`listening`, `idle`), `device` when known |
| `session_start` | the engine connected | `session`, `source`, `engine`, `streaming` (partial results arrive before the audio ends) |
| `result` | per recognition | `session`, `text`, `utterances` (`text`, `definite`), `final` |
| `level` | per audio chunk while the session is open, about 25 a second | `session`, `level` (0.0 silence to 1.0 full scale) |
| `session_end` | the session is over | `session`, `reason` (`final`, `error`, `closed`, `cancelled`), `detail` with `error`: what went wrong, in words to show |

Exact JSON for each is pinned in [`crates/protocol/tests/wire.rs`](../crates/protocol/tests/wire.rs); client messages and replies in [`crates/protocol/tests/client.rs`](../crates/protocol/tests/client.rs).


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

Within a protocol version Anya only adds: new frame types, new fields, new values of `source`, `state` and `reason`. The same holds for client messages, replies, scopes and refusal reasons. Your program ignores a frame type it does not know, a field it does not know, and treats an unknown `reason` as the end of the session. The Rust types in `anya-protocol` already do this (`Unknown` variants). Removing or renaming anything bumps `protocol` in `hello`.

## Fixtures

[`crates/protocol/fixtures/`](../crates/protocol/fixtures/) holds streams to test your program against, one frame per line, exactly as Anya sends them:

| File | What it shows |
|---|---|
| `dictation.jsonl` | one dictated sentence, partials to final |
| `continuous.jsonl` | a meeting through a loopback device: two sessions, utterances turning definite, the source switched off |
| `errors.jsonl` | a session that failed (with its `detail` and a `level`), one cancelled, one closed at shutdown |
| `gateway.jsonl` | what a remote dictation client receives: a streaming session with a refused second start, then a batch session |
| `future.jsonl` | what a newer Anya may send within version 0; your program must read it without failing |
