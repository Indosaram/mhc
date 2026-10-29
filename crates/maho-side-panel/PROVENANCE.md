# Provenance - maho-side-panel

- License: MIT.
- Method: **C** (clean room). Built from `docs/cleanroom/specs/side-panel.md` at spec commit
  `8f3d2e29`, together with the files it references (`docs/cleanroom/specs/config-keys.md`
  section `side_panel` and `memory.enabled`, `docs/cleanroom/specs/task.md` section 2 for the task
  state directory) and the role, naming and test rules in `docs/cleanroom/specs/README.md`.
- Allowed inputs of the implementing session (and the only ones used):
  - the spec files above;
  - `AGENTS.md` (workspace conventions);
  - MIT code already in this checkout, read for the APIs this crate calls or follows as a
    pattern: `crates/maho-ext-api` (extension, flag, command, event, UI, side-column, custom
    overlay, exec and goal-lifecycle types), `crates/maho-tui` (`Component` and mouse event
    types), `crates/maho-interactive` (`side_column.rs` and the `SideColumn` UI request handling,
    to learn how a side column is mounted and its regular-mode fallback), `crates/maho-config`
    (`load_native`, `LoadResult`, the `side_panel` schema rule), `crates/maho-task`
    (`record.rs` field names), `crates/maho-memory` (`MemoryContext`, `Banks`, scoping),
    `crates/builtins/maho-ext-goal` (goal record field names and store path) and
    `crates/builtins/maho-ext-builtin-loose/src/diff.rs` (custom overlay factory usage),
    `crates/maho-core/src/sdk/extension_context.rs` (`goal_store_file`),
    `crates/maho-feedback` (crate layout, extension wiring and `PROVENANCE.md` form),
    `crates/maho-cli/src/cli/default_extensions.rs` and `crates/maho-cli/src/cli/long_term_memory.rs`
    (how MIT extensions are registered), the root `Cargo.toml` (workspace dependencies and lints).
- No forbidden input was read: nothing under `crates/omo/**`, `assets/omo-latest/**`,
  `assets/omo.schema.json` or `.omo/**`; no omo-gated `maho-cli` module; no OmO /
  oh-my-openagent / lazycodex / COMP checkout or URL; no git history of removed paths; no copy of
  the repository on another machine; no output of running SUL code.
- All identifiers, messages, glyphs and section layouts are new. Interface facts (the
  `side_panel.*` keys and bounds, the `--omo-side-panel` flag, the `/side-panel-diff` command,
  task status values, the git command lines and the `mhc-panel:` link scheme proposed by the spec)
  are reproduced from the spec as it requires.
- Tests (`tests/spec.rs`, `t01` .. `t26`) were written from the spec's numbered acceptance tests;
  no upstream or OmO fixtures were reused.
