# Phase 2 step 5 — telemetry (implementer notes)

- Worktree: `/workspace/work/mhc-clean` (branch `cleanroom/phase1`). Before starting, the local
  branches lagged the remote: `origin/mit/skills-relicense` was at 01a8bfc3 (step 4 commits
  dfea3509, 0327b727, 0d184d26, dceb3f1e, 01a8bfc3) while both local branches were at 440b1f4f.
  The worktree held an older, uncommitted draft of the step 4 browser crate (pre-rustfmt, missing
  PROVENANCE/third-party notices) plus its `Cargo.toml`/`Cargo.lock` edits; it was archived to
  `/workspace/work/mhc-clean-stale-browser-draft-20261028.tgz`, removed, and both branches were
  fast-forwarded to 01a8bfc3 (the committed version supersedes the draft). The SUL paths listed in
  `phase1-env.txt` (skip-worktree in this checkout) were not opened; neither were
  `telemetry-core`, `maho-omo-telemetry`, the omo telemetry extension, the omo-gated `maho-cli`
  modules, `assets/omo*`, `.omo/`, any OmO/COMP source, or lazycodex's relicensed telemetry
  component (the OmO telemetry product is OmO-original and dropped per plan §0).
- Sources read (MIT; LICENSE checked first; checkout in `/workspace/work/upstream/oh-my-pi`, already
  pinned in PINS.md):
  - `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 (root LICENSE MIT;
    `packages/stats/LICENSE` MIT): `packages/coding-agent/src/telemetry-export.ts`,
    `telemetry-settings.ts`, `telemetry-export-otlp.ts`, `cli/stats-cli.ts`;
    `packages/agent/src/telemetry.ts` (header, span helpers, attribute keys);
    `packages/stats/{README.md,src/parser.ts,src/rollup.ts (aggregate SQL + toAggregatedStats),
    src/index.ts (printStatsSummary), src/shared-types.ts, src/client/data/formatters.ts}`;
    `packages/utils/src/format.ts`; `docs/environment-variables.md` §11.
  - senpi @ 26acd9353 was searched for a telemetry/usage-stats module; it has none to port.
  - Public conventions: OpenTelemetry OTLP/HTTP and SDK environment-variable specifications
    (endpoint paths, header list syntax, protobuf field numbers of `opentelemetry-proto`), and the
    Console Do Not Track convention (`DO_NOT_TRACK`). oh-my-pi does not read `DO_NOT_TRACK`.
- Spec step (separate commit cfa3a088, before the code): `docs/cleanroom/specs/telemetry.md`
  fixes the opt-out rules, the local ledger format, the `mhc usage` surface and the reduced OTLP
  export, written from the sources above only.
- MIT call sites read in the tree: `crates/maho-ext-api/src/types.rs` (events, extension API,
  commands), `crates/maho-ai/src/types.rs` (`Usage`, `UsageCost`, `AssistantMessage`, timing),
  `crates/maho-core/src/{config.rs,session_manager.rs}` (agent/sessions dirs, session header and
  entry layout), `crates/maho-task/src/settings.rs` (`MHC_TASK_DEPTH`),
  `crates/maho-memory/{Cargo.toml,src/extension.rs}` and `crates/maho-browser/src/extension.rs`
  (extension patterns), `crates/maho-cli/src/{main.rs,cli/default_extensions.rs,
  cli/long_term_memory.rs,cli/browser_automation.rs,cli/mod.rs}`, `crates/maho-config/src/env.rs`,
  `scripts/{check-mit-profile,mit-profile-smoke}.py`, `docs/MIT-PROFILE.md`, `PINS.md`.
- Naming: neutral names only. Crate `maho-telemetry`, mount `<builtin:telemetry>`, `maho-cli`
  module `cli/usage_stats.rs`, command `mhc usage` / `/usage`, variables `MAHO_TELEMETRY`,
  `MAHO_USAGE_STATS`, `MAHO_USAGE_DIR`, attribute prefix `mhc.` (oh-my-pi's `omp.` renamed).
- Results: `crates/maho-telemetry` (P + C, see its PROVENANCE.md and `parity.d/maho-telemetry.md`);
  `crates/maho-cli/src/cli/usage_stats.rs` (MIT glue: `<builtin:telemetry>` and `mhc usage` in the
  MIT profile only). The default (`omo`) build links the crate but does not mount it, so its
  behaviour is unchanged.
- Tests: crate unit + integration tests use no network (an in-memory transport stands in for
  HTTP); the smoke script exercises real HTTP against a loopback collector only.
