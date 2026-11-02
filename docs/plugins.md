# Writing plugins

A plugin adds to Anya without a pull request or a release. It is an npm-style package that provides any of four things (docs/decisions/0016):

| Provides | What it does | What it receives |
| --- | --- | --- |
| `engine` | a speech recognizer, listed beside the built-in engines | the audio of every session opened on it, gateway clients' too (docs/decisions/0009) |
| `context` | hotwords and a language for a dictation, before it opens | the focused app's id, the session's hotwords and language |
| `transform` | text in, text out, on a dictation's final text, before it is pasted | everything dictated |
| `action` | something done with a dictation's final text, beside the paste | everything dictated |

Hooks run for the user's own dictation only, not for continuous transcription or gateway clients. Transforms run as one chain in the order the user sets; the text pasted is the chain's. Actions all get the chain's text and run side by side, never holding up the paste.

A package has up to two entries, and each runs in its own place (docs/decisions/0015):

- **`anya.inline`** runs in QuickJS inside Anya. It is given nothing of the outside world: no network, no disk, no timers, no `import`. Anya tells the user so, and an inline-only plugin needs no Bun. It may provide `context` and `transform`. Use it for text work that must be instant: a replacement list, spoken punctuation, number formats.
- **`anya.host`** runs in a Bun process of its own, with the user's rights. It may provide any of the four. Use it for anything that needs the network, the disk or an npm package: an engine, polishing with an LLM, writing a note.

Before the user installs or enables a plugin, Anya shows its name, what it provides and what each of those receives, whether it runs inline only, and the permissions it declares. Keep the permission list honest; the registry reviews it.

## Package layout

```
my-plugin/
  package.json
  host.js          # anya.host: runs in Bun
  inline.js        # anya.inline: runs in QuickJS, one file
```

```json
{
  "name": "my-plugin",
  "version": "1.0.0",
  "description": "One line for the marketplace.",
  "type": "module",
  "anya": {
    "id": "my-plugin",
    "host": "./host.js",
    "inline": "./inline.js",
    "compat": ">=0.1.0",
    "protocol": 1,
    "permissions": ["network:api.example.com"],
    "fields": [{ "key": "api_key", "label": "API key", "kind": "secret", "required": true }],
    "limits": { "transform_ms": 3000 }
  },
  "devDependencies": { "@wakaka6/anya-plugin": "^1.0.0" }
}
```

