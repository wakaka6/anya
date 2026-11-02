// Spoken punctuation: "明天开会逗号记得带周报句号" becomes "明天开会，记得带周报。".
// Runs inline in QuickJS: one file, no imports.
//
// Engines punctuate on their own, so a spoken mark often arrives wrapped in
// the engine's punctuation ("开会，逗号，记得"): the mark replaces that too.
// Engines also mishear the words ("句好", "唤航"), so the context hook hands
// them to the engine as hotwords first.

// Punctuation and spaces an engine may put around a spoken mark; never a
// line break, which an earlier rule may have made.
const AROUND = "[ \\t\\u3000，,。.、；;：:！!？?]*";
// Around a line break only spaces and commas go: the sentence before keeps
// its own end mark.
const AROUND_BREAK = "[ \\t\\u3000，,]*";

const MARKS = [
  ["新段落|另起一段", "\n\n", AROUND_BREAK],
  ["换行", "\n", AROUND_BREAK],
  ["省略号|省略號", "……", AROUND],
  ["感叹号|感嘆號|叹号|嘆號", "！", AROUND],
  ["问号|問號", "？", AROUND],
  ["逗号|逗號", "，", AROUND],
  ["句号|句號", "。", AROUND],
  ["顿号|頓號", "、", AROUND],
  ["冒号|冒號", "：", AROUND],
  ["分号|分號", "；", AROUND],
  ["左括号|左括號", "（", AROUND],
  ["右括号|右括號", "）", AROUND],
  ["左引号|左引號", "“", AROUND],
  ["右引号|右引號", "”", AROUND],
];

const RULES = MARKS.map(([spoken, mark, around]) => [
  new RegExp(`${around}(?:${spoken})${around}`, "g"),
  mark,
]);

// The Simplified spelling of each spoken mark.
const SPOKEN = MARKS.flatMap(([spoken]) => spoken.split("|").filter((w) => !/[號嘆問]/.test(w)));

export function context() {
  return { hotwords: SPOKEN };
}

export function transform(text) {
  let out = text;
  for (const [pattern, mark] of RULES) out = out.replace(pattern, mark);
  return out;
}
