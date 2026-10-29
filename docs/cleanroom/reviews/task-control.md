# Leak-check record: task-control

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/task-control.md. Reviewer input only; not an implementer input.

## Leak self-check

- [x] no fenced code block; no SUL code, comment, tool description or message text reproduced;
- [x] no SUL type or function name; status values, the 32 KiB limit, the 5-minute stale claim and
      the transition graph are behaviour/interface facts written in new words;
- [x] identifiers are neutral (`task_send`, `team_task_*`, `pending_messages`, `all_sessions`);
      the SUL tool names that clash with `task.md`'s `task_list` were not reused;
- [x] OmO-original features (persona agents, structured shutdown hand-shake, orchestration
      modes) are absent.

## Review (leak check)

Reviewer: separate session from the spec writer; compared against `senpi-task` (control tools
`task_send`/`task_cancel`, team tasklist tools), `team-core` (`team_tasklist`) and
`maho-omo-task` (`/task-kill` command): doc comments, tool descriptions, schema descriptions,
result strings, constants and tests.

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (no fenced blocks at all);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt (after
      the fixes below);
- [x] no list or table mirrors a SUL constant table row-for-row except interface facts (status
      values, result `kind` values, tool parameter names);
- [x] no proposed identifier is an OmO-original name;
- [x] OmO-original features are absent (structured shutdown messages, one-shot reviewer agent
      refusal and paused-run "held" delivery of the SUL `task_send` are not specified).

Deliberate divergences, signed off:

- `team_task_*` tool names: approved. The SUL names `task_create/task_get/task_list/task_update`
  would collide with `task.md`'s existing `task_list`, and the prefix makes the team scope clear.
- `all_sessions`: approved. Neutral, says what it does; SUL uses a different flag name.
- Steering via the `gateway.md` gateway with queue fallback: approved with a correction. As
  written it could never steer: `gateway.md` §5 excludes `-p` runs (explicitly including
  delegated children) from receiving, and `task.md` §3 children are `-p` runs, so every send to a
  running child would have fallen back to `queued`. §1.1 now adds the delegated-run receiving
  exception, states `hop: 1`, covers the gateway-disabled case, and recovers steers left
  undrained when the run ends.

Fixes:

- §1.1: correction above (contract gap, not a leak).
- §2: `interrupted` was a `noop` for `task_cancel`, yet `/task-kill` offers interrupted tasks, so
  confirming would cancel nothing; cancelling an interrupted task now sets `cancelled`. Test 8
  extended (contract gap, not a leak).
- §3 step 2: the quoted notice text implied mandated wording; now wording is free.
- §4 intro: reworded a sentence that paraphrased the SUL `task_create` tool description
  ("does not spawn an agent").
- §4.2: reworded the `deleted` rule away from the SUL `task_update` description wording.
- §4.3: reworded the unknown-blocker rule away from the SUL doc comment wording.
- §4.5: removed the SUL echo-length constant (160) that was not an interface fact; length left
  to the implementer.
