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

Crates without a senpi TS root (vendored Rust crates, imported COMP crates) keep free-form fragments; the audit requires at least one fragment and checks any `TS path` rows they contain.

`--all` audits every crate with a source root or a `parity.d` directory. `--self-test` runs the audit against a synthetic senpi tree and ledger.
