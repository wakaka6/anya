# 0012: Built-in pieces run on one plugin runtime

Accepted, 2026-10-30.

## Context

Built-in engines are a fixed list in the binary, and the engines of community plugins (0004) are a second list the app joins to it by hand. Neither can be switched off, and there is no one place that says what is running and why. Triggers and sinks will want the same treatment once a second adapter needs the same seam (0002). magpie's plugin runtime solved this for a Go program with global registration from `init`, string-named services and disposers passed as callbacks. Rust has better tools for each of those.

## Decision

`anya-plugin-rt`, a core-layer crate with no dependency beyond `thiserror` and `tracing`, holds the mechanism and knows nothing of Anya's seams:

- **`Plugin<P>`** has an id and `apply(&self, points: &P) -> Result<Active, ApplyError>`. `P` is whatever the app offers plugins to serve into; the app defines it. Nothing is registered globally: the app builds the list of plugins itself, and a repeated id is an error when the runtime is made.
- **`Point<T>`** is a place plugins serve things at. `serve` returns a `Registration`; dropping it withdraws the item. `all` lists what is served, so a point with nothing served is empty, never an error. The first point is `engines`, in the app.
- **`Active`** is what `apply` returns: the registrations a plugin holds. Dropping it releases them newest first. A plugin needs no dispose callback; what it holds is what it disposes of.
- **`Runtime<P>::reconcile`** brings the active set to the wanted set: plugins no longer wanted are dropped newest first, then wanted ones are applied in list order. A plugin whose `apply` fails is `Failed` with the reason and the others go on. Each reconcile tries a failed plugin again.
- **`compose`** decides what is wanted: every plugin is on, a row in `[plugins.builtin]` of `config.toml` turns one off or on, and a plugin in use (the engine any use, dictation, continuous or remote, chose) stays on whatever the row says. The result says why, as `Why::Default`, `User` or `InUse`.

The app wraps each built-in engine in one adapter, so engine crates do not change. The Bun plugin host serves its engines into the same point. Community plugins keep their own install, enable and update flow (0010); `enabled` in `plugins.json` stays the user's say over them.

Left out until a plugin needs them: services a plugin requires or provides, an API version range between the runtime and a plugin, and a preset of essentials. magpie has all three; each is added with the first plugin that asks.

## Consequences

- The `engines` list the settings page shows is what the runtime has applied. An engine the user turns off disappears from it; the one in use cannot be turned off.
- `anya plugin builtin` lists the built-in plugins with their state and why, and turns one on or off. The change applies at the next start, as `anya plugin enable` does.
- A new seam that plugins serve gets a `Point` and a field of the app's `P`, not a new registration scheme.
- The runtime has no sandbox and no isolation: it manages plugins that are compiled into the binary and trusted as the binary is.
