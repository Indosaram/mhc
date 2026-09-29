# Parity ledger format

Each crate keeps its ledger as fragments `crates/<X>/parity.d/<todo>.md`, one per todo that touches the crate. A todo writes only its own fragment. There is no `crates/<X>/parity.md`.

`tools/parity-audit.mjs` merges the fragments in ascending todo number. When two fragments carry a row for the same TS path, the higher-numbered todo's row wins; that is how a later todo closes an earlier row.

## Table

Only tables whose first header cell is `TS path` are audited:

| TS path | Rust path | TS tests | Rust tests | status |
|---|---|---:|---:|---|
| components/text.ts | src/components/text.rs | 0 | 0 | done |
| components/text.test.ts | src/components/text_tests.rs | 12 | 12 | done |

- `TS path` is relative to the crate's senpi source root (see `SOURCE_ROOTS` in the script).
- `status` is `done`, `partial`, `todo` or `n/a: <reason>`. An optional `N/A reason` column may carry the reason instead.
- Test counts are numbers. A TS test file's case count is the number of `it(`/`test(` calls in it.

## Checks

`--crate X` fails when:

- a non-test `.ts` file under X's source roots has no row,
- a row is `todo`,
- a TS test file has more cases than the row's Rust tests and no N/A reason,
- an `n/a` row has no reason.

`--only e1,e2,!e3` scopes the audit. Entries are relative to the source root. An entry matches a path that starts with it or contains `/<entry>`. Entries prefixed `!` exclude, and a list of only `!` entries starts from every file.

`--crate X` without `--only` audits the remainder: X's source roots minus every file matched by the `--only` list of another todo for X. Those lists live in `tools/parity-owners.json`, copied from the plan's acceptance lines (todos 15, 18, 21 and 35 are the remainder owners). `--all` ignores ownership and checks every file of every crate.

Crates without a senpi TS root (vendored Rust crates, imported COMP crates) keep free-form fragments; the audit requires at least one fragment and checks any `TS path` rows they contain.

`--all` audits every crate with a source root or a `parity.d` directory. `--self-test` runs the audit against a synthetic senpi tree and ledger.

## Writing your todo's fragment

1. Create `crates/<X>/parity.d/<N>.md`, where `<N>` is your plan todo number (e.g. `crates/maho-core/parity.d/16.md`). Edit no other fragment. To close a row an earlier todo left `todo` or `partial`, repeat that TS path in your own fragment; your higher number wins.
2. Add one table with the header `| TS path | Rust path | TS tests | Rust tests | status |`. List one row per non-test `.ts` file you own (your `--only` matches, or the remainder) and one row per TS test file you port, with its `it(`/`test(` count and the number of Rust tests that cover it.
3. Prose, notes and other tables may follow. Only tables headed `TS path` are audited.
4. Run your acceptance line, e.g. `bun tools/parity-audit.mjs --crate maho-core --only '<your list>'`. Exit 0 is pass, 1 is an audit failure (each problem is printed), 2 is a usage error, and 3 means SENPI_SRC is not at the pinned commit.

## Which checkout is audited

The audited repo is the first that applies:

1. `--repo <path>`,
2. `$PARITY_REPO`,
3. the nearest ancestor of the current directory that holds both `Cargo.toml` and `crates/`,
4. the checkout that contains the script.

So a lane can run another checkout's script against its own tree: `cd /Volumes/T9-Mac/maho-code-lanes/lane-4 && bun ../lane-3/tools/parity-audit.mjs --crate maho-grep` audits lane-4. senpi is read from `$SENPI_SRC` (default `/Users/indo/code/senpi`), and it must be at fe8c564b.

## Golden and faux tools (tools/golden)

- `bun tools/golden/run.mjs --case <name>|--all` renders `tools/golden/cases/*.json` with pinned senpi and writes `crates/<crate>/tests/golden/`. Fixtures come only from this script. Never edit them by hand, and there is no update mode on the Rust side.
- `bun tools/golden/faux-harness.mjs --scenario <name> [--omo] --out <json>` runs a headless senpi AgentSession on the faux provider with `tools/golden/scripts/<name>.json`. It writes normalized events, entries and tool results.
- `bun tools/golden/faux-tui.mjs --script <name> [--omo] [-- <senpi args>]` runs senpi's interactive mode on faux in the current terminal. It is the reference side for pty comparisons.
- Both faux tools run under a private temp HOME in offline mode. `--omo` loads omo-senpi from `$OMO_SRC` (default `/Users/indo/code/oh-my-openagent`), and it must be at 77f3067f.
