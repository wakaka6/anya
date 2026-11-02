// @wakaka6/anya-plugin: what an Anya host plugin imports. Plain ESM, no build step.
// The plugin host serves this same file for `import "@wakaka6/anya-plugin"`, so a
// plugin needs no copy of it; installing it is for the types and for tests.

const ERROR_KINDS = ["not_configured", "connect", "upstream"];
const EVENTS = ["audio", "finish", "close"];
const ID_PATTERN = /^[a-z0-9-]+$/;

// What `define*` stamps on its result. A registered symbol, so a plugin that
// bundles its own copy of this file is still recognised by the host's.
const BRAND = Symbol.for("anya.plugin.define");
const KINDS = ["engine", "context", "transform", "action"];

/**
 * Which `define*` made `value`: "engine", "context", "transform" or "action";
 * undefined for anything else.
 */
export function kindOf(value) {
  const kind = value?.[BRAND];
  return KINDS.includes(kind) ? kind : undefined;
}

/**
 * Declares an engine. Returns the engine object a plugin's entry file
 * exports (by default or by name).
 */
export function defineEngine(engine) {
  if (engine === null || typeof engine !== "object") {
    throw new TypeError("defineEngine takes { manifest, open }");
  }
  const { manifest, open } = engine;
  if (manifest === null || typeof manifest !== "object") {
    throw new TypeError("defineEngine: manifest is missing");
  }
  if (typeof manifest.id !== "string" || !ID_PATTERN.test(manifest.id)) {
    throw new TypeError("defineEngine: manifest.id must be lowercase letters, digits and '-'");
  }
  if (manifest.title === undefined || manifest.title === "") {
    throw new TypeError("defineEngine: manifest.title is missing");
  }
  if (typeof manifest.streaming !== "boolean") {
    throw new TypeError("defineEngine: manifest.streaming must be true or false");
  }
  if (typeof open !== "function") {
    throw new TypeError("defineEngine: open must be a function");
  }
  return Object.freeze({ manifest: { fields: [], ...manifest }, open, [BRAND]: "engine" });
}

function defineHook(kind, method, hook) {
  if (hook === null || typeof hook !== "object") {
    throw new TypeError(`define${kind[0].toUpperCase()}${kind.slice(1)} takes { ${method} }`);
  }
  if (typeof hook[method] !== "function") {
    throw new TypeError(`define${kind[0].toUpperCase()}${kind.slice(1)}: ${method} must be a function`);
  }
  return Object.freeze({ [method]: hook[method], [BRAND]: kind });
}

/**
 * Declares a context hook: it runs before a dictation session opens and may
 * add hotwords and set the language. Export the result from the entry file.
 */
export function defineContext(hook) {
  return defineHook("context", "context", hook);
}

/**
 * Declares a transform hook: text in, text out, on a dictation's final text.
 * Export the result from the entry file.
 */
export function defineTransform(hook) {
  return defineHook("transform", "transform", hook);
}

/**
 * Declares an action hook: it does something with a dictation's final text,
 * beside the paste. Export the result from the entry file.
 */
export function defineAction(hook) {
  return defineHook("action", "act", hook);
}

function check(ok, message) {
  if (!ok) throw new TypeError(message);
}

/** A context hook's answer, checked: `{ hotwords, language }`. */
export function checkContext(answer) {
  if (answer === undefined || answer === null) return { hotwords: [], language: undefined };
  check(typeof answer === "object" && !Array.isArray(answer), "a context hook returns { hotwords?, language? } or nothing");
  const { hotwords = [], language } = answer;
  check(Array.isArray(hotwords) && hotwords.every((w) => typeof w === "string"), "context: hotwords must be an array of strings");
  check(language === undefined || typeof language === "string", "context: language must be a string");
  return { hotwords: [...hotwords], language };
}

/** A transform hook's answer, checked: the text. */
export function checkTransform(answer) {
  check(typeof answer === "string", "a transform hook returns a string");
  return answer;
}

/** An action hook's answer, checked: it returns nothing. */
export function checkAction(answer) {
  check(answer === undefined || answer === null, "an action hook returns nothing");
}

