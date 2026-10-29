# 0001: Rust, Linux and macOS only

Accepted, 2026-10-30.

## Context

A voice-input tool whose capture, recognition, paste, overlay and subscription server know each other directly cannot become a service others build on; it needs seams from the start. Windows support costs a third of the platform code (its own capture, key polling and overlay) and its own class of modifier-key bugs.

## Decision

Anya is written in Rust and supports Linux (Wayland and X11) and macOS. Windows is not a target; the layer rules refuse Windows crates and cargo-deny checks only Linux and macOS targets.

## Consequences

- The workspace states the seams as crates, so a wrong dependency fails the build instead of a review.
- Mature crates cover each layer: cpal, evdev, x11rb, smithay-client-toolkit, objc2, tao, wry, tray-icon.
- The known platform traps get a test or a note where they live: hold-mode release, fast repeated toggles, double paste on KDE, overlay shutdown on Wayland.
