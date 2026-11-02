// Inline hooks run in QuickJS inside Anya: plain functions, one file, no
// `import`, no network, no disk, a few milliseconds per call.

// Before the session opens: the terminal gets its vocabulary.
export function context(ctx) {
  if (ctx.app === "com.apple.Terminal" || ctx.app === "Alacritty") {
    return { hotwords: ["git", "cargo", "kubectl"] };
  }
}

// On the final text: trim it and start with a capital letter.
export function transform(text) {
  const trimmed = text.trim();
  return trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
}
