<div align="center">

<img src="assets/readme/banner.svg" alt="mhc" width="100%" />

**AI coding agent for your terminal: oh-my-openagent on senpi, rebuilt as one native Rust program.**

[![Release](https://img.shields.io/github/v/release/Indosaram/mhc?style=flat-square)](https://github.com/Indosaram/mhc/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/Indosaram/mhc/ci.yml?branch=main&style=flat-square&label=ci)](https://github.com/Indosaram/mhc/actions/workflows/ci.yml)
[![License: MIT + SUL-1.0](https://img.shields.io/badge/license-MIT%20%2B%20SUL--1.0-blue?style=flat-square)](#license)

[简体中文](README.md) · **English**

<img src="assets/readme/demo.png" alt="mhc fixing a Rust compile error in the terminal" width="85%" />

</div>

## Why mhc

- **No runtime to install.** One download with no Node, Bun or npm behind it.
- **Same experience as OmO on senpi.** The TypeScript sources are ported file by file and checked against upstream golden fixtures, so the UI, keys and behavior match.
- **Your existing setup just works.** mhc reads the same `~/.omo` config, models, auth and agents as OmO.
- **Everything included.** Subagents, `ultrawork` / `ulw-loop` workflows, long-term memory, LSP, ast-grep, MCP, web search and builtin skills ship in the release.
- **Any model.** Anthropic, OpenAI, Gemini, DeepSeek, Qwen, Kimi, ZAI, MiniMax, OpenRouter, Ollama, Bedrock, Azure and more.
- **A fraction of the memory.** On the same `mass-ulw` workflow (3 parallel child agents + join), mhc peaked at 83-86 MB of anonymous memory and OmO at 1356-1398 MB, about 16x less, in the same wall time. Method, raw numbers and how to rerun it: [docs/memory-benchmark.md](docs/memory-benchmark.md).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Indosaram/mhc/main/install.sh | sh
```

Linux x86_64 and macOS Apple Silicon. Installs to `~/.local/share/mhc` with `mhc` linked into `~/.local/bin`; run the same command again to upgrade. Pin a version with `MHC_VERSION=v2026.10.19`.

Windows x86_64 (PowerShell), installs to `%LOCALAPPDATA%\mhc` and adds it to your user PATH:

```powershell
irm https://raw.githubusercontent.com/Indosaram/mhc/main/install.ps1 | iex
```

## Updating

mhc includes built-in native self-update that downloads the complete release package with SHA256 checksum verification, atomically replacing the installation while preserving user state (`~/.omo/agent`):

```bash
mhc update           # Check and upgrade to the latest release
mhc update --force   # Bypass version checks and force re-install latest release
mhc update --all     # Update both installed resource packages and native mhc
```

- **Session Safety & Rollback**: Updates refuse replacement if another active mhc session is running from the installation; automatic atomic rollback restores the previous version if replacement fails.
- **Startup Notification**: Non-blocking background checks inform you when an update is available without delaying terminal startup.

## Quick start

```bash
export ANTHROPIC_API_KEY=...    # or OPENAI_API_KEY, GEMINI_API_KEY, ... (or /login inside mhc)
cd your-project
mhc
```

```bash
mhc "Explain how this repository is organized"   # start with a prompt
mhc -p "List all .rs files in src/"              # run once and exit
mhc --model sonnet:high                          # pick a model and thinking level
mhc --continue                                   # continue the last session
```

More: [CLI reference](docs/cli.md) · [Configuration](docs/configuration.md) · `mhc --help`

## Build from source

Needs Rust 1.95 (picked up from `rust-toolchain.toml`), `protoc` and [Bun](https://bun.sh).

```bash
git clone https://github.com/Indosaram/mhc.git && cd mhc
cargo build --release -p maho-cli
./target/release/mhc
```

To build the full self-contained directory the release ships (skills and helper binaries), see `tools/package-native.mjs` and `.github/workflows/release.yml`.

## Status and contributing

mhc is under active development and parity with upstream is not complete yet. Upstream revisions are pinned in [PINS.md](PINS.md); see [docs/parity.md](docs/parity.md) for coverage and [docs/divergences.md](docs/divergences.md) for intentional safety/privacy divergences. Read [CONTRIBUTING.md](CONTRIBUTING.md) before sending changes, report security vulnerabilities via [SECURITY.md](SECURITY.md), and report problems in [issues](https://github.com/Indosaram/mhc/issues).

## Security

For security vulnerability reporting and policy, see [SECURITY.md](SECURITY.md).

## License

Each part keeps the license of the project it ports:

- `crates/maho-*`, `crates/builtins`, `crates/extensions`, `crates/vendor` port [senpi](https://github.com/code-yeongyu/senpi) under **MIT** ([LICENSES/MIT.txt](LICENSES/MIT.txt)).
- `crates/omo/**` ports [oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent) under the **Sustainable Use License 1.0** ([LICENSES/SUL-1.0.md](LICENSES/SUL-1.0.md)): free for non-commercial use, modification and redistribution; no commercial use or paid distribution.

mhc is a modified derivative of both projects (a Rust reimplementation of their TypeScript sources), as the Sustainable Use License requires us to state.
