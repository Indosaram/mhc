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

## Unit task-5152 (omo v5.1.52 delta)

- Method: **C** (clean room). Built from the `## Delta (omo v5.1.52, 2026-10-30)` section of
  `docs/cleanroom/specs/task.md` (D1-D4, D7, D8; the APPLIES rows of its D6 table) at spec commit
  `ac9221fa8`; unit `task-5152`.
- Allowed inputs (and the only ones used): that spec and the deltas of `workflow-dag.md` and
  `team.md` it routes to, this crate's existing MIT code, and the MIT crates it depends on
  (`maho-gateway` delivery ids and `session_control_delivery` details, `maho-model-select`
  `resolve_delegate_model`, `maho-ext-api`). No SUL, OmO or upstream source, checkout, cache or
  web page was opened.
- New: `src/credentials.rs` (D2 classifier, D3 remedy line), `src/steering.rs` (D7 transcript
  admission), `MAHO_TASK_PARENT_SESSION`, `SpawnedBy`/`spawned_by_from` and `Lineage` in
  `src/settings.rs` (D1), `RunSpec::child_env` in `src/runner.rs` (D1), `FallbackPlan` and
  `resolve_without` in `src/models.rs` (D4), the D4 reroute and D7 settle logic in
  `src/manager.rs`, steer ids in `src/control.rs`, and `process_info`/`is_task_child` in
  `src/record.rs` (D8). Environment variable names and the `spawnedBy` field names are interface
  facts from the spec.
- Tests (`tests/delta_5152.rs` and unit tests in the files above) were written from the delta's
  numbered tests; no fixtures were reused.
- task-b (spec copied at 0c0e537, task.md Delta D2 "Matching rules (clarified 2026-11-01)" and
  D4 test 7): an expiry text counts as `rejected` only when the sentence directly after it
  mentions `/login`; "model/models" is matched as a whole word; the final background delivery
  happens before waiters are woken. Tests `d2_test_10_*`, `d2_test_11_*` in
  `src/credentials.rs` and the D4 test 7 assertions in `tests/delta_5152.rs` follow the spec.

## Delegated task completion transport and wake batch limiting (clean-room, GAP-5 / ultrawork.md §9)

- Method: **C** (clean room). Built from `docs/cleanroom/specs/ultrawork.md` section 9 (Delegated
  Task Completion Transport and Wake Batch Limiting) and test vectors 37-39 (§10 Group H); todo 16.
- Allowed inputs: `docs/cleanroom/specs/ultrawork.md`, `crates/maho-task/` MIT code, and
  `crates/maho-config/`. No forbidden source or path was opened or read.
- New: `src/transport/` (`cap.rs`, `batch.rs`, `mod.rs`) providing `cap_completion_response`,
  `build_wake_batch`, `deliver_batch`, `deliver_settled`, `SettledBatcher` and
  `bind_settled_batcher`. UTF-16 code unit bounds without breaking surrogate pairs, raw response
  spill to `local://<state_dir>/completion-results/<task_id>.txt`, visible pointer line in message
  content with both `local://` URI and plain absolute path, graceful spill failure handling with
  visible note, well-formed `<task-result>` XML envelope preservation via
  `format_result_envelope`, and batch overflow manifest spill to
  `local://<state_dir>/completion-results/wake-overflow-<batch_id>.json` with the trailer pointer
  `N more task results: local://<path_to_manifest>`; when the manifest cannot be written the
  inline set is chosen again against the longer error trailer, so the message stays within the
  batch limit.
- `bind_settled_batcher` is defined in `src/transport/mod.rs` and called from
  `src/extension.rs` when the task manager is built. The first settler drains the queue until it
  is empty; results that settle while a delivery is in flight queue up and go out together as the
  next bounded message, and each settler returns (so its waiters wake) only after the batch
  holding its result was delivered. No timer or sleep is involved.
- `TaskSettings::from_view` in `src/settings.rs` reads `task.completion_transport_limit`
  (default 8,000, clamped [1,000, 64,000]) and `task.completion_batch_limit` (default 24,000,
  clamped [2,000, 128,000]). They are not configurable yet: the `maho-config` schema does not
  declare them, so config loading drops them as unknown keys and the defaults apply. They become
  loadable once `maho-config` registers them (todo 17).
- Tests (`tests/transport_cap.rs`): `result_capped_and_spilled` and `batch_capped_with_pointers`,
  surrogate pair bounds, within-budget inlining, below-minimum clamping, wired delivery with capped
  body, visible pointer line, and intact XML envelope (`wired_delivery_caps_body_with_pointer_and_intact_envelope`),
  spill write failure handling (`wired_delivery_failed_spill_still_slices_with_note`), batched
  delivery with overflow manifest (`wired_batch_delivery_caps_and_overflows`), batch manifest write
  failure handling (`wired_batch_delivery_failed_manifest_delivers_inline_with_note`), synchronous
  delivery before waiters wake (`settled_batcher_delivers_before_waiters_wake`), non-nested fallback
  envelope on task failure (`settled_batcher_failed_task_stderr_not_nested`), coalescing of results
  that settle during an in-flight delivery into one bounded message
  (`settled_batcher_coalesces_concurrent_tasks_through_manager`), and bounded details for a single
  overflowed task (`single_overflowed_task_details_stay_bounded`).
