# Parity tracking

mhc proves it matches upstream in two ways: a per-crate ledger that maps every upstream TypeScript
file to its Rust port, and golden fixtures rendered by the pinned upstream itself.

## Ledgers (`parity.d/`)

Each crate keeps its ledger as fragments under `crates/<crate>/parity.d/<todo>.md`, one fragment per
work item. Tables headed `| TS path | Rust path | TS tests | Rust tests | status |` are audited;
`status` is `done`, `partial`, `todo` or `n/a: <reason>`. The full format, the ownership rules and
how a later fragment closes an earlier row are in [tools/PARITY-FORMAT.md](../tools/PARITY-FORMAT.md).

```bash
SENPI_SRC=/path/to/senpi bun tools/parity-audit.mjs --crate maho-core
SENPI_SRC=/path/to/senpi bun tools/parity-audit.mjs --all
bun tools/parity-audit.mjs --self-test
```

The audit exits 0 on pass, 1 on an audit failure, 2 on a usage error and 3 when `SENPI_SRC` is not
at the commit pinned in [PINS.md](../PINS.md). `tools/parity-owners.json` assigns each todo its file
list (`--only`).

## Golden fixtures (`tools/golden/`)

```bash
bun tools/golden/run.mjs --case <name>     # or --all
```

renders `tools/golden/cases/*.json` with pinned senpi and writes `crates/<crate>/tests/golden/`. The
Rust tests compare against those files and have no update mode. `tools/golden/faux-harness.mjs` and
`faux-tui.mjs` run senpi (optionally with OmO via `--omo` and `$OMO_SRC`) on the faux provider in an
isolated temporary `HOME`, as the reference side for event and pty comparisons.

## Other checks

- `bun tools/check-omo-sites.mjs` verifies the `.omo` / `.maho` home-directory literals against
  `tools/omo-sites.json`.
- `tools/run-session2-residual-gate.mjs` runs a declared gate command set and
  `tools/verify-session2-residual.mjs` re-verifies its evidence.

## Further reading

- [docs/divergences.md](divergences.md) — Intentional safety and privacy divergences from upstream
- [docs/perf-divergences.md](perf-divergences.md) — Intentional performance divergences from upstream

