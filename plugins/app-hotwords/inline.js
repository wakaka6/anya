// Hotwords for the app in front: a dictation into the editor favours the
// project's names, one into the terminal its commands. Runs inline in
// QuickJS: one file, no imports.
//
// The `apps` setting holds groups separated by `;` or new lines, each an app
// id, a colon and its words separated by commas:
//   com.microsoft.VSCode: rquickjs, Tokio; com.apple.Terminal: kubectl, cargo

function parse(table) {
  const apps = new Map();
  for (const group of (table ?? "").split(/[;；\n]/)) {
    const colon = group.search(/[:：]/);
    if (colon < 0) continue;
    const app = group.slice(0, colon).trim().toLowerCase();
    const words = group
      .slice(colon + 1)
      .split(/[,，、]/)
      .map((w) => w.trim())
      .filter(Boolean);
    if (app && words.length) apps.set(app, [...(apps.get(app) ?? []), ...words]);
  }
  return apps;
}

let parsedFrom;
let parsed = new Map();

export function context(ctx) {
  if (!ctx.app) return;
  const table = ctx.settings?.apps ?? "";
  if (table !== parsedFrom) {
    parsed = parse(table);
    parsedFrom = table;
  }
  const words = parsed.get(ctx.app.toLowerCase());
  if (words) return { hotwords: words };
}
