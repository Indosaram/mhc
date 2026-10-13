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

