# 0005: Privacy defaults

Accepted, 2026-10-30.

## Context

Anya hears the microphone and, with continuous transcription on, whatever the computer plays. Once other programs subscribe and plugins run engines, a careless default leaks what the user said to a program or vendor they never chose.

## Decision

- Audio goes only to the engine the user chose. An engine that fails is an error the user sees; Anya never retries the audio with another engine.
- A subscriber receives the sources it is granted. `continuous` is the service other programs subscribe to; `dictation` is the user typing by voice and reaches an outside app only with the user's grant for that app.
- Switching continuous transcription off releases the device; subscribers stay connected and are told the source is idle.
- The subscription listens on loopback or a Unix socket only. A control API that can start capture requires a token.
- Transcripts and secrets appear in logs at debug level at most.

## Consequences

- `Source` is on every `session_start` frame, so the server can filter by grant without knowing triggers.
- A recording indicator follows dictation only; continuous transcription is shown by its switch.
