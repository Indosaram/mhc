---
name: ulw-loop
description: "A goal-like loop that decomposes work into systematic, evidence-bound ultrawork steps. Use when the user wants a goal loop or durable, checkpointed execution."
argument-hint: "<goal>"
metadata:
  short-description: Goal-like ultrawork loop for systematic decomposition
---

# ulw-loop

Use this skill when the user asks for `ulw-loop`, `ulw`, durable goal execution, evidence-led work, manual QA, or checkpointed long-running delivery.

This skill is compact by design: the run contract below is the whole bootstrap. `references/full-workflow.md` and `references/define-goal.md` carry the full doctrine; open a section only when the phase you are in needs it.

## Run contract

1. Create goals from bash: `mhc ulw-loop create-goals --brief "<brief>" --json` (CLI below). The session's bash binds `PI_SESSION_ID`, so never pass a plan path. If the output reports `ULW_LOOP_PLAN_EXISTS_COMPLETE` (this session's aggregate is already complete), unrelated new work needs a fresh session; `--force` is only for deliberately overwriting completed evidence.
2. Register the aggregate objective from the returned handoff with `create_goal`, shaped by `references/define-goal.md`. Goal creation is NEVER skipped.
3. Mirror every atomic step into the live `todo` checklist: one granular step per action, exactly one in_progress, transitions marked the instant they happen.
4. Treat each goal as a phase: create its own worktree off the integration base; dispatch its dependency-ordered lanes as ONE `workflow` run (read the mass-ulw skill first; ordering-free lanes stay a `task` batch); verify every criterion with real-surface evidence; land the worktree on the integration base at `mhc ulw-loop checkpoint --goal-id <id> --status complete --evidence "..." --json` per the repository's flow (direct merge or merged PR); define the next goal's run from what this one proved. Tests alone never prove done. When a mass-ulw pointer accompanies this skill, this contract still owns goals, criteria, evidence, and checkpoints.
5. Stop when the goal's WHEN-TO-STOP line holds with evidence in hand.

When the injected ultrawork directive accompanies this skill, its goal/notepad/todo bootstrap is subsumed by this contract: the loop CLI owns goal state and the loop ledger is the notepad. Do not create a second one.

## The CLI: `mhc ulw-loop <subcommand> --json`

Every ulw-loop operation runs from bash through the native CLI. There is no JS SDK to import and no `omo_agent_toolkit` tool.

```bash
mhc ulw-loop status --json
```

Rules that keep it working:

- Always pass `--json`. Every call prints one JSON object: `{ "ok": true, ... }` on success or `{ "ok": false, "error": { "code", "message", "details"? } }` on failure. Read `nextActions` before deciding the next step, and branch on `error.code`, not on the message text.
- Never pass a plan path. The session's bash binds `PI_SESSION_ID`, so the CLI targets this session's plan; state lives under `.omo/ulw-loop/<session-id>/`. Pass `--session-id <id>` only when that env is missing (`ULW_LOOP_SESSION_ID_REQUIRED`).
- Run `mhc ulw-loop help` (or `mhc ulw-loop <subcommand> --help`) to recover every usage line below.

Subcommands (flags are exact):

| Subcommand | Flags |
|---|---|
| `help` | none; prints every usage line |
| `status` | none; output carries `plan`, `summary`, `nextActions`, `evidenceRoot` (stable plan-level artifact dir), `currentAttemptDir` (moves with the active goal) |
| `create-goals` | `--brief "..."` \| `--brief-file <path>` \| `--from-stdin`, `[--codex-goal-mode aggregate\|per_story]`, `[--validation-batch-json <json-or-path>]`, `[--force]` |
| `complete-goals` | `[--retry-failed]`; acquires the next eligible goal or resumes the in-progress one |
| `criteria` | `--goal-id <id>` |
| `record-evidence` | `--goal-id <id> --criterion-id <id> --status pass\|fail\|blocked --evidence "..." [--notes "..."]` |
| `checkpoint` | `--goal-id <id> --status complete\|failed\|blocked --evidence "..." [--codex-goal-json <json-or-path>] [--quality-gate-json <json-or-path>] [--no-advance]`, or `--print-template [--goal-id <id>]` for the quality-gate template |
| `steer` | `--kind <kind> --evidence "..." --rationale "..." [...kind flags]`, or `--proposals-json <json-or-path>`; kinds and their flags are in `references/full-workflow.md` |
| `add-goal` | `--title "..." --objective "..."`; the three seeded placeholder criteria name the exact `steer --kind revise_criterion` call that replaces each one |
| `record-review-blockers` | `--goal-id <id> --title "..." --objective "..." --evidence "..." --codex-goal-json <json-or-path>` |

## Non-Negotiables

