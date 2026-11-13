# Plugin guide

[中文](plugin-guide.md) · [Plugin reference](plugins.md)

This guide takes you from nothing to a plugin installed, enabled and working in your own Anya, then published to the plugin market. It takes about 15 minutes. Every field, budget, failure rule and the index format are in the [plugin reference](plugins.md); here is only what one pass through needs.

## 0. Choose what to write

A plugin can provide four things, several in one package:

| You want to | Provide | For example |
| --- | --- | --- |
| Bring a speech recognition service | `engine` | your own ASR, Deepgram |
| Add hotwords or set the language for the app in front, before a dictation | `context` | your project's names in the editor |
| Rewrite the text before it is pasted | `transform` | drop filler words, spoken punctuation, polish with an LLM |
| Send the text elsewhere, beside the paste | `action` | keep it in notes, a to-do list, a log |

Then choose where it runs:

| | inline | host |
| --- | --- | --- |
| Runs in | QuickJS inside Anya | a Bun process of its own |
| May provide | `context`, `transform` | all four |
| Network, disk, npm packages | no | yes, with the user's rights |
| The user needs Bun | no | yes (Anya downloads it) |
| Good for | text work in milliseconds | anything that needs the network, the disk or heavy work |

**Go inline when you can.** Before enabling, the user reads that it cannot reach the network or the disk, which is easier to trust, and no Bun is needed.

