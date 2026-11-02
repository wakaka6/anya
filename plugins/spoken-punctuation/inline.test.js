import { expect, test } from "bun:test";
import { context, transform } from "./inline.js";

test("spoken marks become marks", () => {
  expect(transform("明天开会逗号记得带周报句号")).toBe("明天开会，记得带周报。");
  expect(transform("你去吗问号太好了感叹号")).toBe("你去吗？太好了！");
  expect(transform("苹果顿号香蕉冒号都要分号谢谢省略号")).toBe("苹果、香蕉：都要；谢谢……");
  expect(transform("他说左引号好的右引号")).toBe("他说“好的”");
});

test("the engine's own punctuation around a spoken mark goes", () => {
  expect(transform("明天开会，逗号，记得带周报。句号。")).toBe("明天开会，记得带周报。");
  expect(transform("你好 问号 ")).toBe("你好？");
  expect(transform("逗號和句號")).toBe("，和。");
});

test("a line break keeps the sentence's end mark", () => {
  expect(transform("第一行。换行，第二行")).toBe("第一行。\n第二行");
  expect(transform("第一段。新段落第二段")).toBe("第一段。\n\n第二段");
  // As whisper writes it: the spoken end mark right before the break.
  expect(transform("记得带上周报句号换行我们用")).toBe("记得带上周报。\n我们用");
  expect(transform("好的, 问号 新段落 然后")).toBe("好的？\n\n然后");
});

test("text without spoken marks is left alone", () => {
  expect(transform("Hello, world. 你好。")).toBe("Hello, world. 你好。");
});

test("the engine is told the spoken marks, in Simplified spelling", () => {
  const { hotwords } = context({});
  expect(hotwords).toContain("逗号");
  expect(hotwords).toContain("句号");
  expect(hotwords).toContain("换行");
  expect(hotwords.some((w) => /[號嘆問]/.test(w))).toBe(false);
});
