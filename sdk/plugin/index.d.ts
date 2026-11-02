/** Text for the user: one string for every locale, or one per BCP-47 tag. */
export type Text = string | { [locale: string]: string };

/** One engine setting, stored under the engine's id in Anya's config. */
export type Field = {
  /** Key in the settings map `open` receives. */
  key: string;
  label: Text;
  help?: Text;
  /** Free text, a masked credential, or one of `options`. */
  kind: "text" | "secret" | "choice";
  /** Required with `kind: "choice"`. */
  options?: string[];
  /** The engine cannot open a session without it. */
  required?: boolean;
  /** Has a working default; UIs may fold it away. */
  advanced?: boolean;
};

/** How an engine describes itself; settings screens are drawn from it. */
export type Manifest = {
  /** Lowercase letters, digits and `-`; the engine's config key. */
  id: string;
  title: Text;
  /** One line under the title. */
  blurb?: Text;
  /** Where the engine's own documentation lives. */
  docs?: string;
  /**
   * True when results arrive while the user speaks. False: send one final
   * result after `finish`; partial results of a batch engine are not shown.
   */
  streaming: boolean;
  fields?: Field[];
};

/** One sentence as the engine split it. */
export type Utterance = {
  text: string;
  /** The engine will not change this sentence any more. */
  definite: boolean;
};

/** A recognition result. `text` is the session's whole text so far. */
export type Recognition = {
  text: string;
  utterances?: Utterance[];
  /** True only on the last result of the session. */
  final?: boolean;
};

/** What went wrong; Anya shows it translated. */
export type ErrorKind = "not_configured" | "connect" | "upstream";

/** What `open` is given. */
export type OpenParams = {
  /** BCP-47 tag; undefined leaves the language to the engine. */
  language?: string;
  /** Words to favour. */
  hotwords: string[];
  /** The values of the manifest's fields, by key. */
  settings: Record<string, string>;
};

/** Audio, the end of audio, and the end of the session. */
export type SessionEvents = {
  /** 40 ms of signed 16-bit little-endian mono PCM at 16 kHz (1280 bytes). */
  audio: (pcm: Uint8Array) => void | Promise<void>;
  /** No more audio: send the final result. */
  finish: () => void | Promise<void>;
  /** The session is over, also when abandoned; release what it holds. */
  close: () => void | Promise<void>;
};

/** One recognition session, as the plugin sees it. */
export interface EngineSession {
  /**
   * Registers a handler. Handlers run in order, one at a time, so audio is
   * never reordered. An exception ends the session with an `upstream`
   * error; throw an error with a `kind` property to choose another kind.
   */
  on<E extends keyof SessionEvents>(event: E, handler: SessionEvents[E]): this;
  /** Reports a result. After one with `final: true` the session is over. */
  result(recognition: Recognition): void;
  /** Ends the session with an error; `detail` is the engine's own words. */
  error(kind: ErrorKind, detail?: string): void;
}

/** An engine as a plugin's entry file exports it. */
export type Engine = {
  readonly manifest: Manifest;
  /**
   * Connects and registers handlers on `session`. Return (or resolve) when
   * ready: Anya then starts the session. Audio that arrives earlier waits.
   * A thrown error ends the session; `open` has 15 seconds.
   */
  open(params: OpenParams, session: EngineSession): void | Promise<void>;
};

/** Declares an engine; export the result from the entry file. */
export function defineEngine(engine: Engine): Engine;

/** The kinds `session.error` takes. */
export const errorKinds: readonly ErrorKind[];

/** For the host and for tests: where a session sends what the plugin reports. */
export type Sink = {
  result(recognition: Required<Recognition>): void;
  error(kind: ErrorKind, detail: string): void;
};

/** A session reporting to `sink`; tests drive it with `start` and `deliver`. */
export function createSession(sink: Sink): EngineSession;
/** Marks the session's handlers registered and releases waiting events. */
export function start(session: EngineSession): void;
/** Runs the handlers for an event, in order; resolves when they finish. */
export function deliver<E extends keyof SessionEvents>(
  session: EngineSession,
  event: E,
  arg?: Parameters<SessionEvents[E]>[0],
): Promise<void>;
/** Nothing more is accepted from the plugin. */
export function end(session: EngineSession): void;
/** The plugin sent its final result or an error. */
export function isEnded(session: EngineSession): boolean;

/** Where a dictation came from. */
export type Source = "dictation" | "continuous";

/** What every hook is told about the dictation. */
export type HookContext = {
  /** What opened the session. */
  source: Source;
  /** The focused app's id (bundle id on macOS, window class on X11), when the desktop tells it. */
  app?: string;
  /** The values of the package's `anya.fields`, by key. */
  settings: Record<string, string>;
  /** Aborted when Anya stops waiting for the call (its budget is spent). */
  signal: AbortSignal;
};

/** What a context hook is given: before the session opens. */
export type ContextCall = HookContext & {
  /** BCP-47 tag the session would open with; undefined leaves it to the engine. */
  language?: string;
  /** The hotwords the session would open with. */
  hotwords: string[];
};

/** What a context hook adds. */
export type Context = {
  /** Words to favour, added to the session's. */
  hotwords?: string[];
  /** The language to open with; the first plugin in the user's order that names one sets it. */
  language?: string;
};

/** What a transform or action hook is given: the session is over. */
export type TextCall = HookContext & {
  /** The session's id. */
  session?: string;
};

/** A context hook as a plugin's entry file exports it. */
export type ContextHook = {
  /** Returns hotwords to add and a language; nothing adds nothing. */
  context(ctx: ContextCall): Context | undefined | void | Promise<Context | undefined | void>;
};

/** A transform hook as a plugin's entry file exports it. */
export type TransformHook = {
  /** Returns the text that goes on; at most 64 KiB. */
  transform(text: string, ctx: TextCall): string | Promise<string>;
};

/** An action hook as a plugin's entry file exports it. */
export type ActionHook = {
  /** Does something with the final text. The return value is ignored; a throw is reported. */
  act(text: string, ctx: TextCall): void | Promise<void>;
};

/** Declares a context hook; export the result from the entry file. */
export function defineContext(hook: ContextHook): ContextHook;
/** Declares a transform hook; export the result from the entry file. */
export function defineTransform(hook: TransformHook): TransformHook;
/** Declares an action hook; export the result from the entry file. */
export function defineAction(hook: ActionHook): ActionHook;

/** Which `define*` made `value`, if one did. */
export function kindOf(value: unknown): "engine" | "context" | "transform" | "action" | undefined;

/** Checks a context hook's answer; throws a TypeError when it has the wrong shape. */
export function checkContext(answer: unknown): { hotwords: string[]; language?: string };
/** Checks a transform hook's answer; throws a TypeError unless it is a string. */
export function checkTransform(answer: unknown): string;
/** Checks an action hook's answer; throws a TypeError unless it is nothing. */
export function checkAction(answer: unknown): void;

/** Test helper: runs a context hook as the host does, `ctx` over defaults. */
export function callContext(
  hook: ContextHook,
  ctx?: Partial<ContextCall>,
): Promise<{ hotwords: string[]; language?: string }>;
/** Test helper: runs a transform hook as the host does; returns the text. */
export function callTransform(hook: TransformHook, text: string, ctx?: Partial<TextCall>): Promise<string>;
/** Test helper: runs an action hook as the host does. */
export function callAction(hook: ActionHook, text: string, ctx?: Partial<TextCall>): Promise<void>;