You need [Bun](https://bun.sh) 1.2 or newer to run tests, and a checkout of Anya (`git clone https://github.com/wakaka6/anya.git`) to validate and try the plugin. `$ANYA` below is that checkout's path.

## 1. Your first plugin: dropping filler words

An inline transform that drops filler words like 嗯 and 呃 before the paste, with the list in the settings.

```
filler-words/
  package.json
  inline.js
  inline.test.js
```

`package.json`:

```json
{
  "name": "filler-words",
  "version": "1.0.0",
  "description": "Drops filler words like 嗯 and 呃 from dictation.",
  "type": "module",
  "files": ["inline.js"],
  "anya": {
    "id": "filler-words",
    "name": { "en": "Filler words", "zh-CN": "去掉语气词" },
    "description": {
      "en": "Drops 嗯, 呃 and the other filler words you list before the text is pasted",
      "zh-CN": "粘贴前去掉“嗯”“呃”等你列出的语气词"
    },
    "inline": "./inline.js",
    "compat": ">=0.0.0",
    "protocol": 1,
    "permissions": [],
    "fields": [
      {
        "key": "words",
        "label": { "en": "Filler words", "zh-CN": "语气词" },
        "help": { "en": "Separated by commas; empty uses 嗯, 呃, 啊", "zh-CN": "用逗号分隔；留空用“嗯、呃、啊”" },
        "kind": "text"
      }
    ]
  }
}
```

What matters:

- `anya.inline` names the entry. What the plugin provides is what the entry exports; there is nothing else to declare.
- `anya.name` and `anya.description` are what the plugin list shows, one per interface language if you like.
- `anya.fields` are the plugin's settings. Anya draws them as a form in the settings window, keeps the values under `[plugins.settings.filler-words]` in the config file and passes them as `ctx.settings`.
- `files` lists what goes into the package; `package.json` always does. Leave the entry out and it is not packed.

`inline.js`:

```js
// Runs in QuickJS inside Anya: one file, no import, no network, no disk.
const DEFAULT = ["嗯", "呃", "啊"];

export function transform(text, ctx) {
  const listed = (ctx?.settings?.words ?? "")
    .split(/[,，]/)
    .map((w) => w.trim())
    .filter(Boolean);
  for (const word of listed.length ? listed : DEFAULT) {
    // The word and the comma the engine put after it.
    text = text.replaceAll(new RegExp(`${word}[，,]?\\s*`, "g"), "");
  }
  return text.trim();
}
```

An inline entry is an ES module that exports plain functions: `transform(text, ctx)` returns the new text, `context(ctx)` returns `{ hotwords?, language? }` or nothing. It cannot `import` anything, and a call has 20 ms by default.

## 2. Test it

An inline plugin is plain functions, so call them. `inline.test.js`:

```js
import { expect, test } from "bun:test";
import { transform } from "./inline.js";

test("filler words and their commas go", () => {
  expect(transform("嗯，明天三点开会，呃，记得带周报。", {})).toBe("明天三点开会，记得带周报。");
});

test("the words the user lists replace the defaults", () => {
  const ctx = { settings: { words: "那个, 就是说" } };
  expect(transform("那个，就是说我们改到周五", ctx)).toBe("我们改到周五");
});
```

```console
$ bun test
 2 pass
 0 fail
```

## 3. Try it in your own Anya

Anya installs plugins only from a plugin index. To try yours, make an index on this computer that lists only it, and point Anya at it.

**Pack and validate.** This is the script the plugin market runs when it publishes, so a plugin that passes here passes there:

```bash
cd filler-words
npm pack                                        # filler-words-1.0.0.tgz
mkdir -p ../unpacked && tar xzf filler-words-1.0.0.tgz -C ../unpacked --strip-components=1
cd ..
bun $ANYA/scripts/validate-plugin.mjs unpacked filler-words/filler-words-1.0.0.tgz \
  "file://$PWD/filler-words/filler-words-1.0.0.tgz" index.json $ANYA/sdk/plugin/index.js
# ok: filler-words 1.0.0 transform sha512-…
```

The script checks the package, loads the entry to see what it exports, and writes the release into `index.json`. To try a change, delete `index.json` and bump the version first: a version goes into an index once.

**Point Anya at the index.** In `~/.config/anya/config.toml`:

```toml
[plugins]
index = "file:///absolute/path/index.json"
```

**Install and enable:**

```console
$ anya plugin install filler-words
Installed filler-words 1.0.0. It is disabled.
…
$ anya plugin enable filler-words
Enabled filler-words. Restart Anya to load it.
```

Restart Anya and open the Plugins page in the settings: the plugin is loaded, its transform is in the dictation hooks, and its “Filler words” setting unfolds under it. Say “嗯，明天开会” in any text field and “明天开会” is pasted.

The Plugins page installs it too: the market lists what the index holds. When you are done, remove `[plugins] index` and Anya is back on the real market.

## 4. Network or disk: a host plugin

Name the entry `anya.host` instead and it runs in a Bun process of its own, with every Node and Bun API. This action writes the time, the app and the length of each dictation to a file:

```js
// host.js
import { appendFile } from "node:fs/promises";
import { defineAction } from "@wakaka6/anya-plugin";

export const action = defineAction({
  async act(text, { app, settings }) {
    const line = `${new Date().toISOString()}\t${app ?? "-"}\t${[...text].length}\n`;
    await appendFile(settings.file, line);
  },
});
```

In `package.json`, write `"host": "./host.js"`, add a `file` field, and declare `"fs:write:<what>"` in `permissions` honestly: the user reads it before enabling.

What differs from inline:

- **Anya provides the SDK.** Anya's plugin host serves `@wakaka6/anya-plugin` at run time, so the package does not ship it. The SDK is not on npm yet; for types, point your editor at `$ANYA/sdk/plugin/index.d.ts`.
- **Bundle your dependencies.** Anya unpacks and runs the package; it never runs `npm install`. If you use npm packages, bundle with `bun build --target=bun`.
- **Keep the event loop free.** Anya pings the process every second and restarts one that has not answered for 3 seconds. Do heavy work in a `Worker`.
- **Honour `signal`.** Every call has a budget; `ctx.signal` aborts when it is spent. Pass it to `fetch`.

**Testing a host plugin.** Tests need the SDK served too. Put `anya-sdk.js` in the plugin's directory to load it from your Anya checkout, the way Anya does at run time:

```js
// anya-sdk.js
import { plugin } from "bun";
import * as sdk from "/absolute/path/anya/sdk/plugin/index.js";

plugin({
  name: "anya-sdk",
  setup: (build) => build.module("@wakaka6/anya-plugin", () => ({ exports: { ...sdk }, loader: "object" })),
});
```

```toml
# bunfig.toml
[test]
preload = ["./anya-sdk.js"]
```

Then the SDK's `callAction`, `callTransform` and `callContext` call a hook as Anya does, checking its answer:

```js
import { expect, test } from "bun:test";
import { readFile, rm } from "node:fs/promises";
import { callAction } from "@wakaka6/anya-plugin";
import { action } from "./host.js";

test("each dictation adds a line with its length", async () => {
  const file = `${import.meta.dir}/log.tsv`;
  await rm(file, { force: true });
  await callAction(action, "明天开会", { app: "com.apple.Notes", settings: { file } });
  expect((await readFile(file, "utf8")).trim().split("\t").slice(1)).toEqual(["com.apple.Notes", "4"]);
});
```

`anya-sdk.js` and `bunfig.toml` are for tests only; leave them out of `files`.

## 5. Speech engines

An engine plugin uses `defineEngine`: its `manifest` gives the title, whether it streams and its settings; `open` connects to your service, sends each `audio` chunk on and calls `session.result(...)` with what comes back. Starting from [`sdk/plugin/examples/echo-engine`](../sdk/plugin/examples/echo-engine/) is quickest; the session events, results and errors are in the reference's [Engines](plugins.md#engines) section. The SDK's `createSession`, `start` and `deliver` test an engine without Anya.

## 6. Publish to the plugin market

1. Fork [wakaka6/anya](https://github.com/wakaka6/anya), put the plugin under `plugins/<plugin id>/` with its tests, and open a pull request.
2. A maintainer reviews the code and the declared permissions. Be specific: `network:api.example.com`, not “network”.
3. Once merged, a maintainer tags it `<plugin id>-v<version>` (for example `filler-words-v1.0.0`). CI packs it, runs the script you ran in step 3, and commits the package, its icon and the index entry to the `registry` branch. The market shows it from then on.

A published version never changes; to fix one, publish a new version.

## Before you publish

- [ ] `anya.id` is lowercase letters, digits and `-`; an engine's `manifest.id` equals it.
- [ ] `files` lists the entries and the icon.
- [ ] `anya.compat` is the range of Anya versions you tested, never `*`.
- [ ] `permissions` lists the hosts, environment variables and paths you touch, honestly.
- [ ] Required settings are marked `"required": true`: until they are set Anya skips the plugin and tells the user what is missing, instead of failing every dictation.
- [ ] Error messages help the user and hold no secrets; no transcripts or credentials in logs.
- [ ] Optionally an icon: `anya.icon` names a PNG or SVG in the package (never a URL), square, at most 1 MiB.
- [ ] The step 3 validation passes, and so does `bun test`.

## Troubleshooting

**`anya plugin install` cannot find the plugin.** Check that `[plugins] index` is `file://` and an absolute path, and that the id is in `index.json`.

**Install says this Anya cannot run it.** `anya.compat` does not include the Anya you run. An Anya built from source is version `0.0.0`; write `>=0.0.0` while you try it.

**Enabled, but nothing happens.** Enabling on the command line takes effect when Anya restarts; in the settings window, at once. The Plugins page shows the plugin's state, including missing settings and why it failed to load. A plugin's `console.log` goes to the debug log: start `anya --daemon --verbose` to see it in the terminal.

**The transform was skipped.** When a transform runs out of time, throws or returns something other than a string, Anya skips that step, goes on with the text before it, and names the plugin on the Activity page. Inline calls have 20 ms by default and host transforms 2 seconds; ask for more in `anya.limits`, within the [budgets](plugins.md#budgets).

**Validation says the inline entry contains an `import`.** An inline entry must be one file that loads nothing. Bundle it into one file, or make it a host entry.

## More examples

- [`plugins/`](../plugins/): the official plugins and their tests. `spoken-punctuation` has a context and a transform, `app-hotwords` uses the app in front and a setting, `daily-notes` is a host action that writes files.
- [`sdk/plugin/examples/`](../sdk/plugin/examples/): `inline`, `hooks` (a host transform and action) and `echo-engine` (a streaming engine).
