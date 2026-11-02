import { appendFile, mkdir, stat } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { defineAction } from "@wakaka6/anya-plugin";

// Keeps every dictation in a Markdown note per day: `<folder>/2026-10-31.md`,
// one `- 14:02 text` line each, the app it went to after it.

export const note = defineAction({
  async act(text, { app, settings }) {
    const line = text.replace(/\s*\n\s*/g, " ").trim();
    if (!line) return;
    const folder = expandHome((settings.folder ?? "").trim());
    if (!folder) throw new Error("choose the notes folder in the plugin's settings");
    const now = new Date();
    const day = `${now.getFullYear()}-${two(now.getMonth() + 1)}-${two(now.getDate())}`;
    const file = path.join(folder, `${day}.md`);
    await mkdir(folder, { recursive: true });
    const fresh = await stat(file).then(
      () => false,
      () => true,
    );
    const head = fresh ? `# ${day}\n\n` : "";
    const where = app ? ` \`${app}\`` : "";
    await appendFile(file, `${head}- ${two(now.getHours())}:${two(now.getMinutes())} ${line}${where}\n`);
  },
});

function two(n) {
  return String(n).padStart(2, "0");
}

function expandHome(p) {
  if (p === "~") return homedir();
  return p.startsWith("~/") ? path.join(homedir(), p.slice(2)) : p;
}
