# AGENTS.md

**Generated:** 2026-10-24
**Commit:** afbe9b8b
**Branch:** main

## OVERVIEW
Rust native port of senpi + oh-my-openagent (maho). File-for-file translation preserving senpi ANSI terminal component model, differential renderer, and agent runtime. Config is shared with omo on disk (`~/.omo/agent`, `~/.omo/omo.jsonc`); env vars keep the `MAHO_` prefix (`crates/maho-core/src/brand.rs` `standalone_brand_profile`). User docs: `README.md` (zh), `README.en.md`, `docs/` (cli, configuration, parity), `CONTRIBUTING.md`.

## STRUCTURE
```
crates/
├── maho-cli/            # mhc binary: CLI routing, subcommands (install/auth/doctor/host/import-omo), migrations
├── maho-core/           # senpi coding-agent core: sessions, settings, auth, model runtime, brand/config paths
├── maho-agent/          # Core agent loop, execution harness, state machine
├── maho-ai/             # Multi-provider LLM abstraction layer & model clients (build.rs needs protoc)
├── maho-chord/          # Native port of @earendil-works/chord: copy-on-write state revisions and deltas
├── maho-tools/          # Builtin tools: read/bash/edit/write/grep/find/ls
├── maho-tool-child/     # Out-of-process tool child process, framed IPC channel, platform job policy
├── maho-tui/            # Keyboard input, fuzzy search, terminal mux primitives
├── maho-interactive/    # Terminal UI, streaming reveal pacing, theme engine
├── maho-rpc/            # Headless JSONL RPC protocol and print mode
├── maho-server/         # Daemon RPC server, SQLite storage, protocol frames
├── maho-codemode/       # eval kernel (bun/js only) and the code-mode tool host
├── maho-process-env/    # Safe process-environment updates at CLI startup (edition 2021)
├── maho-ext-api/        # Static extension contracts
├── maho-ext-host/       # Extension host runtime
├── maho-test-support/   # Shared test helpers (faux provider sessions)
├── builtins/            # 43 builtin extensions (maho-ext-*)
├── extensions/          # 14 native ports of user extensions (see PINS.md)
├── vendor/              # maho-grep, maho-pty
├── desktop/             # senpi-desktop-*: computer-use engine and platform backends (MIT)
└── omo/                 # oh-my-openagent port (SUL-1.0)
    ├── maho-omo/        # omo composition root: registers the components
    ├── components/      # 33 omo feature components (maho-omo-*: task, memory, lsp, ultrawork, ...)
    ├── senpi-task/      # Multi-agent DAG workflow runner, supervisor, category dispatch
    ├── memory-core/     # Git-backed persistent memory filesystem, identity profiles
    ├── omo-config-core/ # omo.json(c) schema, loader, migrations
    └── ...              # *-core libraries, boulder-state, get-worker, utils; sibling binaries lsp-daemon, ast-grep-mcp, git-bash-mcp
tools/                   # parity audit, golden fixtures, packaging, QA drivers (bun)
assets/                  # omo-latest skill overlay, omo.schema.json, readme images
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| CLI commands & flags | `crates/maho-cli/` | `src/main.rs` dispatch, `src/cli/help.txt` |
| Config paths, settings, auth | `crates/maho-core/` | `config.rs`, `brand.rs`, `settings_manager.rs`, `auth_storage.rs` |
| omo config (agents/categories) | `crates/omo/omo-config-core/` | `loader/paths.rs` resolves `~/.omo/omo.jsonc` + project `.omo/` |
| Agent loop & harness | `crates/maho-agent/` | Event stream parsing, tool execution |
| Provider integrations | `crates/maho-ai/` | Anthropic, OpenAI, Bedrock, Gemini |
| Terminal presentation | `crates/maho-interactive/` | ANSI differential renderer, streaming widgets |
| Background tasks & DAG | `crates/omo/senpi-task/` | Task supervisor, dependency scheduling |
| Memory & identity | `crates/omo/memory-core/` | Markdown block store, git auto-commit |
| Parity ledgers & goldens | `crates/*/parity.d/`, `tools/` | `docs/parity.md`, `tools/PARITY-FORMAT.md` |
| Packaging | `tools/package-native.mjs` | `--skill-source latest` uses `assets/omo-latest` |

## PORT RULES & CONVENTIONS
- senpi is upstream. Port file-for-file: one TS file maps to one Rust module, keeping names and order.
- Rendering uses senpi's string/ANSI component model and differential renderer (crossterm for raw mode/IO; no ratatui).
- Workspace lints set `unsafe_code = "forbid"`; a crate that needs unsafe (Win32/FFI) sets `unsafe_code = "deny"` and allows it locally with a SAFETY comment.
- Native Rust port must not delegate core agent behavior to external JS `omo` CLI.
- safety/privacy divergences: keep them, see docs/divergences.md.

## ANTI-PATTERNS (THIS PROJECT)
- Do not introduce ratatui or codex-rs dependencies.
- No TypeScript extension bridge; extensions are statically registered native crates.
- No intermediate per-unit/batch builds during implementation; compile/test only on final merged work.
- Never use fixed sleeps or polling delays in tests; await exact events with bounded timeouts.

## COMMANDS
```bash
cargo check --workspace
cargo nextest run -p <crate>
cargo clippy -p <crate> --all-targets -- -D warnings
```

## REMOTE BUILD ON GITHUB ACTIONS
The repo is public, so GitHub-hosted runners are unmetered. `.github/workflows/ci.yml` runs `cargo clippy --workspace --all-targets -D warnings` on Linux, macOS and Windows, builds `maho-cli`, and runs the whole nextest suite in 6 Linux crate shards and 3 Windows crate shards in parallel (`scripts/ci-shard.py` puts every workspace member in exactly one shard, balanced by test-file count), each on a fresh VM. Nothing is shared with other sessions.

```bash
scripts/gh-verify.sh             # push HEAD + local changes to verify/<user>-<ts>, wait, print per-job result
scripts/gh-verify.sh --keep      # keep the verify/* branch afterwards
scripts/gh-verify.sh --no-wait   # print the run URL and return
```

- It snapshots HEAD plus staged, unstaged and untracked files into one commit with a temporary index; the local branch and working tree are untouched. It exits non-zero when any job fails and prints the failed step logs.
- Pushes use the repo owner's gh account (`Indosaram`, the globally active gh account).
- Build cache: `Swatinem/rust-cache` per job, saved only on `main` (other refs cannot see their own caches after the run, but can read main's). Workspace crates are never cached, so most of a build is compiling them; that is the floor caching cannot remove. sccache with the Actions cache backend was tried (2026-10-19) and was slower: per-object uploads plus 429 rate limiting dropped 40-55% of writes. Warm the release cache with `gh workflow run release.yml --ref main` (no tag).
- Tests that compare against upstream read a pinned senpi checkout at `~/code/senpi`. CI clones `code-yeongyu/senpi` at `SENPI_PIN` (must match `tools/golden/pin.mjs`) and runs `bun install`.
- CI cannot reach the mahoquot gateway (tailnet only), so live model QA still runs locally.
- Builds/tests run only after all code work, once, on the merged result. Linux: this machine when idle, otherwise noye (`ssh box@100.110.229.115`, run cargo under `nohup setsid`). Windows: `ssh win-new`. macOS: `ssh mac`. GitHub CI is an additional check. Instabox is retired.

## INSTALLING mhc (dev build via CI)
When the user asks to (re)install mhc, run `scripts/gh-install.sh` from the checkout to install. It snapshots HEAD plus local changes to a temporary `build/YYYY.MM.DD-N` branch, `.github/workflows/mhc-binary.yml` builds `cargo build --release -p maho-cli --bin mhc` with that version as `MHC_BUILD_VERSION`, and the script downloads the `mhc-linux-x86_64` artifact into `~/.local/share/mhc/mhc` (symlinked from `~/.local/bin/mhc`), checks `mhc --version`, and deletes the branch. N counts installs made that day. It blocks until the run finishes; run it in the background and wait on its exit. Linux x86_64 only; `scripts/install-dev.sh` is the local-build fallback, used only when the user asks for it.

## RELEASES
Push a `v<version>` tag (e.g. `git tag v2026.10.18 && git push origin v2026.10.18`); `.github/workflows/release.yml` builds `mhc` and its sibling binaries (ast-grep-mcp, lsp-daemon, omo-git-bash, senpi-desktop-engine) for linux-x86_64, macos-aarch64 and windows-x86_64 with `MHC_BUILD_VERSION=<version>`, materializes the omowright browser runtime, stages the self-contained directory with `tools/package-native.mjs --skill-source latest`, verifies it, and publishes `mhc-<target>.tar.gz` (Windows: `mhc-windows-x86_64.zip`, `.exe` siblings) plus `.sha256` as a GitHub Release. Asset names carry no version so `releases/latest/download/mhc-<target>.tar.gz` always resolves; `install.sh` and `install.ps1` (the README one-liners) depend on that. Re-run for an existing tag with `gh workflow run release -f tag=<tag>` (assets are replaced).
