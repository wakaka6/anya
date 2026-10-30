# Writing engine plugins

An engine plugin adds a speech recognizer to Anya without a pull request or a release. It is an npm-style package that Anya runs in one Bun process, the plugin host (docs/decisions/0004). Anya shows it as one more engine: it appears in the engine list, takes settings drawn from its manifest, and receives the audio of the sessions the user opens on it. Gateway clients can use it too (docs/decisions/0009); they never see the plugin or its credentials.

A plugin hears the microphone and runs with the user's rights. Anya shows the user the plugin's name, that it receives audio, whether it streams, and the permissions it declares, before the user enables it. Keep the permission list honest; the registry reviews it.

## Package layout

```
my-engine/
  package.json
  engine.js        # the entry named in package.json
```

```json
{
  "name": "my-engine",
  "version": "1.0.0",
  "description": "One line for the marketplace.",
  "type": "module",
  "anya": {
    "id": "my-engine",
    "engine": "./engine.js",
    "compat": ">=0.1.0",
    "protocol": 1,
    "permissions": ["network:api.example.com"]
  },
  "devDependencies": { "@anyahq/plugin": "^1.0.0" }
}
```

- `anya.engine` is the only field the host reads at load time. The rest is read by the registry's CI and ends up in the index.
- `anya.id` is the plugin id (default: `name`): lowercase letters, digits and `-`. The manifest's `id` must equal it.
- `anya.compat` is a semver range of Anya versions. `anya.protocol` is the plugin protocol, currently `1`. Anya installs only releases of its own protocol.
- `anya.permissions` are strings the user sees: `network:<host>`, `env:<NAME>`, `fs:read:<path>`. They are declarations for review and display; the host does not sandbox the plugin.

Dependencies a plugin needs at run time must be bundled into the package (`bun build --target=bun`, or `bundledDependencies`): Anya unpacks the tarball and runs it, it does not run `npm install`.

## The SDK

`import { defineEngine } from "@anyahq/plugin"` works in every plugin; the host serves its own copy of the SDK, so the package does not ship one. Install `@anyahq/plugin` as a dev dependency for the types (`sdk/plugin/index.d.ts`).

```js
import { defineEngine } from "@anyahq/plugin";

export default defineEngine({
  manifest: {
    id: "my-engine",
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

**Manifest.** `streaming: true` means results arrive while the user speaks. With `streaming: false`, send one result with `final: true` after `finish`; Anya never shows partial text of a batch engine. Settings UIs are drawn from `fields`, so a plugin ships no UI. `Text` values are a string or a `{ "en": …, "zh-CN": … }` map.

**`open(params, session)`** connects and registers handlers. When it returns (or resolves) the session starts; audio that arrived earlier waits. It has 15 seconds. `params.settings` holds the values of your fields by key (strings), read fresh at every session.

**Session events.** `on("audio", pcm)` gets a `Uint8Array` of 40 ms, signed 16-bit little-endian mono PCM at 16 kHz (1280 bytes). `on("finish")` means no more audio: send the final result. `on("close")` means the session is over, also when the user cancelled; release what you hold. Handlers run in order, one at a time.

**Results.** `session.result({ text, utterances?, final? })`: `text` is the session's whole text so far, not a delta; `utterances` are `{ text, definite }` sentences; `final: true` ends the session and must be the last result. After it, further calls are ignored.

**Errors.** `session.error(kind, detail)` ends the session. `kind` is `not_configured` (`detail`: the field key), `connect` or `upstream`. A thrown exception in a handler ends the session with `upstream`; an exception thrown from `open` ends it with `connect`. Throw an error with a `kind` property to choose another kind. Anya shows the user the translated kind with your `detail` beside it, and does not retry on another engine (docs/decisions/0005), so make `detail` useful and free of secrets.

**Console.** `console.log` and friends are captured and logged by Anya at debug level; nothing a plugin writes to stdout can break the host protocol. Do not log transcripts or credentials anyway.

**Isolation.** An exception or rejection in one plugin ends that plugin's session; other plugins are untouched. A plugin whose module does not load within 15 seconds is marked failed while the others load.

## Testing locally

Unit-test an engine without Anya by driving a session yourself:

```js
import { createSession, start, deliver } from "@anyahq/plugin";
import engine from "./engine.js";

