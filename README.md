# Anya

[中文](README.md) · [English](README.en.md)

听见你说的话，交给任何想要它的程序。

Anya 是 Linux 和 macOS 上的本地语音转文字服务。小核心只负责录音和识别；听写、持续转写、上屏、浮层、识别引擎都插在它周围，别的程序订阅的也是同一套识别帧。

**状态：** 刚起步，还不能用。架构和规矩见 [ARCHITECTURE.md](ARCHITECTURE.md) 和 [docs/decisions](docs/decisions/)。

**在 Anya 上做应用**（会议纪要、实时字幕、翻译、会听的 agent……）：订阅协议、顺序承诺和测试用的样例流见 [docs/protocol.md](docs/protocol.md)。

## 开发

需要 Rust 1.95（`rust-toolchain.toml` 会自动选上）和 [cargo-deny](https://github.com/EmbarkStudios/cargo-deny)。

```bash
cargo xtask ci       # CI 跑的完整检查：fmt、clippy、测试（含分层规则）、cargo-deny
cargo xtask check    # 只查分层规则
```

## 更新日志

见 [CHANGELOG.md](CHANGELOG.md)。
