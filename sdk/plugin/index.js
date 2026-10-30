// @anyahq/plugin: what an Anya engine plugin imports. Plain ESM, no build step.
// The plugin host serves this same file for `import "@anyahq/plugin"`, so a
// plugin needs no copy of it; installing it is for the types and for tests.

const ERROR_KINDS = ["not_configured", "connect", "upstream"];
const EVENTS = ["audio", "finish", "close"];
const ID_PATTERN = /^[a-z0-9-]+$/;

/**
 * Declares an engine. Returns the engine object a plugin's entry file
 * exports as its default.
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
  return Object.freeze({ manifest: { fields: [], ...manifest }, open });
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
