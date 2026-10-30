# Anya

[中文](README.md) · [English](README.en.md)

Hears what you say, and hands it to any program that wants it.

Anya is a local speech-to-text service for Linux and macOS. A small core only captures audio and runs recognition; dictation, continuous transcription, pasting, the overlay and speech engines all plug in around it, and other programs subscribe to the same recognition frames.

**Status:** just started, not usable yet. The architecture and the rules are in [ARCHITECTURE.md](ARCHITECTURE.md) and [docs/decisions](docs/decisions/).

**Building on Anya** (meeting notes, live captions, translation, an agent that listens…): the subscriber protocol, its ordering promises and streams to test against are in [docs/protocol.md](docs/protocol.md).

## LAN sharing and pairing

By default only this computer can connect to Anya (loopback). To let a phone, a browser or another computer send its own microphone for recognition, turn on "LAN sharing" under "Apps" in the settings (`[subscription] lan = true`, port `lan_port`, 7591 by default):

- On the network the connection is TLS (`wss://`) with a certificate Anya makes and keeps; an app trusts it by the certificate fingerprint it saw while pairing.
- "Add app" shows a six-digit pairing code and a QR code (the address, the code and the certificate fingerprint). The app scans it or takes the code and trades it for a token; a code works once and for two minutes.
- Each app is authorised on its own: dictation, receiving continuous transcription, seeing others' dictation. Change its scopes or remove it at any time; removing it closes its connections at once.
- An app's dictation result goes only to that app, never onto this computer. With a streaming engine the app gets partial results too, otherwise only the final one.

The protocol is in [docs/protocol.md](docs/protocol.md).

## Plugins

Speech engines can be plugins, all run by one Bun process (Bun 1.2 or newer; if none is found, the first plugin install downloads a pinned release). Plugins come from the anya-plugins registry and every download is checked against its integrity hash.

```bash
anya plugin search [query]       # browse the registry
anya plugin install <id>[@ver]   # install; it starts disabled
anya plugin enable <id>          # loaded when Anya starts
anya plugin disable <id>
anya plugin remove <id>
anya plugin list
anya plugin builtin              # built-in engines and their state
anya plugin builtin off <id>     # turn a built-in engine off (on turns it back on)
```

Install and enable are separate steps, on the settings window's Engine page too. An enabled engine plugin receives the audio of every session opened on its engine (a LAN app's dictation included) and runs with your rights, so enable only plugins you trust. Changes made on the command line apply the next time Anya starts; enabling, disabling and removing in the settings window apply at once. Writing a plugin: [docs/plugins.md](docs/plugins.md).

Built-in engines can be turned off too, except the one you have chosen; the change is written to `[plugins.builtin]` in the config file, and a running Anya reads it and applies it at once.

## Development

Needs Rust 1.95 (picked by `rust-toolchain.toml`) and [cargo-deny](https://github.com/EmbarkStudios/cargo-deny).

```bash
cargo xtask ci       # the full gate CI runs: fmt, clippy, tests (with the layer rules), cargo-deny
cargo xtask check    # layer rules only
```

## Changelog

See [CHANGELOG.md](CHANGELOG.md).
