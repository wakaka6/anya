# 0019: No built-in replacement list

Accepted, 2026-11-03.

## Context

0016 shipped a built-in transform, `replace`: `spoken => written` lines in `replacements.txt` next to the config file, applied to every dictation before it is pasted. Using it meant creating a file by hand in a format the settings page could only describe; the page offered a switch, a paragraph on the syntax and a path, and no way to see or edit a rule. Hotwords already steer the engine toward the words a user cares about, and a rule that rewrites text after recognition is what a transform plugin does (docs/plugins.md has one as its inline example).

## Decision

- The built-in `replace` plugin, `replacements.txt` and the code that reads it are removed. A user who wants rewriting installs or writes a transform plugin.
- `Runs::Builtin` goes with it: no built-in piece serves a hook, so every hook runs inline or in a host process.
- `community-hooks` stays the one built-in piece on the hook points.
- Supersedes 0016's consequence that a built-in replacement list is the transform point's second adapter. The `TransformHook` trait keeps its two adapters, inline and host plugins.

## Consequences

- A `replacements.txt` left next to the config file is ignored. A `replace` row in `[plugins.builtin]` or `[plugins] order` names a plugin that does not exist and changes nothing.
- The Plugins page lists one built-in hook switch.
