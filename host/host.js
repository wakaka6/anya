// The Anya plugin host: one Bun process that loads one plugin package and runs
// its engine's sessions and its context, transform and action hooks. Anya
// writes JSON lines to stdin and reads JSON lines from stdout
// (crates/protocol/src/plugin.rs). Nothing else may reach stdout, so plugin
// console output is turned into `log` events.
//
// Anya starts one of these per enabled plugin, writes this file and `sdk.js`
// (sdk/plugin/index.js) side by side into its runtime directory and runs
// `bun host.js`. It pings every second; a `pong` is answered from the stdin
// loop, so a plugin that blocks the event loop stops answering and is killed.

import { AsyncLocalStorage } from "node:async_hooks";
import { readFile } from "node:fs/promises";
import { format } from "node:util";
import { pathToFileURL } from "node:url";
import path from "node:path";
import {
  callAction,
  callContext,
  callTransform,
  createSession,
  deliver,
  end,
  isEnded,
  kindOf,
  start,
} from "./sdk.js";

const HOST_VERSION = "1";
const PROTOCOL = 1;
const LOAD_TIMEOUT_MS = Number(process.env.ANYA_PLUGIN_LOAD_TIMEOUT_MS ?? 15000);
const OPEN_TIMEOUT_MS = Number(process.env.ANYA_PLUGIN_OPEN_TIMEOUT_MS ?? 15000);

// Whose code is running: set around the plugin's load, its session callbacks
// and its hook calls.
const running = new AsyncLocalStorage();

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
  return running.getStore()?.plugin ?? "host";
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

// The one package this process loads: { plugin, engine?, context?, transform?, action? }.
let loaded = null;
// Set once a `load` was accepted: a second one is refused.
let taken = false;
// Open sessions by id: { plugin, session }.
const sessions = new Map();
// Hook calls in flight by call id: their AbortController.
const calls = new Map();

const KIND_ORDER = ["engine", "context", "transform", "action"];

async function load({ plugin, path: dir }) {
  if (taken) {
    emit({ type: "load_failed", plugin, detail: "this process already loaded a plugin" });
    return;
  }
  taken = true;
  try {
    const found = await running.run({ plugin }, () =>
      withTimeout(loadPackage(dir), LOAD_TIMEOUT_MS, "loading"),
    );
    loaded = { plugin, ...found };
    emit({
      type: "loaded",
      plugin,
      engine: found.engine?.manifest,
      provides: KIND_ORDER.filter((kind) => found[kind]),
    });
  } catch (e) {
    emit({ type: "load_failed", plugin, detail: errorText(e) });
  }
}

// Imports the host entry and sorts its exports, default and named, by the
// `define*` that made them: at most one of each kind.
async function loadPackage(dir) {
  const pkg = JSON.parse(await readFile(path.join(dir, "package.json"), "utf8"));
  const entry = pkg?.anya?.host;
  if (typeof entry !== "string") {
    throw new Error('package.json has no "anya": { "host": "./file.js" }');
  }
  const file = path.resolve(dir, entry);
  if (path.relative(dir, file).startsWith("..")) {
    throw new Error("the host entry lies outside the package");
  }
  const mod = await import(pathToFileURL(file).href);
  const found = {};
  const seen = new Set();
  for (const value of Object.values(mod)) {
    if (seen.has(value)) continue;
    seen.add(value);
    const kind = kindOf(value);
    if (!kind) continue;
    if (found[kind]) throw new Error(`the entry exports two ${kind} hooks; a package has at most one of each kind`);
    found[kind] = value;
  }
  if (KIND_ORDER.every((kind) => !found[kind])) {
    throw new Error(
      "the entry exports nothing made with defineEngine, defineContext, defineTransform or defineAction",
    );
  }
  return found;
}

// Takes the session out and lets the plugin release what it holds.
function release(id) {
  const entry = sessions.get(id);
  if (!entry) return undefined;
  sessions.delete(id);
  end(entry.session);
  return running.run({ plugin: entry.plugin, session: id }, () => deliver(entry.session, "close"));
}