- `anya.host` and `anya.inline` name the entries; a package has at least one. What a package provides is what its entries export; the registry's CI reads it from them, so there is nothing to declare.
- `anya.id` is the plugin id (default: `name`): lowercase letters, digits and `-`. An engine's manifest `id` must equal it.
- `anya.name` and `anya.description` (default: the package's `name` and `description`) are shown in the plugin list; they may be `Text`, a string or a `{ "en": …, "zh-CN": … }` map.
- `anya.compat` is a semver range of Anya versions. `anya.protocol` is the plugin protocol, currently `1`. Anya installs only releases of its own protocol.
- `anya.permissions` are strings the user sees: `network:<host>`, `env:<NAME>`, `fs:read:<path>`. They are declarations for review and display; Anya does not sandbox a host entry.
- `anya.fields` are the settings of the plugin's hooks, drawn as a form in the settings window and stored under `[plugins.settings.<id>]`. An engine keeps its own in its manifest. A field with `"required": true` must be set before the plugin's hooks run: until then Anya skips them, and the Plugins page and `anya plugin list` say which settings are missing. Mark only what the hooks cannot work without.
- `anya.limits` asks for budgets other than the defaults (below). The user's settings win.

Dependencies a host entry needs at run time must be bundled into the package (`bun build --target=bun`, or `bundledDependencies`): Anya unpacks the tarball and runs it, it does not run `npm install`. An inline entry must be one bundled file.

## Budgets

Every hook call has a time budget. Inline calls are stopped when it is spent, even in a loop that never yields; a host call is cancelled (its `signal` aborts) and its late answer ignored.

| Budget | `limits` key | Default | Range |
| --- | --- | --- | --- |
| an inline call | `inline_ms` | 20 ms | 1–200 ms |
| an inline plugin's memory | `inline_memory_mib` | 16 MiB | 4–256 MiB |
| a host `context` | `context_ms` | 300 ms | 50–2000 ms |
| a host `transform` | `transform_ms` | 2000 ms | 100–10000 ms |
| the whole transform chain | — | 4000 ms | 500–15000 ms |
| an `action` | `action_ms` | 10000 ms | 1000–60000 ms |

The user sets them in `config.toml`: `[plugins.limits]` for every plugin (and `chain_ms` for the chain), `[plugins.limits.<id>]` for one. Every value is clamped to its range; the ranges are not configurable.

A host process must keep its event loop free. Anya pings it every second; one that has not answered for 3 seconds is killed and restarted, its sessions end with an error, and the user is told which plugin stopped answering. Do heavy work in a `Worker`.

When a hook fails:

| Hook | Failure | What happens |
| --- | --- | --- |
| `context` | error, budget spent, wrong shape | the session opens with what it had |
| `transform` | error, budget spent, not a string, over 64 KiB | the step is skipped and the text before it goes on; the activity list names the plugin |
| `action` | error, budget spent | the activity list names the plugin; the paste and the other actions are untouched |

## Inline entries

An inline entry is an ES module that exports plain functions. It may keep state between calls in module variables.

```js
// inline.js
const rules = [["换行", "\n"], ["句号", "。"]];

export function transform(text, ctx) {
  // ctx: { session, source, app, settings }
  for (const [spoken, written] of rules) text = text.replaceAll(spoken, written);
  return text;
}

export function context(ctx) {
  // ctx: { source, app, settings, language, hotwords }
  if (ctx.app === "com.microsoft.VSCode") return { hotwords: ["rquickjs", "Tokio"] };
}
```

`transform` returns a string. `context` returns nothing, or `{ hotwords?, language? }`: hotwords are added to the session's, and the first context hook in the user's order that names a language sets it. Either may be `async`; its promise must settle within the budget. `console.log` and friends are logged by Anya at debug level.

## Host entries and the SDK

`import { defineEngine, defineContext, defineTransform, defineAction } from "@wakaka6/anya-plugin"` works in every host entry; the host serves its own copy of the SDK, so the package does not ship one. Install `@wakaka6/anya-plugin` as a dev dependency for the types (`sdk/plugin/index.d.ts`).

A host entry exports what it provides, as the default export or named exports, at most one of each kind:

```js
import { defineTransform, defineAction } from "@wakaka6/anya-plugin";

export const transform = defineTransform({
  async transform(text, { settings, signal }) {
    const res = await fetch("https://api.example.com/polish", {
      method: "POST", signal,
      headers: { authorization: `Bearer ${settings.api_key}` },
      body: JSON.stringify({ text }),
    });
    if (!res.ok) throw new Error(`the service answered ${res.status}`);
    return (await res.json()).text;
  },
});

export const action = defineAction({
  async act(text, { session, app, settings, signal }) {
    await appendToNote(settings.vault, text, { signal }); // yours
  },
});
```

`defineContext({ context(ctx) })` gets `{ source, app, settings, language, hotwords, signal }` and returns as an inline `context` does. `defineTransform({ transform(text, ctx) })` and `defineAction({ act(text, ctx) })` get `{ session, source, app, settings, signal }`. `signal` aborts when the budget is spent. A throw or a rejection fails the call; Anya logs your message and shows the user the plugin's name, so make it useful and free of secrets.

A transform computes: calling a service to compute (an LLM) is fine, changing the world (writing a note) is an action's job. Anya cannot stop a transform from doing more, but a transform runs before later ones change the text, on the paste's path, and its failures are not reported as the side effect's.

### Engines

```js
import { defineEngine } from "@wakaka6/anya-plugin";

export default defineEngine({
  manifest: {
    id: "my-plugin",
    title: { en: "My engine", "zh-CN": "我的引擎" },
    blurb: "One line under the title.",
    streaming: true,
    fields: [
      { key: "api_key", label: "API key", kind: "secret", required: true },
      { key: "model", label: "Model", kind: "choice", options: ["small", "large"], advanced: true },
    ],
  },

  async open({ language, hotwords, settings }, session) {
    const socket = await connect(settings.api_key, language, hotwords); // yours
    session.on("audio", (pcm) => socket.send(pcm));
    session.on("finish", () => socket.end());
    session.on("close", () => socket.destroy());
    socket.onText((text, done) => session.result({ text, final: done }));
    socket.onError((e) => session.error("upstream", e.message));
  },
});
```

**Manifest.** `streaming: true` means results arrive while the user speaks. With `streaming: false`, send one result with `final: true` after `finish`; Anya never shows partial text of a batch engine. Settings UIs are drawn from `fields`, so a plugin ships no UI.

**`open(params, session)`** connects and registers handlers. When it returns (or resolves) the session starts; audio that arrived earlier waits. It has 15 seconds. `params.settings` holds the values of your fields by key (strings), read fresh at every session.

**Session events.** `on("audio", pcm)` gets a `Uint8Array` of 40 ms, signed 16-bit little-endian mono PCM at 16 kHz (1280 bytes). `on("finish")` means no more audio: send the final result. `on("close")` means the session is over, also when the user cancelled; release what you hold. Handlers run in order, one at a time.

**Results.** `session.result({ text, utterances?, final? })`: `text` is the session's whole text so far, not a delta; `utterances` are `{ text, definite }` sentences; `final: true` ends the session and must be the last result. After it, further calls are ignored.

**Errors.** `session.error(kind, detail)` ends the session. `kind` is `not_configured` (`detail`: the field key), `connect` or `upstream`. A thrown exception in a handler ends the session with `upstream`; an exception thrown from `open` ends it with `connect`. Throw an error with a `kind` property to choose another kind. Anya shows the user the translated kind with your `detail` beside it, and does not retry on another engine (docs/decisions/0005), so make `detail` useful and free of secrets.

**Console.** `console.log` and friends are captured and logged by Anya at debug level; nothing a plugin writes to stdout can break the host protocol. Do not log transcripts or credentials anyway.

**Isolation.** Each host entry has its own process: an exception, a crash or a stuck event loop affects that plugin alone. A module that does not load within 15 seconds is marked failed.

## Testing locally

Unit-test an engine without Anya by driving a session yourself:

```js
import { createSession, start, deliver } from "@wakaka6/anya-plugin";
import engine from "./host.js";

const results = [];
const session = createSession({ result: (r) => results.push(r), error: (k, d) => results.push({ k, d }) });
await engine.open({ hotwords: [], settings: {} }, session);
start(session);
await deliver(session, "audio", new Uint8Array(1280));
await deliver(session, "finish");
```

Hooks are plain functions: call `transform.transform("text", { settings: {} })` or an inline module's `transform("text", {})` directly. The SDK's `callContext`, `callTransform` and `callAction` run a defined hook as Anya does, with its answer checked.

`sdk/plugin/examples/` has complete packages to copy from: `echo-engine` (a streaming engine), `hooks` (a host transform and action) and `inline` (an inline transform and context).

`plugins/` holds the plugins this repository releases, each with its tests: `spoken-punctuation` (an inline context and transform in one package), `app-hotwords` (an inline context with a setting) and `daily-notes` (a host action). `bun test` in `plugins/` runs them, with `@wakaka6/anya-plugin` served from this repository's SDK as the host serves it (`plugins/bunfig.toml`); `cargo xtask ci` runs it too.

To try the real thing, serve a local registry index. Pack the plugin and let the registry's own script describe it:

```bash
npm pack                                   # my-plugin-1.0.0.tgz
mkdir unpacked && tar xzf my-plugin-1.0.0.tgz -C unpacked --strip-components=1
bun scripts/validate-plugin.mjs unpacked my-plugin-1.0.0.tgz file:///abs/path/my-plugin-1.0.0.tgz index.json <path-to>/sdk/plugin/index.js
```

The script runs the registry's checks and writes the release into `index.json`. Point Anya's plugin registry URL at `file:///abs/path/index.json` (`[plugins] index`; `Registry::fetch` reads `file://` and `https://`), install, then enable the plugin.

## Registry index format (version 1)

`https://raw.githubusercontent.com/wakaka6/anya/registry/index.json`, the `registry` branch of this repository, written by the plugin-release workflow and read by Anya when the user opens the marketplace or checks for updates. Tarballs sit beside it in `registry/tgz/`. Types: `anya_protocol::Index`.

| Field | Meaning |
| --- | --- |
| `version` | Index format, `1`. Anya refuses another value. |
| `plugins[].id` | Plugin id; also the install directory. |
| `plugins[].name`, `description` | `Text`: a string or a locale map. |
| `versions[].version` | Semver. Immutable once published. |
| `versions[].artifact` | URL of the npm tarball (`.tgz`). |
| `versions[].integrity` | `sha512-<base64>` of the tarball (SRI). Anya verifies it before unpacking. |
| `versions[].anya` | Semver range of Anya versions. |
| `versions[].protocol` | Plugin protocol, `1`. |
| `versions[].provides` | What the release provides: `engine`, `context`, `transform`, `action`, as the CI found in its entries. |
| `versions[].host` | Whether it has a host entry, and so runs in Bun. |
| `versions[].streaming` | An engine's `manifest.streaming`, for listing before install. |
| `versions[].permissions` | Strings shown before enabling. |

Unknown fields are ignored. Anya installs a release only when it provides something and nothing this Anya does not know, `protocol` equals its own and `anya` matches its version.

## What Anya does with a release

1. **Install** (a user action): downloads the artifact, checks `integrity`, refuses archives with links or paths that leave the package, unpacks into `plugins/<id>/<version>/` and records `{id, version, integrity, enabled: false}` in `plugins/plugins.json`.
2. **Enable** (a separate user action): the plugin is loaded the next time Anya starts or reloads its plugins: its inline entry into QuickJS, its host entry into a Bun process of its own.
3. **Update**: a newer version is installed explicitly, replaces the pinned version and starts disabled. Nothing installs or updates by itself, and Anya runs no Git tag listener.

Bun comes from `PATH` if it is version 1.2.0 or newer; otherwise Anya downloads its pinned Bun release (SHA-256 checked) when the first plugin with a host entry is installed. Without such plugins nothing is downloaded.

## The plugin release flow

The Anya repository holds one directory per community plugin under `plugins/<id>/` and the workflow `.github/workflows/plugin-release.yml`. Plugin authors open a pull request to add or change a plugin; a maintainer reviews code and the declared permissions. Releasing:

1. A maintainer tags the merged commit `<plugin-id>-v<version>` (for example `echo-v1.2.0`). The tag version must equal `package.json`'s.
2. The workflow packs the plugin (`npm pack`) and runs `scripts/validate-plugin.mjs` against the SDK in the same commit, which checks: id and version format; at least one of `anya.host` and `anya.inline`; `anya.compat` (not unbounded), `anya.protocol`, `anya.permissions`, `anya.fields` and `anya.limits` are well formed; an inline entry is one file without `import` or `require`; each entry loads and exports at least one hook, at most one of each kind; an engine's `manifest.id` equals the plugin id and its `streaming` is a boolean; the version is not already in the index. It writes `provides` and `host` from what the entries export.
3. It computes the tarball's `sha512` integrity, appends the entry to `index.json` and commits the tarball (`tgz/`) and the index to the `registry` branch. There is no GitHub release and no GitHub Pages site.

Nothing to copy: the workflow and the validation script live in this repository, and the SDK it validates against is the one in the tagged commit.

Published versions are immutable: to fix a release, publish a new version.
