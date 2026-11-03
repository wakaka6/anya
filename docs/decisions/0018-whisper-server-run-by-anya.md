# 0018: Anya can run whisper-server and download its models

Accepted, 2026-11-03.

## Context

The built-in `whisper-cpp` engine recognizes on this computer through a `whisper-server` the user starts and keeps running, with a model file the user downloads by hand. That is two steps outside Anya before the first dictation, and a server holding its model in memory while nobody dictates. The engine crate speaks whisper-server's HTTP API; downloading files and owning a process are different jobs, and putting them in the engine would make one crate answer for recognition, the network, the disk and a child process.

## Decision

- `anya-whisper`, a service-layer crate, owns the models and the server. It names no engine: the app hands it the whisper.cpp engine, and it wraps it (`Local::wrap`) so that the engine crate keeps speaking HTTP to an address and nothing else.
- The engine's settings say who runs the server: `mode = "external"` (the default; the user runs it at `url`) or `"managed"` (Anya runs it). Managed settings are `model` (a catalog id or the path of a ggml file), `program` (empty finds `whisper-server` on `PATH` and in Homebrew's prefixes), `idle_timeout_minutes` (empty is 10, `0` keeps it running) and `download_source`. They live in `[recognition.engines.whisper-cpp]` with the engine's other settings. Download progress, the port and the process are runtime state and are never written to the config.
- A managed session takes a lease on the server, starting it if it is not running or runs another model, and opens the engine on its address. Audio queues meanwhile, so recording does not wait for the model to load. The server is started with arguments, never through a shell, listens on a free port of `127.0.0.1` only, and stops after its idle time, when Anya quits, when the user stops it, or before its model is deleted. A server that does not start ends the session with an engine error that quotes its last line (0005); Anya does not switch engines.
- The catalog is whisper.cpp's multilingual models (`base`, `small`, `large-v3-turbo-q5_0`, `large-v3-turbo`, `large-v3`), each pinned in the binary to the SHA-1 whisper.cpp publishes for it. English-only models are left out. The suggested model is `large-v3-turbo-q5_0`, or `small` on a computer with less than 8 GB of memory; the suggestion is a label, not a benchmark.
- A model downloads only when the user asks, from Hugging Face or a mirror the user names. The request is a GET with a `User-Agent` of `anya` and nothing else. It goes to `<file>.part` in `<data dir>/models`, resumes from its length, and becomes `<file>` only after its SHA-1 matches the pinned one; a file that does not match is deleted. A file under its own name is therefore whole and the pinned model, whatever the source, so a mirror needs no trust.
- Anya does not install `whisper-server`. The settings say where it was found, or show the Homebrew command to install it.

## Consequences

- The engine crate is unchanged and testable against a fake server; the download, the hash check and the process are tested in `anya-whisper`, the process with the test binary standing in for whisper-server.
- SHA-1 is what whisper.cpp publishes. It is enough to tell a broken or substituted download from the pinned file, which is all the check is for here; a stronger hash would be one Anya computed itself, with nothing upstream to compare it with.
- A new model in the catalog is a change to Anya, with its size and hash, and ships with a release.
- The first managed session after a stop loads the model: a few seconds for the suggested model on Apple silicon, during which the user is already speaking.
