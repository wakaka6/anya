# Anya

[中文](README.md) · [English](README.en.md)

听见你说的话，交给任何想要它的程序。

Anya 是 Linux 和 macOS 上的本地语音转文字服务。小核心只负责录音和识别；听写、持续转写、上屏、浮层、识别引擎都插在它周围，别的程序订阅的也是同一套识别帧。

**状态：** 刚起步，还不能用。架构和规矩见 [ARCHITECTURE.md](ARCHITECTURE.md) 和 [docs/decisions](docs/decisions/)。

**在 Anya 上做应用**（会议纪要、实时字幕、翻译、会听的 agent……）：订阅协议、顺序承诺和测试用的样例流见 [docs/protocol.md](docs/protocol.md)。

## 局域网共享与配对

默认只有这台电脑能连 Anya（回环地址）。想让手机、浏览器或别的电脑把自己的麦克风送来识别，在设置的“应用接入”里打开“局域网共享”（配置 `[subscription] lan = true`，端口 `lan_port`，默认 7591）：

- 局域网上走 TLS（`wss://`），证书由 Anya 自己生成并保存；应用靠配对时看到的证书指纹信任它。
- 点“添加应用”，设置里会出现六位配对码和二维码（内容是地址、配对码、证书指纹）。应用扫码或输入配对码，换到一个令牌；配对码两分钟内有效、只能用一次。
- 每个应用单独授权：听写、接收持续转写、查看他人的听写。随时可以改权限或移除，移除后它的连接立刻断开。
- 应用的听写结果只回给它自己，不会输入到这台电脑上。引擎支持流式时应用能收到中间结果，否则只收到最终结果。

协议细节见 [docs/protocol.md](docs/protocol.md)。

## 插件

识别引擎可以做成插件，由同一个 Bun 进程运行（要求 Bun 1.2 或更新；第一次安装插件时找不到会自动下载固定版本）。插件从 anya-plugins 仓库安装，下载内容会校验完整性哈希。

```bash
anya plugin search [关键字]      # 浏览仓库
anya plugin install <id>[@版本]  # 安装，安装后处于停用状态
anya plugin enable <id>          # 启用；Anya 启动时加载
anya plugin disable <id>
anya plugin remove <id>
anya plugin list
anya plugin builtin             # 内置引擎及其状态
anya plugin builtin off <id>    # 关闭内置引擎（on 重新开启）
```

安装和启用是两步，设置窗口的“引擎”页里也一样。启用后的引擎插件会收到凭它的引擎开的每一次识别的音频（局域网应用的听写也算），并以你的权限运行，请只启用你信得过的插件。命令行改动在 Anya 下次启动时生效；设置窗口里的启用、停用和移除立即生效。开发插件见 [docs/plugins.md](docs/plugins.md)。

内置引擎也可以关闭，当前选用的引擎除外；这类改动写入配置文件的 `[plugins.builtin]`，运行中的 Anya 会读取并立即生效。

## 开发

需要 Rust 1.95（`rust-toolchain.toml` 会自动选上）和 [cargo-deny](https://github.com/EmbarkStudios/cargo-deny)。

```bash
cargo xtask ci       # CI 跑的完整检查：fmt、clippy、测试（含分层规则）、cargo-deny
cargo xtask check    # 只查分层规则
```

## 更新日志

见 [CHANGELOG.md](CHANGELOG.md)。
