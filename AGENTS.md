# AGENTS.md

Conventions for anyone (human or agent) changing this repository.

## Port rules

- senpi (see PINS.md) is the only upstream. Port file-for-file: one senpi source file maps to one Rust module, keeping names and order so an upstream diff translates mechanically.
- Behavior and output must match the pinned source exactly. No visual or behavioral "improvements"; a deviation needs an N/A reason in the parity ledger.
- No codex-rs code, crates or protocols. No TypeScript extension loading or bridge; extensions are native crates registered statically in maho-cli.
- Rendering is senpi's own string/ANSI component model and differential renderer (crossterm only for raw mode and IO; no ratatui).
- Golden fixtures are generated from senpi code by tools/golden, never hand-written or auto-accepted. There is no UPDATE_GOLDEN.
- Licenses: senpi-derived crates are `license = "MIT"` (LICENSES/MIT.txt); omo-derived crates are `license = "LicenseRef-SUL-1.0"` (LICENSES/SUL-1.0.md). Third-party notices (e.g. omo-zcode-oauth) are kept with the crate that ports them.
- Project-level `.omo/` paths (plans, evidence, drafts, boulder.json) stay `.omo`; the user config home is `~/.maho/agent`.

## Crates and lints

- Every new `maho-*` crate opts into the workspace lints with `[lints] workspace = true` (`unsafe_code = "forbid"`, `clippy::unwrap_used = "deny"`; unwrap is allowed in tests via clippy.toml).
- Imported crates under crates/omo/ (except crates/omo/components and crates/omo/maho-omo) and crates/vendor/ do not opt in; they carry a crate-level allow list instead.
- Only a crate's owner todo edits its lib.rs and Cargo.toml. The root `[workspace.dependencies]` is edited only by the bootstrap and import todos; other lanes add dependencies with exact versions to their own crate, and the orchestrator hoists them.
- Dependencies use exact versions.

## Parity ledger format

- Each crate has `parity.d/<todo>.md`, one fragment per todo that touches it. A todo writes only its own fragment.
- Fragments are merged in ascending todo order; when two fragments carry a row for the same TS path, the higher-numbered todo's row wins.
- Row format (per fragment table):

  | TS test file | TS tests | Rust test file (section) | Rust tests |
  |---|---:|---|---:|

  Source files map the same way (TS source path -> Rust module). Every non-test source file under the crate's source root must be mapped, and Rust test counts may not be lower than the TS test cases unless the row gives an N/A reason.
- `bun tools/parity-audit.mjs --crate <crate> [--only <paths>]` audits a crate; scoped `--only` audits are used when several lanes share a crate.

## Lane rules

- Work happens in one git worktree per todo (branch `lane/<N>`), merged into main in dependency order after the lane's acceptance passes. The bootstrap commit is the only commit made directly on main.
- Each lane builds with `CARGO_TARGET_DIR=$HOME/.cargo-target/maho-code/lane-<N>`; at most 4 cargo-building lanes run at once.
- A lane writes only its declared crates, modules and parity fragment.
- One atomic Conventional Commit per todo, with the footer `Plan: /Users/indo/code/project/.omo/plans/omo-native-rs-tui-parity.md`.
- Evidence goes under `.omo/evidence/` in this repo. Every QA pty, daemon or browser gets a cleanup receipt. Never print keys, tokens or auth.json contents.

## Test discipline

- Port the senpi/omo tests as Rust tests; counts are audited against the TS test cases.
- Run tests with `cargo nextest run -p <crate>` with 0 skipped, and `cargo clippy -p <crate> --all-targets -- -D warnings`.
- No `#[ignore]`, no skipped tests, no deleting failing tests to go green.
- Tests are deterministic: faux provider, fixed clock and seed, temp HOME. No fixed sleeps or polling delays unless time is the behavior under test; await the exact event with a bounded timeout.
- Mocks must keep the behavior being asserted; do not pin prose or doc text with tests.
