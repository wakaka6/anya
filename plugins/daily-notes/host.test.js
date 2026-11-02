import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { callAction } from "@wakaka6/anya-plugin";
import { note } from "./host.js";

let dir;
afterEach(() => dir && rmSync(dir, { recursive: true, force: true }));

function today() {
  const d = new Date();
  const two = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}`;
}

test("each dictation is a line in today's note, the first one under a heading", async () => {
  dir = mkdtempSync(path.join(tmpdir(), "anya-notes-"));
  const folder = path.join(dir, "notes");
  await callAction(note, "明天下午三点开会。", { settings: { folder }, app: "com.apple.Notes" });
  await callAction(note, "第一行\n第二行", { settings: { folder } });
  const lines = readFileSync(path.join(folder, `${today()}.md`), "utf8").split("\n");
  expect(lines[0]).toBe(`# ${today()}`);
  expect(lines[1]).toBe("");
  expect(lines[2]).toMatch(/^- \d\d:\d\d 明天下午三点开会。 `com\.apple\.Notes`$/);
  expect(lines[3]).toMatch(/^- \d\d:\d\d 第一行 第二行$/);
  expect(lines.length).toBe(5);
});

test("empty text writes nothing; no folder is an error that says what to set", async () => {
  dir = mkdtempSync(path.join(tmpdir(), "anya-notes-"));
  await callAction(note, "  \n ", { settings: { folder: dir } });
  expect(() => readFileSync(path.join(dir, `${today()}.md`))).toThrow();
  await expect(callAction(note, "hi", { settings: {} })).rejects.toThrow("notes folder");
});
