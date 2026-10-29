# Provenance - maho-workflow

- License: MIT.
- Method: **C** (clean room). Behaviour source: `docs/cleanroom/specs/workflow-dag.md` at spec
  commit `8f3d2e29`, plus the files it references: `docs/cleanroom/specs/task.md` (task runner,
  state directory) and `docs/cleanroom/specs/config-keys.md` (`task.dag.*` keys and defaults).
  `docs/cleanroom/specs/README.md` gave the rules (roles, forbidden inputs, neutral names, new
  tests, new messages).
- Allowed inputs this session read:
  - the spec files listed above;
  - mhc MIT code in this checkout: `AGENTS.md`, the root `Cargo.toml`
    (`[workspace.dependencies]`, lints), `clippy.toml`, `rust-toolchain.toml`,
    `crates/maho-task/**` (public API: `TaskScope`, `TaskAddon`, `AddonTool`, `AddonCommand`,
    `TaskManager`, `TaskRecord`, `StateDir`, `record::pid_alive`), `crates/maho-team/**`
    (as an example of a task addon and a PROVENANCE file), `crates/maho-skills/src/**`
    (`strip_front_matter`), `crates/maho-gateway/src/{extension,store}.rs`,
    `crates/maho-ext-api/src/{lib,types}.rs` (tool/extension types), and
    `crates/maho-cli/src/cli/{default_extensions,delegation,team_view}.rs` (how MIT extensions
    and task addons are registered), plus grep hits in `crates/maho-core/src/{config,brand,
    package_manager}.rs` for the project config dir name.
- No forbidden input was read: nothing under `crates/omo/**`, `assets/omo-latest/**`,
  `assets/omo.schema.json` or `.omo/**`; no oh-my-openagent / OmO / lazycodex checkout or URL;
  no git history of removed paths (`git log -p`, `git show <old>`, `git checkout <old>`); no
  other machine's copy of this repository; none of the omo-derived `maho-cli` modules listed in
  the spec README; no output of the SUL code.
- Tests (`tests/workflow.rs` and the unit tests in `src/`) were written from the spec's numbered
  acceptance tests (§7); no fixture, golden file or test from another implementation was used.
- Identifiers are neutral (`WorkflowDefinition`, `WorkflowNode`, `WorkflowRun`,
  `WorkflowStore`, `WorkflowScheduler`, `WorkflowAddon`, `TaskExecutor`); every message text is
  this crate's own.
