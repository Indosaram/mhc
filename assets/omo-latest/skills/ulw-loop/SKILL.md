---
name: ulw-loop
description: "A goal-like loop that decomposes work into systematic, evidence-bound ultrawork steps. Use when the user wants a goal loop or durable, checkpointed execution."
argument-hint: "<goal or brief to run as a loop>"
metadata:
  short-description: Goal-like ultrawork loop for systematic decomposition
---

# ulw-loop

Use this skill when the user asks for `ulw-loop`, `ulw`, durable goal execution, evidence-led work, manual QA, or checkpointed long-running delivery.

This skill is intentionally compact. The full workflow lives in `references/full-workflow.md`. Read only the sections needed for the current phase, then execute them exactly.

## Required First Steps

1. Open `references/full-workflow.md`.
2. Read through **Bootstrap** (including its tier triage), **Execution Loop**, the **Manual-QA channels** table, and the **Stop Rules** before running any ULW command or recording evidence.
3. Open `references/define-goal.md` and register the run's goal by it. Goal creation is NEVER skipped: shape the objective and every success criterion by that reference before any implementation.
4. If the task has code edits, tests, QA, or commit work, follow the full workflow's delegation and evidence rules. Tests alone never prove done.

## Non-Negotiables

- Use the ulw-loop CLI state under `.omo/ulw-loop`; do not hand-edit goal state. The CLI is `mhc ulw-loop <command>` (the same `mhc` binary running this session).
- Register goals up front, shaped by `references/define-goal.md` (`mhc ulw-loop create-goals`, then `create_goal` from the printed handoff), and mirror every atomic step into the live `todo` checklist: one ultra-granular step per action, exactly one in_progress, transitions marked the instant they happen. Use the ultrawork directive’s Task resources and todo rules; read those sections of the bundled `ultrawork` skill if absent from context.
- After any compaction or context loss, re-read brief + goals + ledger FIRST plus `mhc ulw-loop status --json`, then resume; never re-plan from scratch.
- Every ulw-loop command needs the session scope: pass `--session-id <id>` (the printed handoff and resume directive carry it; `PI_SESSION_ID`, which mhc's `bash` tool exports, or `OMO_ULW_LOOP_SESSION_ID` in the environment also resolves it). The CLI refuses unscoped state (`ULW_LOOP_SESSION_SCOPE_REQUIRED`) instead of touching the shared `.omo/ulw-loop` root.
- If `mhc ulw-loop create-goals` says this session's aggregate is already complete, start unrelated new work with a fresh `--session-id <new-id>` (passed on every later call) instead of steering or forcing the completed state. Use `--force` only to intentionally overwrite completed evidence.
- Every success criterion needs observable evidence from a real surface: a channel (terminal/TUI via the xterm.js web terminal, HTTP, browser, computer-use) or, for CLI- or data-shaped criteria, an auxiliary surface (CLI stdout, DB diff, parsed config dump).
- Evidence is bound to the tree it was captured at (`git rev-parse --short "HEAD^{tree}"`); it goes stale only when tracked content changes — a rebase or amend that keeps the tree identical keeps it valid. When the tree differs, re-run at the current HEAD and re-record, never relabel or regenerate. Record only after cleanup receipts exist.
- Delegate code edits, test writes, fixes, and QA execution to right-sized `task` workers when the workflow requires it.
- Every `task` prompt starts with `TASK:`, then names `DELIVERABLE`, `SCOPE`, `VERIFY`, and `STOP WHEN`; put role and specialty instructions inside the prompt, which is all the child sees of your context.
- Plan and reviewer children may run for a long time; start them with `run_in_background: true` and keep doing independent root work until their completion notification arrives. Do not poll `task_output` in a loop.
- For work likely to exceed one wait cycle, require the child to send `WORKING: <task> - <current phase>` before long reading, testing, or review passes, and `BLOCKED: <reason>` only when it cannot progress.
- Track spawned task ids and names locally. A child's own report is a claim, not proof of completion. A quiet child is still alive until `task_output` says otherwise.
- While children run, surface the active subagent count, agent names, and latest `WORKING:` phase.
- Fallback only when the child is completed without the deliverable, ack-only after a `task_send` follow-up, explicitly `BLOCKED:`, or no longer running. Then record inconclusive and spawn a smaller task with the missing deliverable.
- Use `git-master` for git-tracked edits: inspect recent and touched-path commit history, then commit each verified work unit atomically in the repository's observed language, scope, and message style with only that unit's files staged. Never carry verified units into a later omnibus commit.

## Team mode: decide it, do not default to it

Solo execution with parallel background `task` workers is the default. A team (`team_create`) adds per-member briefing, shared-state, and relay overhead, so it must be paid for by the work's shape. Decide ONCE, when the plan's work units are known, and record the verdict plus its reason in the notepad.

Stand up a team when BOTH hold:

1. **The units' scopes overlap in a way you cannot cleanly cut.** They touch the same module, contract, or migration, so one unit's discovery changes what another should do. Fire-and-forget workers cannot exchange that mid-flight; teammates can, because the lead relays it.
2. **Running them at the same time actually finishes sooner.** The units are each substantial and none is merely waiting on another's output. Two units where the second only consumes the first's result are a sequence, not a team.

When the units are genuinely independent — separate files, no shared contract — spawn parallel background `task` workers instead and avoid the team coordination overhead entirely. When the work is one cohesive unit, do it yourself. Overlap alone is not enough: near-identical units that would collide on the same lines are faster done in sequence by one worker.

Under team mode, isolate and land per unit:

- **One git worktree per member**, never a shared checkout — concurrent members editing one working tree corrupt each other's diffs and evidence. Give each member its own branch off the base and its own worktree path. Apply the same resource rules when creating member worktrees.
- **Merge per work unit, as each unit is verified.** A member's unit lands when its own evidence is captured and its gates are green; it does not wait for the slowest sibling. Integrate each merged unit back into the base the others branch from, so overlapping members rebase onto real merged work rather than guessing at it.
- **Conflicts are the lead's job.** When two members' units touch the same lines, the lead decides the order they land and tells the later member what changed; members never resolve a sibling's conflict blind.

## mhc tool mapping

The workflow's orchestration steps map to these mhc tools:

| Intent | mhc tool |
| --- | --- |
| Spawn a worker | `task` with `category` (`quick`, `unspecified-low`, `unspecified-high`, `deep-low`, `deep-high`, `ultrabrain`, `visual-engineering`, `writing`) or `subagent_type` (`explore`, `librarian`, `plan-consultant`, `plan-reviewer`); skills it needs go in `load_skills` |
| Fan out a wave | one `task` call with a `tasks` array (up to 16), or several calls with `run_in_background: true` |
| Peek at a running worker | `task_output` (`mode: "status"` or `"tail"`) — once, not in a loop |
| Steer, re-task, or revive a worker | `task_send` |
| Stop a runaway | `task_cancel` |
| Wait for a worker | nothing: its completion arrives as a notification carrying the result |
| Team lanes | `team_create`, `team_status`, `team_delete`; shared board on `task_create` / `task_list` / `task_update` |
| Live checklist | `todo` (`op: "init"`, `"append"`, `"start"`, `"done"`, `"drop"`) |
| Goal driver | `get_goal`, `create_goal`, `update_goal` |

## Driver goal lifecycle

The session goal (`create_goal` / `get_goal` / `update_goal`) is a driver the loop instructs, not a gate of its own. `checkpoint` accepts an optional goal snapshot through the CLI flag `--codex-goal-json` (the flag keeps its historical name; on mhc pass the `get_goal` result, shaped `{"goal":{"objective":"<plan codexObjective verbatim>","status":"complete"}}`, inline or as a file path) and records it in the ledger. Its advice comes back in `nextActions`: with no snapshot it tells you to call `get_goal` and, when nothing is active, `create_goal` with the plan's objective verbatim. Input that is neither JSON nor a readable JSON file, or a snapshot the CLI cannot reconcile with the plan, fails with `CODEX_GOAL_SNAPSHOT_ERROR`; read the message, fix the snapshot, and rerun.
