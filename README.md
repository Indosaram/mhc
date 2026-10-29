<div align="center">

<img src="assets/readme/banner.svg" alt="mhc" width="100%" />

**终端里的 AI 编程智能体：把 senpi 上的 oh-my-openagent 重写为一个原生 Rust 程序。**

[![Release](https://img.shields.io/github/v/release/Indosaram/mhc?style=flat-square)](https://github.com/Indosaram/mhc/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/Indosaram/mhc/ci.yml?branch=main&style=flat-square&label=ci)](https://github.com/Indosaram/mhc/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](#许可证)

**简体中文** · [English](README.en.md)

<img src="assets/readme/demo.png" alt="mhc 在终端中修复 Rust 编译错误" width="85%" />

</div>

## 为什么用 mhc

- **原生核心程序。** 核心智能体运行为独立原生可执行文件，无需 Node、Bun 或 npm（代码执行 eval 工具需要 [Bun](https://bun.sh)，扩展包安装 `mhc install` 需要 `npm`）。
- **与 OmO on senpi 体验一致。** 逐文件移植上游 TypeScript 源码，并用上游生成的 golden fixtures 校验，界面、按键和行为保持一致。
- **直接沿用现有配置。** 读取同一套 `~/.omo` 配置、模型、认证和智能体设置文件。
- **功能开箱即全。** 子智能体、工作流、长期记忆、LSP、ast-grep、MCP、网页搜索和内置技能都随发布包提供。
- **支持各家模型。** Anthropic、OpenAI、Gemini、DeepSeek、通义千问、Kimi、智谱 ZAI、MiniMax、OpenRouter、Ollama、Bedrock、Azure 等。

## 隐私与安全默认设置

mhc 开箱即默认采用保护隐私与安全的保守策略。所有与上游有意的安全与隐私差异均记录在 [docs/divergences.md](docs/divergences.md)，漏洞报告流程见 [SECURITY.md](SECURITY.md)。

- **遥测默认关闭。** 除非明确开启，否则不会向外发送任何使用数据、提示词、路径或归因标头。开启后，仅发送不敏感的运行时指标（工具耗时、模型名、错误码；绝不发送文件内容或提示词）。如需开启，请在 `~/.omo/omo.jsonc` 中配置 `"telemetry": { "enabled": true }`，或在 `~/.omo/agent/settings.json` 中配置 `"enableInstallTelemetry": true`。
- **从不收集 GeoIP。** 即使开启遥测，也始终强制禁用 IP 地理位置解析（`$geoip_disable: true`）。
- **人物画像默认关闭。** 长期记忆会提取项目与主用户的事实，但第三方人物画像（`people/<slug>/`）默认关闭；如需启用，可在 `~/.omo/omo.jsonc` 中配置 `"memory": { "people": { "enabled": true } }`。
- **可选的 guarded 权限预设。** 默认预设与 OmO 相同，为 `full-access`。在 `settings.json` 中设置 `"permissionPreset": "guarded"`（或传 `--permission-preset guarded`）后，工具可自由访问项目目录与 mhc 自有目录（`~/.omo/agent`、会话、记忆）；在交互模式下，访问外部路径、凭据文件（`auth.json`）或系统临时目录根路径会提示用户确认；在非交互模式（`-p`）或子智能体任务中，外部访问会被拒绝并给出覆盖提示。
- **敏感环境变量清理。** 模型调用的 Shell 命令（bash 工具及 eval 内核）默认过滤敏感环境变量（`*_TOKEN`、`*_KEY`、`*_SECRET`、`*_AUTH`、`DATABASE_URL` 等）。可在 `settings.json` 中设置 `"bashEnvScrub": false` 关闭清理，或通过 `"bashEnvPassthrough": ["VAR_NAME"]` 白名单放行指定变量。
- **可选 bash 沙箱。** 支持在 Linux（`bwrap`）和 macOS（`sandbox-exec`）上为 Shell 命令启用操作系统级写隔离保护；在 `settings.json` 中配置 `"bashSandbox": "auto"` 或 `"bashSandbox": "required"`（默认 `"off"`）。
- **第一方仿冒提供商风险确认。** 仿冒第一方客户端身份的提供商（如使用 Claude 订阅 OAuth 令牌的 `anthropic`、`anthropic-subscription`、`chatgpt-subscription`、`github-copilot`、`kimi-coding`、`devin`、`cursor`）在记录明确风险确认前会被阻止请求。可在交互模式下通过 `/login <provider>` 确认，或使用 `mhc auth consent <provider>` 命令记录确认。
- **多账号轮换风险确认。** 遇速率限制时的多账号自动轮换默认关闭，仅使用主账号；需执行 `mhc auth consent <provider> --rotation` 显式确认后方可开启轮换。
- **会话历史保留配置。** 默认永久保留会话。可在 `settings.json` 中设置 `"sessionRetentionDays": N`（1–36500），启动时自动清理闲置超过 N 天的 `*.jsonl` 会话。*注意：* 由于 OmO 不会获取 mhc 的 `.jsonl.lock` 伴生锁，若启用了会话清理，仅在 OmO 中打开且闲置超过 N 天的会话可能会被清理。

### 从早期 mhc / OmO 升级

若您从 mhc 早期版本或 OmO 升级，并希望恢复原有的宽松或上游默认行为，可配置相应设置：

| 行为 | 之前 / OmO 默认值 | mhc 安全默认值 | 恢复之前行为的方法 |
| --- | --- | --- | --- |
| Shell 环境变量 | 继承进程全部环境变量 | 过滤包含密钥特征的环境变量 | `settings.json` 中设置 `"bashEnvScrub": false` 或指定 `"bashEnvPassthrough": ["NAME"]` |
| 使用遥测 | 默认开启 | 默认关闭（`off`） | `~/.omo/omo.jsonc` 中设置 `"telemetry": { "enabled": true }` 或 `settings.json` 中设置 `"enableInstallTelemetry": true` |
| GeoIP 地理位置 | 上游默认开启 | 强制永久关闭（`$geoip_disable: true`） | 不可开启（隐私保障） |
| 人物画像记忆 | 默认记录第三方画像 | 默认关闭（`people.enabled: false`） | `~/.omo/omo.jsonc` 中设置 `"memory": { "people": { "enabled": true } }` |
| 订阅提供商登录 | 允许直接使用 | 须先记录风险确认 | 执行 `mhc auth consent <provider> --yes` 或在终端中执行 `/login <provider>` |
| 多账号轮换 | 遇限速自动多账号轮换 | 默认关闭轮换，仅使用主账号 | 执行 `mhc auth consent <provider> --rotation --yes` |
| Bash 操作系统沙箱 | 无沙箱限制 | 默认关闭（`"off"`，支持按需开启） | 默认 `"bashSandbox": "off"` 与上游一致 |
| 会话保留清理 | 永久保留 | 永久保留（支持按需开启） | 默认（未设置 `sessionRetentionDays`）保留全部历史会话 |

## 安装

支持平台：**Linux x86_64**、**Linux aarch64 (ARM64)**、**macOS Apple Silicon (aarch64)**、**macOS x86_64 (Intel)** 与 **Windows x86_64**。

每个发布包均经过 [minisign](https://jedisct1.github.io/minisign/) 签名并附带 GitHub 构建溯源认证（build provenance）。安装脚本在检测到 PATH 中的 `minisign` 时会自动校验签名与内置公钥（`mhc-release.pub`）；亦可使用 `gh attestation verify` 验证发布包溯源。

```bash
curl -fsSL https://raw.githubusercontent.com/Indosaram/mhc/main/install.sh | sh
```

安装到 `~/.local/share/mhc`，并把 `mhc` 链接到 `~/.local/bin`；再次运行同一命令即可升级。指定版本：`MHC_VERSION=v2026.10.19`。

Windows x86_64（PowerShell），安装到 `%LOCALAPPDATA%\mhc` 并加入用户 PATH：

```powershell
irm https://raw.githubusercontent.com/Indosaram/mhc/main/install.ps1 | iex
```

## 更新与升级

mhc 支持原生自更新，自动下载完整自包含发布包，校验 SHA256 校验和及 minisign 签名，原子替换时完整保留用户配置与会话数据（`~/.omo/agent`）：

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

mhc 采用 **MIT** 许可证（[LICENSES/MIT.txt](LICENSES/MIT.txt)），移植自 [senpi](https://github.com/code-yeongyu/senpi)（MIT）。第三方组件沿用其各自的许可证，详见 [LICENSE](LICENSE)。
