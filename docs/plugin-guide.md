# 插件开发指南

[English](plugin-guide.en.md) · [插件参考](plugins.md)

这份指南带你从零写一个插件，在自己的 Anya 里装上、启用、试用，再发布到插件市场。全程大约 15 分钟。字段、预算、失败处理和索引格式的完整说明在[插件参考](plugins.md)里，这里只讲走通一遍需要的东西。

## 0. 先决定写哪一种

插件可以提供四样东西，一个包里可以有多样：

| 想做的事 | 提供 | 例子 |
| --- | --- | --- |
| 接入一家新的语音识别服务 | `engine` | 自建的 ASR、Deepgram |
| 听写开始前，按前台应用补热词或定语言 | `context` | 在编辑器里加上项目里的名字 |
| 粘贴前改写文字 | `transform` | 去掉语气词、口述标点、调用 LLM 润色 |
| 和粘贴同时，把文字送去别处 | `action` | 记进笔记、发到待办、写日志 |

再决定在哪里运行：

| | inline | host |
| --- | --- | --- |
| 运行在 | Anya 内部的 QuickJS | 插件自己的 Bun 进程 |
| 能提供 | `context`、`transform` | 全部四样 |
| 能联网、读写文件、用 npm 包 | 不能 | 能，以用户的权限运行 |
| 用户需要装 Bun | 不需要 | 需要（Anya 会代为下载） |
| 适合 | 毫秒级的纯文字处理 | 要联网、要磁盘、要做重活 |

**能用 inline 就用 inline。** 用户启用前看到的是“不能访问网络和磁盘”，更容易信任；也不用装 Bun。

