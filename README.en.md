# Anya

[中文](README.md) · [English](README.en.md)

Hears what you say, and hands it to any program that wants it.

Anya is a local speech-to-text service for Linux and macOS. A small core only captures audio and runs recognition; dictation, continuous transcription, pasting, the overlay and speech engines all plug in around it, and other programs subscribe to the same recognition frames.

**Status:** just started, not usable yet. The architecture and the rules are in [ARCHITECTURE.md](ARCHITECTURE.md) and [docs/decisions](docs/decisions/).

**Building on Anya** (meeting notes, live captions, translation, an agent that listens…): the subscriber protocol, its ordering promises and streams to test against are in [docs/protocol.md](docs/protocol.md).

## Development

Needs Rust 1.95 (picked by `rust-toolchain.toml`) and [cargo-deny](https://github.com/EmbarkStudios/cargo-deny).

```bash
cargo xtask ci       # the full gate CI runs: fmt, clippy, tests (with the layer rules), cargo-deny
cargo xtask check    # layer rules only
```

## Changelog

See [CHANGELOG.md](CHANGELOG.md).
