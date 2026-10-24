<div align="center">

<img src="assets/readme/banner.svg" alt="mhc" width="100%" />

**终端里的 AI 编程智能体：把 senpi 上的 oh-my-openagent 重写为一个原生 Rust 程序。**

[![Release](https://img.shields.io/github/v/release/Indosaram/mhc?style=flat-square)](https://github.com/Indosaram/mhc/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/Indosaram/mhc/ci.yml?branch=main&style=flat-square&label=ci)](https://github.com/Indosaram/mhc/actions/workflows/ci.yml)
[![License: MIT + SUL-1.0](https://img.shields.io/badge/license-MIT%20%2B%20SUL--1.0-blue?style=flat-square)](#许可证)

**简体中文** · [English](README.en.md)

<img src="assets/readme/demo.png" alt="mhc 在终端中修复 Rust 编译错误" width="85%" />

</div>

## 为什么用 mhc

- **无需安装运行时。** 下载即用，不依赖 Node、Bun 或 npm。
- **与 OmO on senpi 体验一致。** 逐文件移植上游 TypeScript 源码，并用上游生成的 golden fixtures 校验，界面、按键和行为保持一致。
- **直接沿用现有配置。** 与 OmO 读取同一套 `~/.omo` 配置、模型、认证和智能体设置。
- **功能开箱即全。** 子智能体、`ultrawork` / `ulw-loop` 工作流、长期记忆、LSP、ast-grep、MCP、网页搜索和内置技能都随发布包提供。
- **支持各家模型。** Anthropic、OpenAI、Gemini、DeepSeek、通义千问、Kimi、智谱 ZAI、MiniMax、OpenRouter、Ollama、Bedrock、Azure 等。
- **内存占用低得多。** 在同一个 `mass-ulw` 工作流（3 个并行子智能体 + 汇总）上，mhc 的匿名内存峰值为 83-86 MB，OmO 为 1356-1398 MB，约少 16 倍，耗时相同。测量方法、原始数据和复现步骤见 [docs/memory-benchmark.md](docs/memory-benchmark.md)（英文）。

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/Indosaram/mhc/main/install.sh | sh
```

支持 Linux x86_64、Linux aarch64、macOS Apple Silicon 和 macOS x86_64（Intel）。安装到 `~/.local/share/mhc`，并把 `mhc` 链接到 `~/.local/bin`；再次运行同一命令即可升级。指定版本：`MHC_VERSION=v2026.10.19`。

Windows x86_64（PowerShell），安装到 `%LOCALAPPDATA%\mhc` 并加入用户 PATH：

```powershell
irm https://raw.githubusercontent.com/Indosaram/mhc/main/install.ps1 | iex
```

## 更新与升级

mhc 支持原生自更新，自动下载完整自包含发布包并校验 SHA256，原子替换时完整保留用户配置与会话数据（`~/.omo/agent`）：

```bash
mhc update           # 检查并升级至最新发布版本
mhc update --force   # 忽略当前版本，强制重新下载并安装最新发布版本
mhc update --all     # 顺序更新已安装的资源包及 mhc 原生程序
```

- **会话安全与回滚**：更新检测到其他活跃会话时会自动拒绝替换以保护运行时状态；遇到写入异常时自动原子回滚至原有版本。
- **非阻塞启动提示**：交互终端启动时在后台静默查询缓存，检测到新版本时在提示框内告知，不阻塞界面加载与输入。

## 快速开始

```bash
export ANTHROPIC_API_KEY=...    # 或 OPENAI_API_KEY、GEMINI_API_KEY 等（也可在 mhc 内用 /login）
cd your-project
mhc
```

```bash
mhc "介绍一下这个仓库的结构"        # 带着提示词启动
mhc -p "列出 src/ 下所有 .rs 文件"   # 执行一次后退出
mhc --model sonnet:high            # 选择模型和思考强度
mhc --continue                     # 继续上一次会话
```

更多：[CLI 参考](docs/cli.md) · [配置](docs/configuration.md) · `mhc --help`

## 从源码构建

需要 Rust 1.95（由 `rust-toolchain.toml` 自动选择）、`protoc` 和 [Bun](https://bun.sh)。

```bash
git clone https://github.com/Indosaram/mhc.git && cd mhc
cargo build --release -p maho-cli
./target/release/mhc
```

如需构建与发布包相同的自包含目录（含技能和辅助程序），参见 `tools/package-native.mjs` 和 `.github/workflows/release.yml`。

## 项目状态与贡献

mhc 仍在积极开发中，与上游的一致性尚未全部完成。上游版本固定在 [PINS.md](PINS.md)，覆盖情况见 [docs/parity.md](docs/parity.md)，安全与隐私差异见 [docs/divergences.md](docs/divergences.md)。提交改动前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)，安全问题请通过 [SECURITY.md](SECURITY.md) 私下报告，其他问题请提交到 [issues](https://github.com/Indosaram/mhc/issues)。

## 安全

安全策略及漏洞报告流程见 [SECURITY.md](SECURITY.md)。

## 许可证

各部分沿用其所移植项目的许可证：

- `crates/maho-*`、`crates/builtins`、`crates/extensions`、`crates/vendor` 移植自 [senpi](https://github.com/code-yeongyu/senpi)，采用 **MIT**（[LICENSES/MIT.txt](LICENSES/MIT.txt)）。
- `crates/omo/**` 移植自 [oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent)，采用 **Sustainable Use License 1.0**（[LICENSES/SUL-1.0.md](LICENSES/SUL-1.0.md)）：允许免费的非商业使用、修改和再分发；禁止商业使用和付费分发。

本软件是上述两个项目的修改衍生作品（其 TypeScript 源码的 Rust 重新实现），此声明为 Sustainable Use License 所要求。