你需要：[Bun](https://bun.sh) 1.2 或更新，用来跑测试；一份 Anya 的源码（`git clone https://github.com/wakaka6/anya.git`），用来校验和试装插件。下文用 `$ANYA` 指这份源码的路径。

## 1. 写第一个插件：去掉语气词

一个 inline 的 transform：粘贴前去掉“嗯”“呃”这类语气词，词表可以在设置里改。

```
filler-words/
  package.json
  inline.js
  inline.test.js
```

`package.json`：

```json
{
  "name": "filler-words",
  "version": "1.0.0",
  "description": "Drops filler words like 嗯 and 呃 from dictation.",
  "type": "module",
  "files": ["inline.js"],
  "anya": {
    "id": "filler-words",
    "name": { "en": "Filler words", "zh-CN": "去掉语气词" },
    "description": {
      "en": "Drops 嗯, 呃 and the other filler words you list before the text is pasted",
      "zh-CN": "粘贴前去掉“嗯”“呃”等你列出的语气词"
    },
    "inline": "./inline.js",
    "compat": ">=0.0.0",
    "protocol": 1,
    "permissions": [],
    "fields": [
      {
        "key": "words",
        "label": { "en": "Filler words", "zh-CN": "语气词" },
        "help": { "en": "Separated by commas; empty uses 嗯, 呃, 啊", "zh-CN": "用逗号分隔；留空用“嗯、呃、啊”" },
        "kind": "text"
      }
    ]
  }
}
```

几个要点：

- `anya.inline` 指向入口文件。插件提供什么，由入口导出了什么决定，不用另外声明。
- `anya.name` 和 `anya.description` 是插件列表里显示的文字，可以给每种界面语言各写一份。
- `anya.fields` 是插件的设置。Anya 会在设置窗口里替你画出表单，值存在配置文件的 `[plugins.settings.filler-words]` 下，调用时放在 `ctx.settings` 里。
- `files` 列出要打进包的文件，`package.json` 会自动带上。忘了列，入口就不会被打包。

`inline.js`：

```js
// Runs in QuickJS inside Anya: one file, no import, no network, no disk.
const DEFAULT = ["嗯", "呃", "啊"];

export function transform(text, ctx) {
  const listed = (ctx?.settings?.words ?? "")
    .split(/[,，]/)
    .map((w) => w.trim())
    .filter(Boolean);
  for (const word of listed.length ? listed : DEFAULT) {
    // The word and the comma the engine put after it.
    text = text.replaceAll(new RegExp(`${word}[，,]?\\s*`, "g"), "");
  }
  return text.trim();
}
```

inline 入口是一个 ES 模块，导出普通函数：`transform(text, ctx)` 返回新的文字，`context(ctx)` 返回 `{ hotwords?, language? }` 或什么都不返回。它不能 `import` 任何东西，每次调用默认只有 20 毫秒。

## 2. 测试

inline 插件就是普通函数，直接调用即可。`inline.test.js`：

```js
import { expect, test } from "bun:test";
import { transform } from "./inline.js";

test("filler words and their commas go", () => {
  expect(transform("嗯，明天三点开会，呃，记得带周报。", {})).toBe("明天三点开会，记得带周报。");
});

test("the words the user lists replace the defaults", () => {
  const ctx = { settings: { words: "那个, 就是说" } };
  expect(transform("那个，就是说我们改到周五", ctx)).toBe("我们改到周五");
});
```

```console
$ bun test
 2 pass
 0 fail
```

## 3. 装进自己的 Anya 试用

Anya 只从插件索引安装插件。试用时，在本机做一个只含你这个插件的索引，让 Anya 指向它。

**打包并校验。** 用的是插件市场发布时运行的同一个校验脚本，它在这里通过，发布时也会通过：

```bash
cd filler-words
npm pack                                        # 生成 filler-words-1.0.0.tgz
mkdir -p ../unpacked && tar xzf filler-words-1.0.0.tgz -C ../unpacked --strip-components=1
cd ..
bun $ANYA/scripts/validate-plugin.mjs unpacked filler-words/filler-words-1.0.0.tgz \
  "file://$PWD/filler-words/filler-words-1.0.0.tgz" index.json $ANYA/sdk/plugin/index.js
# ok: filler-words 1.0.0 transform sha512-…
```

脚本会检查包是否合规，加载入口看它导出了什么，再把这个版本写进 `index.json`。改了代码要重新试时，先删掉 `index.json` 并升一下版本号：同一个版本只能写进索引一次。

**让 Anya 读这个索引。** 在 `~/.config/anya/config.toml` 里加上：

```toml
[plugins]
index = "file:///绝对路径/index.json"
```

**安装、启用：**

```console
$ anya plugin install filler-words
已安装 filler-words 1.0.0，目前处于停用状态。
…
$ anya plugin enable filler-words
已启用 filler-words。重启 Anya 后加载。
```

重启 Anya，打开设置的“插件”页：插件显示“已加载”，“听写钩子”里多了它的 transform，展开能看到“语气词”这个设置。在任意输入框里说一句“嗯，明天开会”，粘贴出来的就是“明天开会”。

也可以在设置窗口的“插件”页里装：插件市场会列出索引里的插件。试完把 `[plugins] index` 删掉，Anya 就回到正式的插件市场。

## 4. 需要联网或读写文件：host 插件

把入口换成 `anya.host`，它就在自己的 Bun 进程里运行，可以用 Node/Bun 的全部 API。下面是一个 action：每次听写后，把时间、应用和字数记进一个文件。

```js
// host.js
import { appendFile } from "node:fs/promises";
import { defineAction } from "@wakaka6/anya-plugin";

export const action = defineAction({
  async act(text, { app, settings }) {
    const line = `${new Date().toISOString()}\t${app ?? "-"}\t${[...text].length}\n`;
    await appendFile(settings.file, line);
  },
});
```

`package.json` 里写 `"host": "./host.js"`，在 `fields` 里加一个 `file` 设置，并在 `permissions` 里如实声明 `"fs:write:<说明>"`：用户启用前会看到它。

和 inline 插件不同的地方：

- **SDK 由 Anya 提供。** `@wakaka6/anya-plugin` 在运行时由 Anya 的插件宿主注入，包里不用带。SDK 还没有发布到 npm；需要类型提示的话，引用 `$ANYA/sdk/plugin/index.d.ts`。
- **依赖要打进包里。** Anya 解包后直接运行，不会执行 `npm install`。用了 npm 包，就用 `bun build --target=bun` 打成一个文件。
- **别阻塞事件循环。** Anya 每秒检查一次插件进程，3 秒没有回应就结束并重启它。重活放进 `Worker`。
- **尊重 `signal`。** 每次调用都有时间上限，到点时 `ctx.signal` 会中止，把它传给 `fetch`。

**测试 host 插件。** 测试里也要有人提供 SDK。在插件目录放一个 `anya-sdk.js`，让 Bun 从 Anya 源码里加载它，和 Anya 运行时的做法一样：

```js
// anya-sdk.js
import { plugin } from "bun";
import * as sdk from "/绝对路径/anya/sdk/plugin/index.js";

plugin({
  name: "anya-sdk",
  setup: (build) => build.module("@wakaka6/anya-plugin", () => ({ exports: { ...sdk }, loader: "object" })),
});
```

```toml
# bunfig.toml
[test]
preload = ["./anya-sdk.js"]
```

然后用 SDK 的 `callAction`、`callTransform`、`callContext` 按 Anya 的方式调用钩子，它们也会检查返回值是否合规：

```js
import { expect, test } from "bun:test";
import { readFile, rm } from "node:fs/promises";
import { callAction } from "@wakaka6/anya-plugin";
import { action } from "./host.js";

test("each dictation adds a line with its length", async () => {
  const file = `${import.meta.dir}/log.tsv`;
  await rm(file, { force: true });
  await callAction(action, "明天开会", { app: "com.apple.Notes", settings: { file } });
  expect((await readFile(file, "utf8")).trim().split("\t").slice(1)).toEqual(["com.apple.Notes", "4"]);
});
```

`anya-sdk.js` 和 `bunfig.toml` 只用于测试，不要列进 `files`。

## 5. 识别引擎

引擎插件用 `defineEngine`：在 `manifest` 里写标题、是否流式和设置项，在 `open` 里连上你的服务，收到 `audio` 就发出去，有结果就调用 `session.result(...)`。照着 [`sdk/plugin/examples/echo-engine`](../sdk/plugin/examples/echo-engine/) 改最快，会话事件、结果和错误的约定见[插件参考的“Engines”一节](plugins.md#engines)。用 SDK 的 `createSession`、`start`、`deliver` 可以不经过 Anya 测试一个引擎。

## 6. 发布到插件市场

1. Fork [wakaka6/anya](https://github.com/wakaka6/anya)，把插件放到 `plugins/<插件 id>/`（连同测试），提一个 PR。
2. 维护者会审代码和声明的权限。权限写得越具体越好，比如 `network:api.example.com` 而不是笼统的“联网”。
3. 合并后，维护者打一个 `<插件 id>-v<版本>` 的标签（例如 `filler-words-v1.0.0`）。CI 打包、运行你在第 3 步用过的校验脚本，把包、图标和索引条目提交到 `registry` 分支。用户打开插件市场就能看到它。

已发布的版本不能修改；要修正，发一个新版本。

## 发布前检查

- [ ] `anya.id` 只含小写字母、数字和 `-`；引擎插件的 `manifest.id` 和它相同。
- [ ] `files` 列出了入口和图标。
- [ ] `anya.compat` 写的是你测试过的 Anya 版本范围，不能是 `*`。
- [ ] `permissions` 如实列出要访问的主机、环境变量和路径。
- [ ] 必填的设置标了 `"required": true`：没填时 Anya 会跳过插件并告诉用户缺什么，而不是每次听写都报错。
- [ ] 错误信息对用户有用，不含密钥；日志里不写听写内容和凭据。
- [ ] 可以加一个图标：`anya.icon` 指向包内的 PNG 或 SVG（不能是 URL），正方形，最大 1 MiB。
- [ ] 第 3 步的校验脚本通过，`bun test` 通过。

## 常见问题

**`anya plugin install` 说找不到插件。** 确认 `[plugins] index` 是 `file://` 加绝对路径，并且 `index.json` 里有这个 id。

**安装时说这个版本的 Anya 用不了。** `anya.compat` 不包含你正在用的 Anya 版本。从源码构建的 Anya 版本号是 `0.0.0`，试用时写 `>=0.0.0`。

**启用后没有效果。** 命令行里的启用要在 Anya 重启后生效；设置窗口里的启用立即生效。看看“插件”页上它的状态：缺设置、加载失败的原因都会显示在那里。插件的 `console.log` 记在调试日志里：用 `anya --daemon --verbose` 启动，它会出现在终端里。

**transform 被跳过了。** 超时、抛错、返回的不是字符串，Anya 都会跳过这一步，用改写前的文字继续，并在“活动”页写明是哪个插件。inline 默认 20 毫秒，host 默认 2 秒，需要更多时间可以在 `anya.limits` 里申请，范围见[预算](plugins.md#budgets)。

**inline 入口校验失败，说包含 `import`。** inline 入口必须是一个不加载任何东西的文件。需要依赖的话，改用 host，或者先打成一个文件。

## 更多例子

- [`plugins/`](../plugins/)：官方插件和它们的测试。`spoken-punctuation` 同时有 context 和 transform，`app-hotwords` 用前台应用和设置，`daily-notes` 是一个写文件的 host action。
- [`sdk/plugin/examples/`](../sdk/plugin/examples/)：`inline`、`hooks`（host 的 transform 和 action）、`echo-engine`（流式引擎）。