- Write loop state only through the CLI; it lives under `.omo/ulw-loop/<session-id>/` and is never hand-edited. Mutations are serialized across processes by the session's `.state.lock`, so parallel `record-evidence` calls from workers are safe.
- Register goals up front, shaped by `references/define-goal.md` (`mhc ulw-loop create-goals --brief "<brief>" --json`, then `create_goal` from the printed handoff), and mirror every atomic step into the live `todo` checklist: one ultra-granular step per action, exactly one in_progress, transitions marked the instant they happen.
- After any compaction or context loss, re-read brief + goals + ledger FIRST plus `mhc ulw-loop status --json` (`mhc ulw-loop help` lists every subcommand with its flags), then resume; never re-plan from scratch.
- If `create-goals` answers `ULW_LOOP_PLAN_EXISTS_COMPLETE`, this session's aggregate is already done: start unrelated new work in a fresh session instead of steering or forcing the completed state. Use `--force` only to intentionally overwrite completed evidence.
- Every success criterion needs observable evidence from a real surface: a channel (terminal/TUI via the xterm.js web terminal, HTTP, browser, computer-use) or, for CLI- or data-shaped criteria, an auxiliary surface (CLI stdout, DB diff, parsed config dump).
- Evidence is bound to the tree it was captured at (`git rev-parse --short "HEAD^{tree}"`); it goes stale only when tracked content changes — a rebase or amend that keeps the tree identical keeps it valid. When the tree differs, re-run at the current HEAD and re-record, never relabel or regenerate. Record only after cleanup receipts exist.
- Delegate code edits, test writes, fixes, and QA execution to right-sized omo-senpi subagents through the native `task` tool or through `workflow` nodes when the phase's lanes carry ordering.
- Use `git-master` for git-tracked edits: inspect recent and touched-path commit history, then commit each verified work unit atomically in the repository's observed language, scope, and message style with only that unit's files staged. Never carry verified units into a later omnibus commit.

## Team mode: decide it, do not default to it

Solo execution with parallel background `task` workers is the default: fan independent units out in one batched spawn, each routed to the `category` (or configured `subagent_type`) that fits it, with scopes cut so no two workers write the same files. A team (`team_create`) adds per-member briefing, shared-state, and relay overhead, so it must be paid for by the work's shape. Decide ONCE, when the plan's work units are known, and record the verdict plus its reason in the notepad.

Stand up a team when BOTH hold:

1. **The units' scopes overlap in a way you cannot cleanly cut.** They touch the same module, contract, or migration, so one unit's discovery changes what another should do. Fire-and-forget workers cannot exchange that mid-flight; teammates can, because the lead relays it.
2. **Running them at the same time actually finishes sooner.** The units are each substantial and none is merely waiting on another's output. Two units where the second only consumes the first's result are a sequence, not a team.

When the units are genuinely independent — separate files, no shared contract — spawn parallel background `task` workers instead and avoid the team coordination overhead entirely. When the work is one cohesive unit, do it yourself. Overlap alone is not enough: near-identical units that would collide on the same lines are faster done in sequence by one worker.

Under team mode, isolate and land per unit:

- **One git worktree per member**, never a shared checkout — concurrent members editing one working tree corrupt each other's diffs and evidence. Give each member its own branch off the base and its own worktree path.
- **Merge per work unit, as each unit is verified.** A member's unit lands when its own evidence is captured and its gates are green; it does not wait for the slowest sibling. Integrate each merged unit back into the base the others branch from, so overlapping members rebase onto real merged work rather than guessing at it.
- **Conflicts are the lead's job.** When two members' units touch the same lines, the lead decides the order they land and tells the later member what changed; members never resolve a sibling's conflict blind.

## Native Senpi Task Contract

Senpi already exposes its real subagent spawn surface through the omo-senpi `task` component. Use it directly. Do not route delegation through external app-server threads or another harness.

| Intent | Native Senpi tool |
| --- | --- |
| Spawn one worker | `task({ prompt, subagent_type | category, run_in_background: true })` |
| Fan out independent workers | `task({ tasks: [{ prompt, subagent_type | category }, ...], run_in_background: true })` |
| Send context or correction | `task_send({ task_id, message })` |
| Inspect one midpoint | `task_output({ task_id, mode: "tail" })` |
| Stop a runaway worker | `task_cancel({ task_id })` |
| Coordinate overlapping work | `team_create`, `task_create`, `task_get`, `task_list`, `task_update`; communicate with `task_send` |

Every worker prompt starts with `TASK:` and names `DELIVERABLE`, `SCOPE`, `VERIFY`, and `STOP WHEN`. Put requested skill names and all required context inside `prompt`; children do not inherit interview context automatically.

## Driver goal lifecycle

The session goal you create with `create_goal` is a DRIVER the loop instructs, never a gate. A checkpoint accepts an optional driver snapshot, records it verbatim, and never rejects on its status or objective; the advice comes back in `nextActions`. The snapshot is filled from this session's goal store automatically, so pass `codexGoal` only to override it. If the driver was completed early, the advice tells you to `create_goal` again with the plan's objective verbatim - a completed goal is replaced, not reused. If the driver is paused or usage/budget limited, the advice tells you to resume it. A different objective is a warning, not a refusal, and it is reported once: the first checkpoint under that driver records the objective in the plan (`acknowledgedDriverObjectives`) and later calls stay quiet; `status().result.driver` shows the relation at any time. `create_goal` is advised only when the goal store really holds no goal.
