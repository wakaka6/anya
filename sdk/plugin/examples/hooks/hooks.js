import { appendFile } from "node:fs/promises";
import { defineAction, defineTransform } from "@wakaka6/anya-plugin";

// Text in, text out: collapses runs of whitespace and adds the `suffix`
// setting. The text "slow" waits five seconds (or until Anya stops waiting),
// "boom" throws; both are there to see how Anya shows a hook that fails.
export const tidy = defineTransform({
  async transform(text, { settings, signal }) {
    if (text === "slow") {
      await new Promise((resolve) => {
        const timer = setTimeout(resolve, 5000);
        signal.addEventListener("abort", () => (clearTimeout(timer), resolve()), { once: true });
      });
    }
    if (text === "boom") throw new Error("transform was told to fail");
    return text.replace(/\s+/g, " ").trim() + (settings.suffix ?? "");
  },
});

// Changes the world: appends the final text to the file the `file` setting
// names, one line per dictation. With no file it does nothing.
export const note = defineAction({
  async act(text, { settings }) {
    if (text === "boom") throw new Error("action was told to fail");
    if (settings.file) await appendFile(settings.file, text.replace(/\n/g, " ") + "\n");
  },
});