const results = [];
const session = createSession({ result: (r) => results.push(r), error: (k, d) => results.push({ k, d }) });
await engine.open({ hotwords: [], settings: {} }, session);
start(session);
await deliver(session, "audio", new Uint8Array(1280));
await deliver(session, "finish");
```

`sdk/plugin/examples/echo-engine` is a complete streaming plugin (partials with byte counts, a final on finish) to copy from.

To try the real thing, serve a local registry index. Pack the plugin and describe the tarball:

```bash
npm pack                                   # my-engine-1.0.0.tgz
node -e 'const c=require("crypto"),f=require("fs");console.log("sha512-"+c.createHash("sha512").update(f.readFileSync("my-engine-1.0.0.tgz")).digest("base64"))'
```

```json
{ "version": 1, "plugins": [{
  "id": "my-engine", "name": "My engine",
  "versions": [{
    "version": "1.0.0",
    "artifact": "file:///abs/path/my-engine-1.0.0.tgz",
    "integrity": "sha512-…",
    "anya": ">=0.0.0", "protocol": 1, "kind": "engine",
    "streaming": true, "permissions": []
  }]
}] }
```

Point Anya's plugin registry URL at `file:///abs/path/index.json` (`Registry::fetch` reads `file://` and `https://`), install, then enable the plugin. `docs/plugins/registry-ci/validate.mjs` runs the registry's checks on your package locally:

```bash
bun validate.mjs <unpacked-dir> my-engine-1.0.0.tgz <artifact-url> index.json <path-to>/sdk/plugin/index.js
```

## Registry index format (version 1)

`https://raw.githubusercontent.com/anyahq/anyahq-plugins/dist/index.json`, the `dist` branch of the registry repository, written by the registry's CI and read by Anya when the user opens the marketplace or checks for updates. Tarballs sit beside it in `dist/tgz/`. Types: `anya_protocol::Index`.

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
| `versions[].kind` | `engine`. |
| `versions[].streaming` | The engine's `manifest.streaming`, for listing before install. |
| `versions[].permissions` | Strings shown before enabling. |

Unknown fields are ignored. Anya installs a release only when `kind` is `engine`, `protocol` equals its own and `anya` matches its version.

## What Anya does with a release

1. **Install** (a user action): downloads the artifact, checks `integrity`, refuses archives with links or paths that leave the package, unpacks into `plugins/<id>/<version>/` and records `{id, version, integrity, enabled: false}` in `plugins/plugins.json`.
2. **Enable** (a separate user action): the plugin is loaded by the host the next time the host starts or reloads.
3. **Update**: a newer version is installed explicitly, replaces the pinned version and starts disabled. Nothing installs or updates by itself, and Anya runs no Git tag listener.

Bun comes from `PATH` if it is version 1.2.0 or newer; otherwise Anya downloads its pinned Bun release (SHA-256 checked) when the first plugin is installed. Without plugins nothing is downloaded.

## The anyahq-plugins release flow

The registry repository `anyahq-plugins` holds one directory per plugin under `plugins/<id>/` and the CI below. Plugin authors open a pull request to add or change a plugin; a maintainer reviews code and the declared permissions. Releasing:

1. A maintainer tags the merged commit `<plugin-id>-v<version>` (for example `echo-v1.2.0`). The tag version must equal `package.json`'s.
2. The workflow packs the plugin (`npm pack`) and runs `scripts/validate.mjs` against the Anya SDK, which checks: id and version format; `anya.engine`, `anya.compat` (not unbounded), `anya.protocol` and `anya.permissions` are present and well formed; the entry loads and exports `defineEngine(...)`; `manifest.id` equals the plugin id and `streaming` is a boolean; the version is not already in the index.
3. It computes the tarball's `sha512` integrity, appends the entry to `index.json` and commits the tarball (`tgz/`) and the index to the `dist` branch. There is no GitHub release and no GitHub Pages site.

Copy `docs/plugins/registry-ci/release.yml` to `.github/workflows/release.yml` and `docs/plugins/registry-ci/validate.mjs` to `scripts/validate.mjs` in the registry repository. The workflow pins Bun and downloads the SDK from the Anya repository; pin that URL to the Anya release the registry targets.

Published versions are immutable: to fix a release, publish a new version.
