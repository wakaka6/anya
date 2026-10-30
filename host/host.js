// The Anya plugin host: one Bun process that loads engine plugins and runs
// their sessions. Anya writes JSON lines to stdin and reads JSON lines from
// stdout (crates/protocol/src/plugin.rs). Nothing else may reach stdout, so
// plugin console output is turned into `log` events.
//
// Anya writes this file and `sdk.js` (sdk/plugin/index.js) side by side into
// its runtime directory and runs `bun host.js`.

import { AsyncLocalStorage } from "node:async_hooks";
import { readFile } from "node:fs/promises";
import { format } from "node:util";
import { pathToFileURL } from "node:url";
import path from "node:path";
import { createSession, deliver, end, isEnded, start } from "./sdk.js";

const HOST_VERSION = "1";
const PROTOCOL = 1;
const LOAD_TIMEOUT_MS = Number(process.env.ANYA_PLUGIN_LOAD_TIMEOUT_MS ?? 15000);
const OPEN_TIMEOUT_MS = Number(process.env.ANYA_PLUGIN_OPEN_TIMEOUT_MS ?? 15000);

// Whose code is running: set around a plugin's load and its session callbacks.
const context = new AsyncLocalStorage();

const rawWrite = process.stdout.write.bind(process.stdout);
function emit(event) {
  rawWrite(JSON.stringify(event) + "\n");
}

function log(plugin, level, message) {
  emit({ type: "log", plugin, level, message });
}

function hostLog(level, message) {
  log("host", level, message);
}

function whoIsRunning() {
  return context.getStore()?.plugin ?? "host";
}

for (const [method, level] of [
  ["log", "info"],
  ["info", "info"],
  ["debug", "debug"],
  ["warn", "warn"],
  ["error", "error"],
  ["trace", "debug"],
]) {
  console[method] = (...args) => log(whoIsRunning(), level, format(...args));
}
process.stdout.write = (chunk) => {
  log(whoIsRunning(), "info", String(chunk).replace(/\n$/, ""));
  return true;
};
process.stderr.write = (chunk) => {
  log(whoIsRunning(), "error", String(chunk).replace(/\n$/, ""));
  return true;
};

