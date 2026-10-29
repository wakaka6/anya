# 0004: Community plugins run out of process, in Bun

Accepted, 2026-10-30.

## Context

The community should add engines and apps without a pull request and a release. magpie's provider plugins show a working model: npm packages run by one Bun host process that magpie downloads, talking JSON lines over stdin/stdout, with load timeouts so one stuck plugin does not hold the rest.

Embedded Lua was the alternative. It has no WebSocket or TLS of its own, so Anya would write half of every vendor's client, and its ecosystem is small.

## Decision

- Built-in pieces are Rust crates in the binary, switched on or off by the user.
- Community plugins are npm packages run by one Bun host. Anya downloads Bun when the first plugin is installed; without plugins nothing is downloaded.
- An engine plugin sends a `Manifest` and `Recognition`s; the plugin host wraps it as one more `Engine`.
- Results per session are a few dozen frames, so one stdio hop per frame is cheap enough; there is no second, in-process JS engine.

## Consequences

- A plugin runs with the user's rights and an engine plugin hears the microphone. The UI says which plugin receives audio, and the community repository reviews plugins.
- A plugin that fails mid-session ends the session with an error; Anya does not fall back to another engine (0005).
