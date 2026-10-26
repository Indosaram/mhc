# Clean-room record: review-work

- Base: lazycodex `75b017473c71d669dd9aed0a10e950e70724b283`, `plugins/omo/skills/review-work` (MIT), copied from `cleanroom/review-work/base/`.
- Inputs: MIT base + `cleanroom/review-work/spec.md` only. No SUL-1.0 text opened, read, grepped or diffed.
- Coordinator decision (spec §2): keep the five base-only rules (blocking gate, ledger record per lane/SHA, inline blockers, review worktree, evidence redaction) under the neutral heading "Gate rules".

## Changes

| Spec item | Done | What |
|---|---|---|
| §1.1 | yes | Deleted "Codex Harness Tool Compatibility". |
| §1.2 | yes | Deleted "Codex Subagent Reliability"; its neutral substance rewritten in my own words as "Lane dispatch and reliability" for mhc tools: one `task` call (`run_in_background=true`, self-contained prompt), reviewer is a leaf (no `team_*`, one `task` even inside a `workflow`), single-use (re-review = new `task` on the delta), wait without polling, ended-without-deliverable = INCONCLUSIVE never PASS, one smaller retry, still emit final report. |
| §1.3 | yes | Deleted the "On Codex, use review-work only when..." paragraph. One gate reviewer is always required. |
| §2 | yes | Five rules kept under "## Gate rules"; `$ulw-execute` -> "an execute/autopilot workflow". |
| renames | yes | `oracle` -> "your configured read-only review agent" / `subagent_type="reviewer"`; ulw-loop/ultrawork mentions in Phase 1 rewritten generically; Phase 3 "reliability followup" reworded to point at the new section. Added a "## Lanes" heading over the existing one-reviewer paragraph and table. |

## Acceptance

- `grep -nE 'lazycodex|multi_agent|fork_context|fork_turns|\.codex|On Codex' SKILL.md` -> empty; no oracle/omo/ulw/ultrawork/Sisyphus.
- Body still has exactly two lanes (manual QA by orchestrator + one gate reviewer); verdict logic unchanged (PASS only on no failing QA row AND APPROVE).

## Left as-is (flag)

- `omowright` / `connectCloakProfile` (browser library API names) in Phase 1 web channel kept from base.
