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
├── desktop/             # senpi-desktop-*: computer-use engine and platform backends (MIT; tool layer is in omo/)
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

## LOCAL SCRATCH (`.omo/`)
`.omo/` is gitignored local state: session evidence, plans, benchmarks, the private build-host
note. Nothing in it is committed, so no tracked file may depend on a path under it. Parity
ledgers citing `.omo/evidence/...` point at local artifacts that exist only on the machine that
produced them.

## LOCAL VERIFICATION
GitHub Actions is disabled for this repository: `.github/workflows/ci.yml` keeps only
`workflow_dispatch` (no push or pull-request trigger) and the workflow is disabled in the
repository settings. Verification runs on this machine.

```bash
scripts/verify-local.sh          # fmt --check, clippy -D warnings, nextest --workspace
scripts/verify-local.sh --fast   # fmt --check and clippy only
```

- It runs on the checkout as it stands, staged/unstaged/untracked included, and stops at the first failing step with that step's tail.
- `cargo nextest run --workspace` is the whole suite; use `-p <crate>` while iterating, but land on the workspace run.
- Tests that compare against upstream read a pinned senpi checkout at `~/code/senpi` (`SENPI_PIN` in `tools/golden/pin.mjs`).
- Builds/tests run only after all code work, once, on the merged result. Machine names and commands for the private build hosts stay out of this public repo; they live in the gitignored `.omo/build-hosts.local.md`.

## INSTALLING mhc (local dev build)
When the user asks to (re)install mhc, run `scripts/install-dev.sh` from the checkout. It builds a release `mhc` locally, packages it like a release and installs it as `versions/YYYY.MM.DD-N/` under the install root with the stable root launcher and `current.json`, then checks `mhc --version`. N counts installs made that day. Linux x86_64 only.

GitHub Actions is disabled repository-wide, so no workflow builds a binary for an install: every build runs on this machine (or another host the user names explicitly) and is started only when they ask for it.

## RELEASES
GitHub Actions is disabled repository-wide, so `release.yml` no longer builds anything automatically. Releases are built locally on each target host (linux-x86_64, macos-aarch64, windows-x86_64) and published from here; `release.yml` stays in the tree, disabled, only as a record of the previous pipeline. Packaging remains `tools/package-native.mjs --skill-source latest`, and asset names still carry no version so `releases/latest/download/mhc-<target>.tar.gz` resolves for `install.sh` and `install.ps1`.
