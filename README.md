<div align="center">

<img src="assets/readme/banner.svg" alt="mhc" width="100%" />

# mhc

**在终端里写代码的 AI 编程智能体，用 Rust 原生实现。**

与 Claude Code、Codex CLI、OpenCode 属于同一类工具：在项目目录里运行 `mhc`，
它会阅读代码、执行命令、修改文件，帮你完成开发任务。

[![License: MIT + SUL-1.0](https://img.shields.io/badge/license-MIT%20%2B%20SUL--1.0-blue?style=flat-square)](#-许可证)
[![Rust 1.95](https://img.shields.io/badge/rust-1.95-orange?style=flat-square&logo=rust)](rust-toolchain.toml)
[![Status: WIP](https://img.shields.io/badge/status-WIP-yellow?style=flat-square)](#-项目状态)

**简体中文** · [English](README.en.md)

<img src="assets/readme/demo.png" alt="mhc 在终端中定位并修复 Rust 编译错误" width="90%" />

<sub>真实运行截图：一句话让 mhc 找到 bug、修改代码、执行 <code>cargo run</code> 验证。</sub>

</div>

---

## 💡 这是什么？

mhc 是 [senpi](https://github.com/code-yeongyu/senpi)（终端 AI 编程智能体）及
[oh-my-openagent (OmO)](https://github.com/code-yeongyu/oh-my-openagent)（在其之上的增强插件集）的
**Rust 原生移植版**。

一句话：**OmO + senpi 的全部体验，变成一个不依赖 Node/Bun 运行时的单一原生程序。**

- **行为一致** —— 逐文件移植上游 TypeScript 源码，并用从上游生成的 golden fixtures 验证输出完全一致。界面、按键、输出与 OmO on senpi 相同。
- **原生、自包含** —— 一个 `mhc` 可执行文件加同目录的资源（内置技能、MCP 服务），复制即可使用。
- **扩展即 crate** —— 所有扩展都是静态注册的 Rust crate，没有 TypeScript 桥接层。
- **无缝迁移** —— 已有 OmO 配置可用 `mhc import-omo` 一键导入。

## 🆚 与 TypeScript 版 OmO 的区别

| | OmO on senpi（TypeScript） | **mhc（Rust）** |
| --- | --- | --- |
| 运行时 | 需要 Bun / Node | **无需任何运行时** |
| 分发 | npm 包 + 依赖 | **一个可执行文件 + 资源目录** |
| 扩展 | 运行时加载 TypeScript | **编译期静态注册的 Rust crate** |
| 界面与行为 | — | **与上游一致**（golden fixtures 校验） |
| 已有配置 | — | `mhc import-omo` 直接迁移 |

## ✨ 功能

| 类别 | 内容 |
| --- | --- |
| 交互方式 | 全功能终端 UI（TUI）、单次执行模式 `-p`、JSON / RPC 模式、Codex app-server 协议服务 |
| 模型提供商 | Anthropic、OpenAI、Google Gemini、DeepSeek、通义千问、Kimi / 月之暗面、智谱 ZAI、MiniMax、小米 MiMo、OpenRouter、Ollama、Amazon Bedrock、Azure OpenAI 等 |
| 内置工具 | `read` / `bash` / `edit` / `write` / `grep` / `find` / `ls`，以及网页搜索、网页抓取、图像生成、LSP、ast-grep、MCP |
| 会话 | 保存、继续（`-c`）、恢复（`-r`）、分叉（`--fork`）、导出为 HTML（`--export`） |
| OmO 能力 | `ultrawork`、`ulw-loop`、`mass-ulw` 工作流，子智能体（task），长期记忆，自动注释检查，内置技能（git-master、debugging、frontend 等） |
| 定制 | 技能、提示词模板、主题、`AGENTS.md` / `CLAUDE.md` 项目指令、Hooks、权限系统 |

## 🚀 安装

目前没有预编译版本，需要从源码构建。依赖：Rust 1.95（由 `rust-toolchain.toml` 自动选择）、[Bun](https://bun.sh)，以及一份
oh-my-openagent 源码（版本见 [PINS.md](PINS.md)，用于打包内置技能）。

```bash
git clone https://github.com/Indosaram/mhc.git
cd mhc

# 1. 构建所有可执行文件（mhc 与 ast-grep-mcp）
cargo build --workspace --bins --release

# 2. 打包成一个自包含目录（OMO_SRC 指向 oh-my-openagent 源码）
OMO_SRC=/path/to/oh-my-openagent \
  bun tools/package-native.mjs --binary target/release/mhc --output dist/mhc

# 3. 校验打包结果
bun tools/package-native.mjs --verify-only dist/mhc
```

完成后把 `dist/mhc` 加入 `PATH` 即可。

<details>
<summary>打包细节</summary>

- 脚本会复制 `mhc` 和同目录的 `ast-grep-mcp`，把内置技能放到 `skills/`、许可证放到 `licenses/`，
  并写入 `manifest.json`，记录每个文件的 sha256、大小、权限以及构建所用的版本与源码 pin。
- `--omo-root` 指定 oh-my-openagent 源码（默认 `$OMO_SRC`）；`--ast-grep-mcp` 覆盖辅助程序路径；
  `--force` 覆盖已有的非空目录。
- 运行时按可执行文件所在目录查找资源：技能在 `<dir>/skills`（可用 `OMO_SENPI_SKILLS_ROOT` 覆盖），
  ast-grep MCP 服务在 `<dir>/ast-grep-mcp`。缺少技能目录会给出警告，缺少辅助程序则跳过 ast-grep 服务，
  所以未通过 `--verify-only` 的目录不算完整安装。
- `bun tools/package-native.mjs --self-test` 用测试数据检查打包与校验逻辑。

</details>

## ⚡ 快速开始

```bash
# 设置任一提供商的 API Key
export ANTHROPIC_API_KEY=...        # 或 OPENAI_API_KEY、DEEPSEEK_API_KEY、GEMINI_API_KEY ...

cd /path/to/your-project
mhc                                  # 进入交互界面
```

常用示例：

```bash
mhc "解释一下这个仓库的结构"                      # 带初始提示启动
mhc -p "列出 src/ 下所有的 .rs 文件"              # 执行一次后退出
mhc @screenshot.png "这个报错是什么原因？"         # 附带文件
mhc --model sonnet:high                            # 指定模型与思考强度
mhc --continue                                     # 继续上一次会话
mhc --tools read,grep,find,ls -p "审查 src/ 的代码" # 只读模式
mhc --help                                         # 查看全部选项
```

配置目录为 `~/.omo/agent`（可用 `MAHO_CODING_AGENT_DIR` 修改）。已在使用 OmO 的话：

```bash
mhc import-omo        # 从 ~/.omo/agent 导入 settings、models、keybindings、auth 与会话
```

## 🗂 项目结构

| 路径 | 内容 | 上游 |
| --- | --- | --- |
| `crates/maho-cli` | `mhc` 命令行入口 | senpi |
| `crates/maho-core`、`maho-agent`、`maho-ai` | 智能体循环、模型接入、会话与配置 | senpi |
| `crates/maho-tui`、`maho-interactive` | 终端 UI 组件与差分渲染器 | senpi |
| `crates/maho-rpc`、`maho-server`、`maho-codemode` | RPC / app-server / CodeMode | senpi |
| `crates/builtins/*` | 内置扩展（42 个） | senpi |
| `crates/extensions/*` | 用户扩展的原生移植（14 个） | 见 [PINS.md](PINS.md) |
| `crates/omo/**` | OmO 功能（工作流、记忆、LSP、子智能体、桌面操作等） | oh-my-openagent |
| `tools/` | golden fixture 生成、parity 审计、打包脚本 | — |

## 🚧 项目状态

项目仍在积极开发中，与上游的对齐工作尚未全部完成。上游源码版本固定在 [PINS.md](PINS.md)，
各 crate 的移植对照记录在 `parity.d/` 中。

## 🤝 参与贡献

请先阅读 [AGENTS.md](AGENTS.md)。核心规则：

- 以 senpi 为唯一上游，逐文件移植，保持命名与顺序，行为必须与固定版本完全一致。
- golden fixture 只能通过 `tools/golden` 从上游生成，不能手写。
- 测试：`cargo nextest run -p <crate>`（0 skipped）与 `cargo clippy -p <crate> --all-targets -- -D warnings`。

## ⭐ 支持项目

如果 mhc 对你有帮助，欢迎点一个 Star，这是对项目最直接的支持。
遇到问题或有想法，请提交 [Issue](https://github.com/Indosaram/mhc/issues)。

## 📄 许可证

本仓库为**混合许可**：每一部分沿用其所移植的上游项目的许可证。

- `crates/maho-*`、`crates/builtins`、`crates/extensions`、`crates/vendor` 移植自
  [senpi](https://github.com/code-yeongyu/senpi)，采用 **MIT** 许可，见 [LICENSES/MIT.txt](LICENSES/MIT.txt)。
- `crates/omo/**` 移植自 [oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent)，
  采用 **Sustainable Use License 1.0**，见 [LICENSES/SUL-1.0.md](LICENSES/SUL-1.0.md)。
  允许免费的非商业使用、修改与再分发；不允许商业使用或付费分发。

本软件是上述两个上游项目的修改衍生作品（其 TypeScript 源码的 Rust 重新实现），此声明为
Sustainable Use License 所要求。
