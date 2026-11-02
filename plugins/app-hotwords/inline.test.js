import { expect, test } from "bun:test";
import { context } from "./inline.js";

const apps = "com.microsoft.VSCode: rquickjs, Tokio；com.apple.Terminal：kubectl，cargo\nAlacritty: git; com.apple.Terminal: brew";

test("the app in front gets its words", () => {
  expect(context({ app: "com.microsoft.VSCode", settings: { apps } })).toEqual({ hotwords: ["rquickjs", "Tokio"] });
  expect(context({ app: "com.apple.terminal", settings: { apps } })).toEqual({
    hotwords: ["kubectl", "cargo", "brew"],
  });
  expect(context({ app: "Alacritty", settings: { apps } })).toEqual({ hotwords: ["git"] });
});

test("an app not listed, no app, or no table adds nothing", () => {
  expect(context({ app: "com.apple.Safari", settings: { apps } })).toBe(undefined);
  expect(context({ app: undefined, settings: { apps } })).toBe(undefined);
  expect(context({ app: "Alacritty", settings: {} })).toBe(undefined);
});

test("a changed table is read again", () => {
  expect(context({ app: "Alacritty", settings: { apps: "Alacritty: nvim" } })).toEqual({ hotwords: ["nvim"] });
  expect(context({ app: "Alacritty", settings: { apps: "Alacritty: tmux" } })).toEqual({ hotwords: ["tmux"] });
});