function fail(id, kind, detail) {
  if (!sessions.has(id)) return;
  emit({ type: "session_error", session: id, code: kind, detail });
  emit({ type: "session_closed", session: id });
  void release(id);
}

async function open({ session: id, plugin, engine: engineId, language, hotwords, settings }) {
  const engine = loaded?.plugin === plugin ? loaded.engine : undefined;
  if (!engine || engine.manifest.id !== engineId) {
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
    await running.run({ plugin, session: id }, () =>
      withTimeout(Promise.resolve(engine.open(params, session)), OPEN_TIMEOUT_MS, "open"),
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
  return running.run({ plugin: entry.plugin, session: id }, () => deliver(entry.session, event, arg));
}

function close(id) {
  if (!sessions.has(id)) return undefined;
  emit({ type: "session_closed", session: id });
  return release(id);
}

// A hook call that ended with an error. Nothing is sent for a call Anya
// stopped waiting for: it has forgotten it.
function failCall(id, detail) {
  const controller = calls.get(id);
  if (!controller) return;
  calls.delete(id);
  if (!controller.signal.aborted) emit({ type: "call_failed", call: id, detail });
}

// Runs the loaded plugin's hook of `kind` for `command`; `invoke` calls it
// with the SDK helper and returns the event to send.
function hook(kind, command, invoke) {
  const id = command.call;
  const def = loaded?.plugin === command.plugin ? loaded[kind] : undefined;
  if (!def) {
    emit({ type: "call_failed", call: id, detail: `plugin ${command.plugin} has no ${kind} hook` });
    return;
  }
  const controller = new AbortController();
  calls.set(id, controller);
  const base = {
    source: command.source ?? "dictation",
    app: command.app,
    settings: command.settings ?? {},
    signal: controller.signal,
  };
  void running
    .run({ plugin: command.plugin, call: id }, async () => {
      const event = await invoke(def, base);
      if (calls.delete(id) && !controller.signal.aborted) emit({ ...event, call: id });
    })
    .catch((e) => failCall(id, errorText(e)));
}

function cancel(id) {
  const controller = calls.get(id);
  if (!controller) return;
  calls.delete(id);
  controller.abort();
}

async function shutdown() {
  for (const id of [...calls.keys()]) cancel(id);
  const closing = [...sessions.keys()].map((id) => close(id));
  await withTimeout(Promise.allSettled(closing), 2000, "shutdown").catch(() => {});
  process.exit(0);
}

function handle(command) {
  switch (command.type) {
    case "ping":
      emit({ type: "pong", seq: command.seq });
      break;
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
    case "context":
      hook("context", command, async (def, base) => {
        const { hotwords, language } = await callContext(def, {
          ...base,
          session: command.session,
          language: command.language,
          hotwords: command.hotwords ?? [],
        });
        return { type: "context_done", hotwords, language };
      });
      break;
    case "transform":
      hook("transform", command, async (def, base) => ({
        type: "transformed",
        text: await callTransform(def, command.text, { ...base, session: command.session }),
      }));
      break;
    case "action":
      hook("action", command, async (def, base) => {
        await callAction(def, command.text, { ...base, session: command.session });
        return { type: "action_done" };
      });
      break;
    case "cancel":
      cancel(command.call);
      break;
    case "shutdown":
      void shutdown();
      break;
    default:
      break;
  }
}

// A plugin that throws outside a handler must not take the host down: its
// session or call, if the throw happened in one, fails; the rest go on.
function unhandled(e) {
  const store = running.getStore();
  const detail = errorText(e);
  if (store?.session) fail(store.session, "upstream", detail);
  else if (store?.call) failCall(store.call, detail);
  else log(store?.plugin ?? "host", "error", `unhandled: ${detail}`);
}
process.on("uncaughtException", unhandled);
process.on("unhandledRejection", unhandled);

// `import "@wakaka6/anya-plugin"` in a plugin is this host's own copy of the SDK.
const sdk = { ...(await import("./sdk.js")) };
Bun.plugin({
  name: "anya-sdk",
  setup(build) {
    build.module("@wakaka6/anya-plugin", () => ({ exports: sdk, loader: "object" }));
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
