# Provenance: Claude Code plugin hook discovery

Scope: `src/claude_plugins.rs`, its call sites in `src/config_loader.rs`, the `claude-plugins`
feature in `Cargo.toml`, and `tests/claude_plugins.rs`. The rest of this crate is the existing MIT
port of senpi's hook runtime and is not covered by this record.

- Method: **C** (clean room).
- Specification: `docs/cleanroom/specs/claude-plugin-hooks.md` at spec commit `8f3d2e29`.
- Replaces: the installed-plugin discovery previously linked from the SUL crate
  `claude-code-compat-core`; the `claude-plugins` feature no longer has any dependency.

## Allowed inputs used in the implementing session

- `docs/cleanroom/specs/claude-plugin-hooks.md` and `docs/cleanroom/specs/README.md`;
- `AGENTS.md`, the root `Cargo.toml` (workspace lints and dependencies), `rust-toolchain.toml`,
  `clippy.toml`;
- the MIT code of this crate (`crates/builtins/maho-ext-hooks/**`), `maho-ext-api` (`lib.rs`,
  `ToolCallEvent`), `maho-core::config::home_dir`;
- the MIT senpi checkout's hooks `tool-adapter.ts`, read only to confirm the existing exit-code-2
  refusal-reason behaviour of the runtime;
- grep results over `crates/maho-cli/Cargo.toml` and `docs/MIT-PROFILE.md` to find where the
  feature is referenced.

## Statement

No forbidden input was read: nothing under `crates/omo/**` (absent from this checkout),
`assets/omo-latest/**`, `assets/omo.schema.json`, `.omo/**`, the OmO-derived `maho-cli` modules,
any oh-my-openagent / OmO / lazycodex source or URL, or the git history of removed paths. Tests are
new and written from the spec's numbered acceptance tests; messages are new.