function withTimeout(promise, ms, what) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${what} timed out after ${ms / 1000} s`)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function errorText(e) {
  return e?.message ?? String(e);
}

// Plugins by package id: { engine }.
const plugins = new Map();
// Open sessions by id: { plugin, session }.
const sessions = new Map();

async function load({ plugin, path: dir }) {
  try {
    const engine = await context.run({ plugin }, () =>
      withTimeout(loadEngine(dir), LOAD_TIMEOUT_MS, "loading"),
    );
    plugins.set(plugin, { engine });
    emit({ type: "loaded", plugin, manifest: engine.manifest });
  } catch (e) {
    emit({ type: "load_failed", plugin, detail: errorText(e) });
  }
}

async function loadEngine(dir) {
  const pkg = JSON.parse(await readFile(path.join(dir, "package.json"), "utf8"));
  const entry = pkg?.anya?.engine;
  if (typeof entry !== "string") {
    throw new Error('package.json has no "anya": { "engine": "./file.js" }');
  }
  const file = path.resolve(dir, entry);
  if (path.relative(dir, file).startsWith("..")) {
    throw new Error("the engine entry lies outside the package");
  }
  const mod = await import(pathToFileURL(file).href);
  const engine = mod.default;
  if (typeof engine?.open !== "function" || typeof engine?.manifest?.id !== "string") {
    throw new Error("the entry does not export defineEngine(...) as its default");
  }
  return engine;
}

// Takes the session out and lets the plugin release what it holds.
function release(id) {
  const entry = sessions.get(id);
  if (!entry) return undefined;
  sessions.delete(id);
  end(entry.session);
  return context.run({ plugin: entry.plugin, session: id }, () => deliver(entry.session, "close"));
}

function fail(id, kind, detail) {
  if (!sessions.has(id)) return;
  emit({ type: "session_error", session: id, code: kind, detail });
  emit({ type: "session_closed", session: id });
  void release(id);
}

async function open({ session: id, plugin, engine: engineId, language, hotwords, settings }) {
  const entry = plugins.get(plugin);
  if (!entry || entry.engine.manifest.id !== engineId) {
    emit({ type: "session_error", session: id, code: "upstream", detail: `plugin ${plugin} is not loaded` });
    emit({ type: "session_closed", session: id });
    return;
  }
  const session = createSession({
    result(recognition) {
      if (!sessions.has(id)) return;
      emit({ type: "recognition", session: id, ...recognition });
      if (recognition.final) {
        emit({ type: "session_closed", session: id });
        void release(id);
      }
    },
    error(kind, detail) {
      fail(id, kind, detail);
    },
  });
  sessions.set(id, { plugin, session });
  const params = { language, hotwords: hotwords ?? [], settings: settings ?? {} };
  try {
    await context.run({ plugin, session: id }, () =>
      withTimeout(Promise.resolve(entry.engine.open(params, session)), OPEN_TIMEOUT_MS, "open"),
    );
  } catch (e) {
    fail(id, e?.kind ?? "connect", errorText(e));
    return;
  }
  if (!sessions.has(id) || isEnded(session)) return;
  start(session);
  emit({ type: "session_ready", session: id });
}

function forSession(id, event, arg) {
  const entry = sessions.get(id);
  if (!entry) return undefined;
  return context.run({ plugin: entry.plugin, session: id }, () => deliver(entry.session, event, arg));
}

function close(id) {
  if (!sessions.has(id)) return undefined;
  emit({ type: "session_closed", session: id });
  return release(id);
}

async function shutdown() {
  const closing = [...sessions.keys()].map((id) => close(id));
  await withTimeout(Promise.allSettled(closing), 2000, "shutdown").catch(() => {});
  process.exit(0);
}

function handle(command) {
  switch (command.type) {
    case "load":
      void load(command);
      break;
    case "open":
      void open(command);
      break;
    case "audio":
      void forSession(command.session, "audio", Buffer.from(command.pcm, "base64"));
      break;
    case "finish":
      void forSession(command.session, "finish");
      break;
    case "close":
      void close(command.session);
      break;
    case "shutdown":
      void shutdown();
      break;
    default:
      break;
  }
}

// A plugin that throws outside a handler must not take the host down: its
// session, if the throw happened in one, fails; the rest go on.
function unhandled(e) {
  const store = context.getStore();
  const detail = errorText(e);
  if (store?.session) fail(store.session, "upstream", detail);
  else log(store?.plugin ?? "host", "error", `unhandled: ${detail}`);
}
process.on("uncaughtException", unhandled);
process.on("unhandledRejection", unhandled);

// `import "@anyahq/plugin"` in a plugin is this host's own copy of the SDK.
const sdk = { ...(await import("./sdk.js")) };
Bun.plugin({
  name: "anya-sdk",
  setup(build) {
    build.module("@anyahq/plugin", () => ({ exports: sdk, loader: "object" }));
  },
});

emit({ type: "ready", host_version: HOST_VERSION, protocol: PROTOCOL });

// stdin closing means Anya is gone: leave with it.
const decoder = new TextDecoder();
let buffered = "";
for await (const chunk of Bun.stdin.stream()) {
  buffered += decoder.decode(chunk, { stream: true });
  let newline;
  while ((newline = buffered.indexOf("\n")) >= 0) {
    const line = buffered.slice(0, newline).trim();
    buffered = buffered.slice(newline + 1);
    if (!line) continue;
    let command;
    try {
      command = JSON.parse(line);
    } catch {
      hostLog("warn", "ignored a line that is not JSON");
      continue;
    }
    try {
      handle(command);
    } catch (e) {
      hostLog("error", `command ${command?.type}: ${errorText(e)}`);
    }
  }
}
await shutdown();
