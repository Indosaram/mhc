# Contributing to mhc

mhc is a file-for-file Rust port of [senpi](https://github.com/code-yeongyu/senpi) with
[oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent) on top. Upstream revisions are
pinned in [PINS.md](PINS.md); the pinned behavior is the behavior of record. Agent-facing rules
live in [AGENTS.md](AGENTS.md) and the per-crate `AGENTS.md` files.

## Setup

- Rust 1.95.0 (`rust-toolchain.toml` selects it, with clippy and rustfmt).
- `protoc` on `PATH` (the `maho-ai` build script compiles vendored protobuf schemas).
- [Bun](https://bun.sh) for the scripts under `tools/`.
- [cargo-nextest](https://nexte.st) for tests.
- For golden fixtures and parity audits: senpi checked out at the pinned commit (`$SENPI_SRC`) and
  oh-my-openagent at its pinned commit (`$OMO_SRC`).

## Build, lint, test

```bash
cargo check --workspace
cargo clippy -p <crate> --all-targets -- -D warnings
cargo nextest run -p <crate>
cargo build --workspace --bins --release     # mhc, ast-grep-mcp, lsp-daemon, ...
```

The workspace lints (`unsafe_code = "forbid"`, `clippy::unwrap_used = "deny"`, ...) are set in the
root `Cargo.toml`; every new crate opts in with `[lints] workspace = true`. Tests must not skip,
sleep or poll: await the exact event with a bounded timeout. `scripts/gh-verify.sh` runs clippy, the
build and the whole test suite on GitHub Actions for your checkout, and `scripts/gh-install.sh` builds
and installs a release `mhc` there (see [AGENTS.md](AGENTS.md)).

## Porting rules

- One senpi TypeScript file maps to one Rust module, keeping names and order.
- Rendering keeps senpi's string/ANSI component model and differential renderer (crossterm only; no
  ratatui, no codex-rs).
- Extensions are native crates registered statically; there is no TypeScript bridge.
- Golden fixtures come only from `tools/golden` run against pinned senpi; never edit them by hand.
- Record what you ported in the crate's parity ledger: see [docs/parity.md](docs/parity.md).

## Packaging and QA

- `bun tools/package-native.mjs --binary target/release/mhc --output dist/mhc` stages an install,
  `--verify-only dist/mhc` checks it against its manifest (see the README).
- `tools/hg-v-*.mjs`, `tools/residual-qa.mjs` and `tools/qa/` are
  real-surface acceptance drivers against a built binary with an isolated `HOME`; each documents its
  usage in the file header.

## Commits

Conventional Commits with a crate scope, e.g. `fix(interactive): ...`, `docs(agents): ...`. One
verified change per commit.

## License

The repository is MIT. Contributions are accepted under the MIT License; third-party components
keep their own licenses (see [LICENSE](LICENSE)).
