# 0016: Context, transform and action hooks around dictation

Accepted, 2026-10-30.

## Context

A plugin can only add an engine. What decides how a session listens, what its text becomes and where the text goes is fixed in the binary, so the community can add recognizers but not the features users ask for around them: vocabulary for the app in front, polished text, notes and to-dos kept from what was said. Downstream apps (0007) read frames; they cannot change what is pasted.

## Decision

Three more points beside `engines` (0012), for local dictation sessions only. The four are peers: a package provides any of them (`provides`, 0015), and a built-in piece serves into the same point as a community plugin. A built-in plugin serves the community's hooks into theirs, so turning that plugin off stops every community hook.

- **`context`**, inline or host, before `Engine::open`. It gets the source, the focused app's id when the desktop tells it, and the language and hotwords the session would open with. It returns hotwords to add and a language; the first plugin in the user's order that names a language sets it. It cannot choose the engine.
- **`transform`**, inline or host, on the final text. Text in, text out. Transforms run as one chain in the order the user sets, inline and host mixed. A transform computes; calling a service to compute (an LLM) is fine, changing the world (writing a note) is an action's job.
- **`action`**, host only, after the chain. Every enabled action gets the chain's text with the session's id, source and focused app, and runs beside the paste and the other actions; one plugin's actions run in session order. Paste is the built-in action, on by default and switchable off.

Budgets (0015), default and range:

| Budget | Default | Range |
| --- | --- | --- |
| an inline call | 20 ms | 1–200 ms |
| an inline plugin's memory | 16 MiB | 4–256 MiB |
| a host `context` | 300 ms | 50–2000 ms |
| a host `transform` | 2000 ms | 100–10000 ms |
| the whole transform chain | 4000 ms | 500–15000 ms |
| an `action` | 10000 ms | 1000–60000 ms |

When a hook fails:

| Hook | Failure | What happens |
| --- | --- | --- |
| `context` | error, budget spent, wrong shape | the session opens with what it had; logged |
| `transform` | error, budget spent, not a string, over 64 KiB | the step is skipped and the text before it goes on; the user is told which plugin |
| `action` | error, budget spent | the user is told which plugin; paste and the other actions are untouched |

Every hook gets a copy of its input. Diagnostics keep fixed codes, counts and durations, never text (0005). A plugin with a `context` hook learns the focused app and one with a `transform` or `action` hook receives everything dictated; the enable prompt says so, as it says an engine receives audio.

Left out until a plugin needs them: a `command` hook (when two command actions collide or commands must be listed), action sets per hotkey, hooks on partial results or on continuous and remote sessions, triggers, audio sources, and plugin parts in the UIs.

## Consequences

- The text pasted is the chain's; frames still carry the engine's (0003), so a subscriber sees what was recognised, not what was pasted. Publishing the delivered text is a protocol change of its own.
- The dictation controller hands its final text to the chain and the actions on a task, as it does paste now; hotkey callbacks are unchanged.
- The SDK gains `defineContext`, `defineTransform` and `defineAction`; inline modules export the same hooks as plain functions.
- A built-in replacement list is the transform point's second adapter, so the point gets its trait with the first community transform.
