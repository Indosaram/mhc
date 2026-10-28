# Provenance — maho-task

- License: MIT.
- Method: **P** (port of MIT originals) for the task model; **C** (clean-room, from
  `docs/cleanroom/specs/task.md`) for the process runner, state directory and interfaces, which
  have no MIT original.
- Origin (license verified as MIT before reading): `can1357/oh-my-pi` @
  `f59bbac3d56e8b485fef466388b1cb5be4b93123` (LICENSE: MIT, Copyright 2025 Mario Zechner,
  2025-2026 Can Bölük, 2026 Stencil Labs, Inc.), `packages/coding-agent/src/`:
  - `task/agents.ts`, `task/discovery.ts`, `discovery/helpers.ts` (`parseAgentFields`) →
    `src/agents.rs` (frontmatter fields, reserved names, "absent tools keep defaults, malformed
    tools do not widen access", project-before-user-before-bundled precedence);
  - `prompts/agents/task.md`, `prompts/agents/scout.md` → `src/agents/{task,scout}.md` (adapted:
    oh-my-pi `yield`/structured-output, `@role` model selectors and its tool names removed);
  - `task/output-manager.ts` → `src/ids.rs` (unique ids with `-2`, `-3` suffixes, seeded from
    existing outputs);
  - `task/parallel.ts` (`Semaphore`, concurrency limit semantics) → `src/manager.rs` slots;
  - `task/result-summary.ts`, `prompts/tools/task-summary.md` → `src/summary.rs` (`<task-result>`
    envelope, inline vs preview with a pointer to the full output, status labels);
  - `task/index.ts` (background jobs delivered as follow-ups), `task/restore.ts` and
    `prompts/tools/task-follow-up.md` (a stopped agent continues with its context) →
    `src/manager.rs`, `src/extension.rs`;
  - `task/types.ts` `canSpawnAtDepth`, task schema (`tasks[]` + shared `context`) →
    `src/settings.rs`, `src/extension.rs`.
- Model choice uses `maho-model-select::resolve_delegate_model` and the `maho-config` view
  (`categories`, `agents`, `task`), both MIT crates of this repository.
- Not ported: oh-my-pi in-process session executor, IRC messaging, workpool, seance, structured
  output/yield, eval tools, speculative launch, prewalk/advisor, name generator word lists, TUI
  renderers. Worktree isolation and team/tmux are only traits here (Phase 3).
- No SUL source (`crates/omo/**`, including `senpi-task`, `maho-omo-task`, `delegate-core`,
  `isolation-core`, `team-core`, `tmux-core`; the omo-gated `maho-cli` task modules) was opened.
- Tests were written for this crate from the spec; no upstream or OmO fixtures were reused.

## Task control and team tasklist (Phase 4)

- Method: **C** (clean room).
- Spec: `docs/cleanroom/specs/task-control.md` at spec commit `8f3d2e29` (plus the files it
  references: `task.md`, `team.md`, `gateway.md`).
- Allowed inputs of the implementing session: the spec files under `docs/cleanroom/specs/`,
  `AGENTS.md`, and the MIT code already in this checkout (`crates/maho-task`, `crates/maho-team`,
  `crates/maho-gateway`, `crates/maho-ext-api`, `crates/maho-tools`, `crates/maho-config`,
  `crates/maho-cli/src/cli/{delegation,team_view,session_gateway,default_extensions}.rs`).
- No forbidden input was read: nothing under `crates/omo/**`, `assets/omo-latest/**`,
  `assets/omo.schema.json` or `.omo/**`, no oh-my-openagent / OmO / lazycodex source or URL, no
  git history of removed paths, and no other machine's copy of this repository.
- Tests were written new from the spec's numbered acceptance tests (§6); no fixtures were reused.
