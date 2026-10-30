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

/** Declares an engine; export the result as the entry file's default. */
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
