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