function expect(kind, hook) {
  check(kindOf(hook) === kind, `expected the result of define${kind[0].toUpperCase()}${kind.slice(1)}`);
  return hook;
}

function about(ctx) {
  return { source: "dictation", app: undefined, settings: {}, signal: new AbortController().signal, ...ctx };
}

/**
 * Test helper: runs a context hook the way the host does, with `ctx` over
 * defaults, and returns its checked answer `{ hotwords, language }`.
 */
export async function callContext(hook, ctx = {}) {
  return checkContext(await expect("context", hook).context({ language: undefined, hotwords: [], ...about(ctx) }));
}

/** Test helper: runs a transform hook the way the host does; returns the text. */
export async function callTransform(hook, text, ctx = {}) {
  return checkTransform(await expect("transform", hook).transform(text, { session: undefined, ...about(ctx) }));
}

/** Test helper: runs an action hook the way the host does. */
export async function callAction(hook, text, ctx = {}) {
  checkAction(await expect("action", hook).act(text, { session: undefined, ...about(ctx) }));
}

/** Kinds of failure `session.error` takes. */
export const errorKinds = Object.freeze([...ERROR_KINDS]);

const state = new WeakMap();

class EngineSession {
  on(event, handler) {
    if (!EVENTS.includes(event)) {
      throw new TypeError(`unknown session event '${event}'; use ${EVENTS.join(", ")}`);
    }
    if (typeof handler !== "function") {
      throw new TypeError("session.on takes a function");
    }
    state.get(this).handlers[event].push(handler);
    return this;
  }

  result(recognition) {
    const s = state.get(this);
    if (s.ended) return;
    if (recognition === null || typeof recognition !== "object" || typeof recognition.text !== "string") {
      throw new TypeError("session.result takes { text, utterances?, final? }");
    }
    const utterances = (recognition.utterances ?? []).map((u) => ({
      text: String(u.text),
      definite: Boolean(u.definite),
    }));
    const final = Boolean(recognition.final);
    if (final) s.ended = true;
    s.sink.result({ text: recognition.text, utterances, final });
  }

  error(kind, detail) {
    const s = state.get(this);
    if (s.ended) return;
    if (!ERROR_KINDS.includes(kind)) {
      throw new TypeError(`unknown error kind '${kind}'; use ${ERROR_KINDS.join(", ")}`);
    }
    s.ended = true;
    s.sink.error(kind, String(detail ?? ""));
  }
}

/**
 * A session that reports to `sink` (`result(recognition)`, `error(kind,
 * detail)`). The host builds one per `open`; tests build their own and drive
 * it with `deliver`.
 */
export function createSession(sink) {
  const session = new EngineSession();
  state.set(session, {
    sink,
    ended: false,
    ready: false,
    handlers: { audio: [], finish: [], close: [] },
    pending: [],
    chain: Promise.resolve(),
  });
  return session;
}

function run(session, event, arg) {
  const s = state.get(session);
  s.chain = s.chain.then(async () => {
    for (const handler of s.handlers[event]) {
      try {
        await handler(arg);
      } catch (e) {
        session.error(e?.kind ?? "upstream", e?.message ?? String(e));
      }
    }
  });
  return s.chain;
}

/**
 * Hands the session an event, in order: handlers run one at a time, each
 * after the previous one finished. Events sent before `start` wait for it.
 * Returns when the handlers are done.
 */
export function deliver(session, event, arg) {
  const s = state.get(session);
  if (!s.ready) {
    return new Promise((resolve) => s.pending.push([event, arg, resolve]));
  }
  return run(session, event, arg);
}

/** `open` returned: handlers are registered; delivers what waited. */
export function start(session) {
  const s = state.get(session);
  s.ready = true;
  const waiting = s.pending.splice(0);
  for (const [event, arg, resolve] of waiting) run(session, event, arg).then(resolve);
}

/** The session is over: nothing more is accepted. */
export function end(session) {
  state.get(session).ended = true;
}

/** The session ended on its own (final result or error). */
export function isEnded(session) {
  return state.get(session).ended;
}
