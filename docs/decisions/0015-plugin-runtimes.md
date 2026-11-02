# 0015: Plugins run inline in QuickJS or in a Bun process of their own

Accepted, 2026-10-30.

## Context

0004 runs every community plugin in one Bun host. A plugin that loops without awaiting stalls the host's event loop, so one engine plugin doing heavy work freezes another's streaming session; a timeout in JS cannot stop a loop that never yields. The hooks of 0016 make this worse: they run on every dictation, some between the final result and the paste.

Those hooks are of two sorts. Most text work (a replacement list, spoken punctuation, number formats) is a pure function that must take microseconds, and a stdio round trip to Bun costs more than the work. The rest (polishing with an LLM, writing a note) needs the network or the disk, and needs Bun's ecosystem as engines do. magpie runs its per-event gateway hooks in a JS interpreter inside its own process and its I/O hooks in Bun for the same reason.

## Decision

A plugin package has up to two entries, and each runs in its own place:

- **`anya.inline`** runs in QuickJS (`rquickjs`, MIT) inside Anya, in the service crate `anya-script`. Each plugin gets a QuickJS runtime of its own on a thread of its own. The runtime is given nothing of the outside world: no `fetch`, file system, timers, `process` or `import`; the entry is one bundled file. Every call has a time budget, enforced by QuickJS's interrupt handler, and the runtime has a memory and stack limit. A call that is stopped, throws or returns the wrong shape has failed, and the hook's failure rule (0016) applies.
- **`anya.host`** runs in Bun, one process per plugin. Each process loads one package; the host protocol and its JSON lines stay as they are. Anya pings each process every second; a process that has not answered for 3 seconds is killed, restarted with backoff, and the user is told which plugin stopped answering. Sessions open on a killed process end with an error (0005). Processes start with Anya, so the first dictation after start waits for no Bun.
- `anya.host` replaces `anya.engine`. A package with only an inline entry starts no Bun, and needs none downloaded.
- **Budgets are configurable within fixed caps.** 0016 gives each hook a default and a range. A plugin's manifest may state the budget it needs; `[plugins.limits]` in `config.toml` sets every plugin's, `[plugins.limits.<id>]` one plugin's; the user's value wins over the manifest's, and every value is clamped to the hook's range. The caps are not configurable: they keep the paste from waiting without bound. The watchdog's 1 and 3 seconds are not a budget and stay fixed; a longer one would only keep a stuck plugin alive.
- **The index says what a release provides**: `provides` (`engine`, `context`, `transform`, `action`) replaces `kind`. The package does not declare it: the registry's CI loads both entries and writes what they export, so the two cannot disagree.

`PLUGIN_PROTOCOL` and `INDEX_VERSION` stay 1: nothing has been released, so the entry and index fields change in place.

## Consequences

- An inline plugin cannot reach the network or the disk, and Anya can say so to the user before it is enabled. A host plugin still runs with the user's rights; its permissions remain declarations.
- One process per plugin is about 22 MB of resident memory per enabled host plugin (Bun 1.3.14, idle), and is what later lets a sandbox (`sandbox-exec`, Landlock) confine each plugin to the permissions it declares.
- `anya-plugin-host` keeps one child, backoff and watchdog per plugin; `host/host.js` loads one package; `scripts/validate-plugin.mjs` checks `anya.inline` (one file, no `import`) and `anya.host`, and derives `provides` from their exports.
- `anya-script` joins the service layer in `xtask/src/arch.rs`; ARCHITECTURE.md and docs/plugins.md describe both runtimes.
- Supersedes 0004's single Bun host and its "no second, in-process JS engine".
