# Changelog

User-visible changes, newest first.

## Unreleased

- Starting Anya while its tray app is already running opens the running app's settings instead of starting a second copy that answered every hotkey twice.
- The recording time in the statistics counts each dictation's audio; it stayed at zero.
- The overlay preview also works from the settings page in a browser (`--web`).

- Changing the hotwords (in the settings, in the config file or in the `hotwords_file`) now reconnects a continuous session that stays open, once, so it takes them; Anya checks every 2 seconds. Before, only the next session saw the change.
- The average and latest speed, and the minutes in the usage totals, count the audio of each dictation; they stayed at zero.

- On macOS the overlay's text (the notch and the capsule) is drawn with font smoothing, as solid as the system's own text; it used to look thin and faint.

- New settings window and tray panel in the style of macOS System Settings: a sidebar with eight pages (Activity, Dictation, Continuous, Engine, Plugins, Overlay, Apps, General), grouped sections, a plugin market on its own page, changes saved the moment they are made, a live input-level meter, an overlay preview that shows the real overlay for a moment, and a tray panel that shows the last sentence, a failure with retry, or what is missing before Anya can start.

- New app icon: Anya Forger hearing a thought, in a light variant (sky-blue, a thought bubble) and a dark one (night purple, glow and star eyes). The settings page, the tray panel and, on Linux, the window and tray icons follow the system appearance and switch when it changes; the macOS menu bar keeps its monochrome icon.

- Anya runs end to end on macOS and Linux (Wayland, X11): the `anya` binary wires global hotkeys, microphone and system-sound capture, five engines (Doubao, DashScope, OpenAI-compatible, AuralWise, MiMo), paste and clipboard output, the recording overlay (the notch on macOS), the tray app with its settings window, the terminal UI, `--web`, `--doctor`, `--install`, `anya subscribe` and `anya replay`.
- Every user-facing string comes from one catalog in English and Simplified Chinese; the interface follows the system language or `[ui] language`.
- The config file is `~/.config/anya/config.toml`, saved atomically and readable by its owner only; an edit made outside Anya is picked up without a restart.
- A setting with a value an engine cannot use is reported as invalid, not as missing.
- The environment check (`--doctor`, and at every start) names what is missing and how to fix it; a failed required check stops startup.

- Anya is an ASR gateway: a paired app (a phone, a browser page, another computer) sends its own microphone audio and gets results from the engine the user chose, without its own vendor account. LAN sharing is off by default; turned on, Anya also listens over TLS on the local network, and every connection there needs a token from pairing (a six-digit code or QR code in the settings). Each paired app has scopes (`dictate`, `subscribe_continuous`, `subscribe_dictation`) the user can change or revoke.
- Protocol: `session_start` carries `streaming`, whether partial results arrive while audio is still being sent. Clients send `pair`, `dictation_start`, `dictation_stop`, `dictation_cancel` and binary PCM audio; Anya answers with `paired`, `dictation_accepted` and `refused`. Pinned in `crates/protocol/tests/client.rs`; `fixtures/gateway.jsonl` shows a remote dictation client's stream.

- Provider plugins use the shared Engine and Manifest contracts. The anyahq-plugins registry publishes reviewed, versioned packages; Anya refreshes its index without installing plugins automatically.
- Engine plugins run in one Bun process that restarts by itself if it dies; a plugin that fails ends its session with an error and Anya never switches engines. Installing checks the package against the index's sha512 integrity, records it disabled, and enabling is a separate step. Bun is taken from PATH or, at the first install, downloaded at a pinned version and checked against its SHA-256. docs/plugins.md is the plugin author's guide, with the `@anyahq/plugin` SDK, the index format and the anyahq-plugins release workflow.

- Continuous transcription now defaults to Anya's built-in system-sound capture; dictation still defaults to the microphone. The audio layer also exposes loopback devices without requiring a virtual audio driver.

- Protocol 0: subscriber frames `hello`, `source`, `session_start`, `result` and `session_end`, and the engine `Manifest`. Version 0 may still change without notice.
- docs/protocol.md is the guide for apps that subscribe: the frames, ten ordering promises (a definite utterance never changes, a late subscriber catches up from the latest result…), how to read results for captions, notes and final text, and the compatibility rule. `crates/protocol/fixtures/` holds streams to test against, including `future.jsonl` with what a newer Anya may add.
- The Rust protocol types read frame types, sources, states and end reasons a newer Anya adds as `Unknown` instead of failing on the whole frame.
- A `level` frame carries each audio chunk's loudness for level meters, and `session_end` with reason `error` carries a `detail` to show the user.
