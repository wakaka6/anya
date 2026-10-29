# 0002: A small core; engines, triggers and sinks around it

Accepted, 2026-10-30.

## Context

When pasting into the focused field is core behaviour, the overlay reads the recorder's internals and adding an engine means editing every package that lists engines by name, other programs cannot build alternatives to any of them. Pasting is what one app needs, not what a speech service is.

## Decision

Core owns capture, sessions and the frames it publishes. Everything else plugs in:

- **Engines** turn audio into recognitions behind the `Engine` seam and describe themselves with a `Manifest`. No crate outside an engine and the app names one.
- **Triggers** decide when a session opens and ends: the dictation hotkey, the continuous switch, an app's request.
- **Sinks** decide what happens to the text: paste, clipboard, overlay, stats, a subscriber.

## Consequences

- Built-in sinks read frames, so they prove the frames are enough for an outside app to build the same.
- Settings UIs draw engine settings from manifests and need no change for a new engine.
- Paste-specific settings (where text goes) belong to the output sink, not to core.
