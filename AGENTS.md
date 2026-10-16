# AGENTS.md

**Generated:** 2026-10-13
**Commit:** 04a9b789
**Branch:** preserve/main-uncommitted-before-latest-omo

## OVERVIEW
Rust native port of senpi + oh-my-openagent (maho). File-for-file translation preserving senpi ANSI terminal component model, differential renderer, and agent runtime.

## STRUCTURE
```
crates/
├── maho-cli/            # CLI binary entry point, command dispatch, config migration
├── maho-agent/          # Core agent loop, execution harness, state machine
├── maho-ai/             # Multi-provider LLM abstraction layer & model clients
├── maho-interactive/    # Terminal UI, streaming reveal pacing, theme engine
├── maho-server/         # Daemon RPC server, SQLite storage, protocol frames
├── maho-tui/            # Keyboard input, fuzzy search, terminal mux primitives
├── omo/senpi-task/      # Multi-agent DAG workflow runner, supervisor, category dispatch
├── omo/memory-core/     # Git-backed persistent memory filesystem, identity profiles
└── builtins/            # Builtin tool definitions, execution sandboxes
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| CLI commands & flags | `crates/maho-cli/` | Entry point, CLI routing, migrations |
| Agent loop & harness | `crates/maho-agent/` | Event stream parsing, tool execution |
| Provider integrations | `crates/maho-ai/` | Anthropic, OpenAI, Bedrock, Gemini |
| Terminal presentation | `crates/maho-interactive/` | ANSI differential renderer, streaming widgets |
| Background tasks & DAG | `crates/omo/senpi-task/` | Task supervisor, dependency scheduling |
| Memory & identity | `crates/omo/memory-core/` | Markdown block store, git auto-commit |

## PORT RULES & CONVENTIONS
- senpi is upstream. Port file-for-file: one TS file maps to one Rust module, keeping names and order.
- Rendering uses senpi's string/ANSI component model and differential renderer (crossterm for raw mode/IO; no ratatui).
- Every new `maho-*` crate opts into workspace lints: `unsafe_code = "forbid"`, `clippy::unwrap_used = "deny"`.
- Native Rust port must not delegate core agent behavior to external JS `omo` CLI.

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

## REMOTE BUILD ON INSTABOX
When the user says "instabox에서 빌드" / "build on instabox" (or asks for check/clippy/test there), run cargo on an instabox sandbox instead of this machine:

```bash
scripts/instabox-build.sh                                   # cargo build -p maho-cli on bong-g-1
scripts/instabox-build.sh -- check --workspace              # any cargo args after --
scripts/instabox-build.sh -m bong-h-2 -k h -- clippy -p maho-ai --all-targets -- -D warnings
```

- Run it from the checkout/worktree whose code should be built. It resets the box clone to `merge-base HEAD origin/main`, then applies local commits after that, staged/unstaged edits and untracked files as one binary diff. Nothing is pushed. `instabox exec --stdin` caps one transfer at 1 MB, so the diff is sent in 900 KB chunks and reassembled on the box; untracked files over 1 MB (build output like `dist/`) are skipped.
- Machines: `bong-g-1..5` use key `g`, `bong-h-1..5` use key `h` (`-m`, `-k`; default `bong-g-1`/`g`). Each box is 4 vCPU / 8 GB, Debian 13; repo at `/data/home/mhc`, log at `/data/home/instabox-run.log`. Give parallel jobs different machines.
- Keys live only in `~/.config/instabox/keys.env` (`INSTABOX_KEY_G`, `INSTABOX_KEY_H`). Never write a key into the repo; it is public. Ad-hoc commands: `INSTABOX_API_KEY=$INSTABOX_KEY_G instabox exec <machine> -- <cmd>` (`instabox ls`, `resume`, `ssh`, `files`).
- Boxes auto-pause after 30 min idle; `exec` resumes them. First run on a fresh box installs rustup 1.95.0 into `~/.rustup` (`/opt/rust` is root-owned) and protoc 29.3 into `~/protoc` (needed by `maho-ai` build.rs; no apt/sudo).
- `/data` is only 20 GB: a default debug build (full debuginfo + incremental) fills it. The script writes a box-side `~/.cargo/config.toml` (line-tables-only debuginfo, no debuginfo for deps, incremental off) and wipes `target/` when less than ~6 GB is free.
- The script blocks until cargo finishes and exits non-zero on failure; run it in the background and wait on its completion. Cargo runs under `setsid nohup` on the box because `instabox exec` kills its children on return; `exec --timeout` is capped at 600 s.
- Missing CLI: `npm i -g instabox@latest` (binary lands in `~/.local/bin/instabox`).

