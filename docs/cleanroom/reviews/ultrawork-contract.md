# Leak-check record: ultrawork-contract (contract requirements spec)

Independent reviewer record for `docs/cleanroom/specs/ultrawork-contract.md` (plan todo 5). This record is reviewer input only, not implementer input. The reviewer session is separate from the spec writers of todos 3 and 4. The trace annex `ultrawork-contract-trace.md` is parent-only and out of scope.

## Compared SUL set

- `/home/projects/mhc-wt/ulw-sul-ref` (51 files: `maho-omo-ultrawork`, `maho-omo-todo-fanout-reminder`, `maho-omo-mass-ulw`, `senpi-task/src/completion`), extracted at the todo-1 baseline.
- `maho-omo-skill-pointers` (11 files) is in the todo-5 scope but missing from ulw-sul-ref, so it was copied read-only from the main checkout to `/tmp/t5-sp`.
- 62 files in total. Each check first asserts that the file set is non-empty.

## Checks

1. 8-word overlap: `tools/ulw-bench/ngram-overlap.mjs` (exits 1 on any 8-gram) was run against each SUL file separately. `/tmp/t5/overlap-count.mjs` wraps the same `findOverlap` export to print a per-file count and the matching grams.
2. Identifiers, constants, test values and regex literals: `/tmp/t5/idcheck.mjs` collects every string literal of 8 or more characters (per line), every `fn`/`const`/`static`/`struct` name containing `_` or camel case, and every `snake_case:` field from the SUL `.rs` files, then searches the spec for each one.
   - Allowlisted interface facts (README rule 2, README-ulw clause 2): `ultrawork`, `ulw mass`, `mass ulw`, `mass-ulw`, `/skill:ultrawork`, `ulw-plan`, `ulw-loop` (published skill names), `completion-results`, `local://`, `task_id`, `state_dir` (on-disk path/URI facts that the spec mandates), plus plain dictionary words.
   - The script source is in the evidence file.
3. Regexes: the SUL keyword and masking regexes (`index.rs:24-39`) were compared by hand with spec section 1. The spec describes the masked regions by category only and contains none of the patterns. The SUL `RELAY_HEADER` shape and the `loop|plan|research|execute|mass` exemption list do not appear.
4. Comment wording: SUL doc comments (`index.rs:22-23,40,47,49,57,62-64`, `notification.rs`, `reminder.rs`) were compared by hand with spec sections 1, 4, 6 and 9. Apart from the leaks fixed below, no sentence follows a SUL comment.

## Pre-fix identifier check (engine spec, shown for the shared checker)

```
$ git stash -q && node /tmp/t5/idcheck.mjs docs/cleanroom/specs/ultrawork.md /home/projects/mhc-wt/ulw-sul-ref/crates /tmp/t5-sp | grep -v ': 0$'; git stash pop -q
  HIT identifier matched_ulw
  HIT identifier matched_ultrawork
  HIT identifier occurrence_count
  HIT field matched_ulw
  HIT field matched_ultrawork
  HIT field occurrence_count
omo/components/maho-omo-ultrawork/src/index.rs: 6
  HIT literal "ulw_helper"
omo/components/maho-omo-ultrawork/tests/arming.rs: 1
total: 7
exit=1
```

The pre-fix overlap and identifier checks on this contract spec already printed 0. Its leaks were paraphrases that only the row-by-row clause (iv) reading below catches.

## Clause (iv) row-by-row audit

The SUL source is `crates/omo/components/maho-omo-ultrawork/src/directive.txt` (31,542 bytes) at the listed lines.

- `ok`: the row states an observable behavior in its own structure, carries no SUL sentence shape, example or ordering, and any MIT cite points at text that exists in `assets/omo-latest/skills/ultrawork/SKILL.md`.
- `reworded <SUL file:line>`: the pre-review row read as a line-level rewording of that SUL sentence and was rewritten in place by this review.

Ordering check: the spec groups its rows into 11 topic tables (1.1-1.11). These tables do not follow the SUL section order (Role, Goal, Tier, QA channels, Bootstrap, Finding, Parallel, Execution, Waiting, task tools, Child, Gate, Commits, Constraints, Output, Stop). For example, Constraints rules R65-R68 sit in table 1.1 and Stop rules sit with interaction rules in 1.9. Clause 2 is satisfied.

| Row | SUL rule source (directive.txt line) | Result | Clause (iv) reason |
| --- | --- | --- | --- |
| R1 | 11 | ok | One-line posture restated with the spec's own failure framing; the MIT Role section carries the same rule, so citing it satisfies clause 5. |
| R2 | 14-18 | ok | States the real-surface proof requirement abstractly; no clause of the SUL Goal paragraph (green suite vs user-facing) is mirrored. |
| R3 | 20-35 | ok | Collapses the SUL trigger list into two categories; no enumerated fact list or quoted signal is carried over. |
| R4 | 27,36-38 | ok | Default-and-ratchet is restated in one clause; the SUL 'when unsure take HEAVY' and redo wording is absent. |
| R5 | 22-26 | ok | Describes control-plane sizing in neutral terms; the SUL 'payload' and 'sync, prompt, create, verify' sequence are not reused. |
| R6 | 59-61 | ok | HTTP channel described by what it checks; no command flags or capture tuple order from the SUL list. |
| R7 | 62-65,108-114 | ok | Names the capture goal only; the SUL repo script path and tmux prohibition text are not reproduced. |
| R8 | 66-81 | ok | Keeps one fact (do not wipe the user's browser state) without the SUL engine names or CDP method list. |
| R9 | 85-89 | ok | One-sentence GUI channel rule with its own failure mode; the SUL tool examples (AppleScript, xdotool) are omitted. |
| R10 | 94-100 | ok | Scenario precision rule stated as inputs/command/binary observable; the SUL bad-example quotes are absent. |
| R11 | 102-106 | ok | Auxiliary-evidence rule restated; the SUL 'never count' list (--dry-run, should respond) is not copied. |
| R12 | 122-126 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:122 | Pre-review text ('Defer goal and plan tracking to external orchestrators when running under an active loop runner') tracked the SUL supersede sentence clause by clause, so it was rewritten around the outcome (one source of truth) and its MIT cite, which pointed at text that does not exist in SKILL.md, was replaced. |
| R13 | 119 | ok | Per-task temp root and private caches restated generically; env var names and the mktemp invocation are not reproduced. |
| R14 | 120 | ok | Cleanup scope rule in the spec's own words; the SUL 'retain worktrees still needed by open PRs' clause is not mirrored. |
| R15 | 129-135 | ok | Skill survey compressed to select-then-load-on-use; the SUL 'skipping a skill that fits is a defect' framing is absent. |
| R16 | 136-141 | ok | Survey-before-edit stated as an outcome; the SUL discovery-wave contents list (code, git history, memory, evidence) is not copied. |
| R17 | 143-157 | ok | Planner spawn condition restated as 'only when boundaries or decompositions are uncertain'; SUL examples and ordering are not reused. |
| R18 | 159-182 | ok | Goal registration with stop criterion is generic; the SUL call-shape instruction ('exactly objective; do not include status') is absent. |
| R19 | 185-194 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:185 | Pass 1 still followed the SUL wake-source list (monitor, pending child, scheduled continuation) in SUL order, with the 'frozen while the wake event is in flight' framing. Pass 2 restates it as the mhc goal tool's `blocked` semantics (an interface fact) with its own failure mode, and cites no MIT text. |
| R20 | 196-199 | ok | Deletion row describing a removed mechanism by name; it carries no SUL sentence. |
| R21 | 201-203 | ok | Deletion row; the header name is cited as the removed artifact, not restated as an instruction. |
| R22 | 205-206 | ok | Deletion row; cites the removed section name only. |
| R23 | 208-209 | ok | Deletion row; cites the removed section name only. |
| R24 | 211-212 | ok | Deletion row; cites the removed section name only. |
| R25 | 214-215 | ok | Deletion row; cites the removed section name only. |
| R26 | 217-218 | ok | Deletion row; cites the removed section name only. |
| R27 | 220-221 | ok | Deletion row; cites the removed section name only. |
| R28 | 235-241 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:235 | Pre-review text ('Persist multi-step implementation plans to structured files in project storage before modifying code') followed the SUL 'write the ordered plan to a file FIRST' sentence and cited a MIT section that lacks the rule; rewritten with the mirror relation as the observable and the MIT location cite corrected. |
| R29 | 243-252 | ok | Real-time checklist updates are also in MIT SKILL.md section 3, which the row cites; the SUL verb list (start/done/append/drop) is not enumerated. |
| R30 | 253-255 | ok | Cleanup-todo-at-creation restated briefly; the SUL resource enumeration (PID, tmux session, port, container) is absent. |
| R31 | 256-262 | ok | Step-text shape stated as target/rationale/check; the SUL WHERE/WHY/HOW/VERIFY template and GOOD/BAD examples are not copied. |
| R32 | 270-272 | ok | LSP-first symbol lookup is in MIT SKILL.md Finding things; the row cites it and uses its own failure framing. |
| R33 | 273-276 | ok | Structural search rule without the SUL tool invocation details ($VAR, MCP op names). |
| R34 | 277-278 | ok | Generic text-search rule; the SUL arrow list is not mirrored. |
| R35 | 279-282 | ok | Explore-agent dispatch stated by its trigger; matches the MIT Finding things content it cites. |
| R36 | 285-293 | ok | Concurrency of independent reads restated in one sentence; the SUL Promise.all/parallel(thunks) instruction text is absent. |
| R37 | 288-291 | ok | Prefer runtime builtins stated generically; the SUL Bun.$/Bun.Glob/fetch list is not reproduced. |
| R38 | 294-295 | ok | Parallel independent / sequence dependent is a generic rule with no SUL phrasing overlap. |
| R39 | 296-298 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:296 | The pre-review text ('Execute mutating filesystem and environment modifications individually, checking the outcome of each') closely reworded the SUL sentence 'ONE ACTION AT A TIME, each observed before the next' and was rewritten as an observable. MIT SKILL.md Finding things does not contain this rule, so the cite is now 'none in MIT text' (pass 2 fix). |
| R40 | 304-308 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:304 | Pre-review text ('Inspect graphical user interface modifications sequentially across desktop and mobile viewports') reworded the SUL screenshot-after-each-change sentence and cited a MIT section that lacks it; rewritten around the capture-and-check observable with no MIT cite. |
| R41 | 309-310 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:309 | Pre-review text ('Switch to a Python evaluation kernel when JavaScript evaluation is occupied') restated the SUL 'HOP to py' instruction; the deletion row now names the mechanism neutrally and cites correction 10 instead of a MIT section that does not contain it. |
| R42 | 311-314 | ok | Disjoint write scopes are in MIT SKILL.md Delegation on mhc; the row adds its own failure mode and no SUL clause. |
| R43 | 321-326 | ok | Read-tests-first rule from MIT Execution loop, cited; the SUL framing that a failing test found before the change is a finding is absent. |
| R44 | 327-328 | ok | Smallest-change rule is generic and cites the MIT Execution loop. |
| R45 | 328-336 | ok | Add-test-only-when condition condensed; the SUL mutation-coverage sentence and test-restating examples are absent. |
| R46 | 343-346 | ok | Real-surface scenario run restated with its own failure mode. |
| R47 | 347-355 | ok | Teardown rule without the SUL command list (kill -0, tmux kill-session, lsof). |
| R48 | 356-357 | ok | Diagnostics-clean rule in one sentence; no SUL phrasing. |
| R49 | 359-366 | ok | Rerun-what-moved then full run before delivery is condensed; the SUL evidence-validity clause is not mirrored. |
| R50 | 372-390 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:372 | Pass 1 tracked the SUL Waiting discipline headline and listed events in the SUL enumeration. Pass 2 drops the event list and keeps only the pushed-event-versus-polling observable and the cost reason, which match the MIT section it cites. |
| R51 | 391-394 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:391 | Pass 1 still tracked the SUL rule of registering in the same eval cell that starts the work. Pass 2 states the requirement as the outcome: any operation a later decision depends on has a completion notification arranged before the agent moves on. It has no step or cell coupling and no SUL event list. |
| R52 | 395-396 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:395 | Pre-review text ('Inspect background process logs only when necessary to make execution decisions') restated the SUL 'Peek ONLY for a midpoint decision' sentence; rewritten and cited to the MIT midpoint-peek line in Delegation on mhc. |
| R53 | 399-405 | ok | Category-or-agent-type dispatch is an mhc task-tool interface fact, also in MIT Delegation on mhc. |
| R54 | 401-407 | ok | Read-only analysis agents stated as a rule with its own failure mode; no SUL sentence order. |
| R55 | 409-418 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:409 | Pre-review text ('Coordinate multi-agent collaborative efforts using shared team taskboards and structured messaging') condensed the SUL team_create paragraph and cited a MIT section without it; rewritten with the dependency condition and no MIT cite. |
| R56 | 421-423 | ok | Child brief contents are in MIT Delegation on mhc (TASK/DELIVERABLE/SCOPE/VERIFY/STOP WHEN) which the row paraphrases at category level. |
| R57 | 424-430 | ok | Status-as-progress plus one follow-up is also in MIT Delegation on mhc; the row uses its own failure framing. |
| R58 | 432-441 | ok | Terminal-state barrier matches MIT Subagent-dependent transition barrier, cited; no SUL clause list. |
| R59 | 443-451 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:445 | Pre-review text added the SUL plan-file trigger in the SUL's 'only when explicitly requested or heavyweight with approved plan' shape; rewritten as the two-situation rule in the spec's words. |
| R60 | 452-453 | ok | Self-review contents (diff, diagnostics, evidence) also appear in MIT Verification gate, which it cites. |
| R61 | 463-472 | ok | Criterion-tied blockers and two re-reviews on delta are in MIT Verification gate, cited. |
| R62 | 473-477 | reworded crates/omo/components/maho-omo-ultrawork/src/directive.txt:474 | Pre-review text ('Escalate unresolved reviewer disagreements to the user via structured interactive question dialogs') reworded the SUL step-5 escalation; rewritten naming the real tool names as interface facts. |
| R63 | 480-482 | ok | Deletion row stating why the frequent-commit rule is wrong; no SUL sentence reused. |
| R64 | 483-491 | ok | History-matching commit format rule cites MIT Commits; the SUL command list is not copied. |
| R65 | 494-498 | ok | Evidence-over-inspection rule in the spec's own words; the SUL invalid-proof examples are absent. |
| R66 | 499-507 | ok | Blast-radius fix rule condensed; the SUL tracked-issue and delegated-scope clauses are not mirrored. |
| R67 | 508-509 | ok | Never-suppress rule restated generically; matches MIT Constraints, cited. |
| R68 | 510 | ok | One-clause evidence rule; generic. |
| R69 | 513 | ok | Deletion row; names the removed banner mechanism, does not restate the banner text. |
| R70 | 514 | ok | Plan summary after setup is generic and cites MIT Output discipline. |
| R71 | 515-519 | ok | Handoff reports at phase transitions restated; the SUL slot labels are not reproduced in the row. |
| R72 | 520-522 | ok | Final summary content rule condensed; the SUL commit-list format is absent. |
| R73 | 525-527 | ok | Answer-as-soon-as-possible rule in the spec's words; cites MIT Stop rules. |
| R74 | 528-536 | ok | Stop condition restated as criteria plus teardown receipts; the SUL 'FUNDAMENTALLY fulfilled' emphasis is absent. |
| R75 | 537-539 | ok | Leftover QA state means not done; generic restatement. |
| R76 | 540-542 | ok | Two-failure ask rule; matches MIT Stop rules, cited. |
| R77 | 543-544 | ok | Two empty waves stop rule; matches MIT Stop rules, cited. |

Totals: 77 rows, 65 `ok`, 12 reworded (R12, R19, R28, R39, R40, R41, R50, R51, R52, R55, R59, R62).

Cite fixes made while rewording: R12, R19, R28, R40, R41 and R55 cited MIT SKILL.md sections that do not contain the rule. For example, R12 cited `:112 (Bootstrap)`, but MIT SKILL.md has no outer-runner supersede text. Those rows now cite the correct MIT location or say `none in MIT text`, as clause 5 requires.

## Other sections

| Section | Result | Reason |
| --- | --- | --- |
| 2. Corrections 1-14 | ok | Each correction states a delta from the legacy contract in the spec's own words. Correction 3 quotes the mhc intent-gate line, which is mhc's own system prompt and not SUL. Correction 12's tool names and correction 11's `scripts/tui-capture.mjs` are interface facts. |
| 3. Reference topics table | ok | The topic ids and triggers are the spec's own partition. No SUL table has this shape. |
| 4. Size budget | ok | `31,542` is a measured byte count of the SUL file (a number, not text). 14-16 KB and 6,000 B are new targets. |
| 5. Scenarios S1-S5 | ok | The prompts are the todo-2 ulw-bench fixtures written by this plan, not SUL test values. `idcheck.mjs` reports 0 literal hits. |

## Checklist (README.md, as amended by README-ulw.md)

- [x] No fenced code block from the SUL tree. The spec has none.
- [x] No sentence translates or paraphrases a SUL prompt (after the 12 rewordings above).
- [x] No list mirrors a SUL table row for row. The rows are grouped by the spec's own topics.
- [x] No proposed identifier is SUL-internal. The reference topic ids are new.
- [x] Only in-scope features (README-ulw clause 1).

## Required leakcheck (orchestrator --ids list, pass 3)

The command was run on both specs and its output is recorded verbatim. Both must exit 0.

```
$ bun /home/projects/mhc/.omo/ulw-execute/leakcheck.mjs docs/cleanroom/specs/ultrawork.md /home/projects/mhc-wt/ulw-sul-ref --ids 'UltraworkExtension,ArmingTracker,ArmingSnapshot,PromptClassification,InvocationStage,InvocationRoute,SuppressionReason,was_armed,compact_rearm_pending,post_compact_rearm,<omo-ultrawork-reminder>,<ultrawork-mode>,omo-ultrawork:directive'
{"spec":"docs/cleanroom/specs/ultrawork.md","sulFiles":51,"overlapLines":0,"idLines":0}
exit=0
$ bun /home/projects/mhc/.omo/ulw-execute/leakcheck.mjs docs/cleanroom/specs/ultrawork-contract.md /home/projects/mhc-wt/ulw-sul-ref --ids 'UltraworkExtension,ArmingTracker,ArmingSnapshot,PromptClassification,InvocationStage,InvocationRoute,SuppressionReason,was_armed,compact_rearm_pending,post_compact_rearm,<omo-ultrawork-reminder>,<ultrawork-mode>,omo-ultrawork:directive'
{"spec":"docs/cleanroom/specs/ultrawork-contract.md","sulFiles":51,"overlapLines":0,"idLines":0}
exit=0
```

## Commands run after the fixes (pass 2)

```
$ bun /home/projects/mhc/.omo/ulw-execute/leakcheck.mjs docs/cleanroom/specs/ultrawork-contract.md /home/projects/mhc-wt/ulw-sul-ref --ids "$(cat /tmp/t5/ids.txt)"
{"spec":"docs/cleanroom/specs/ultrawork-contract.md","sulFiles":51,"overlapLines":0,"idLines":0}
exit=0
```
The `--ids` list in `/tmp/t5/ids.txt` holds the SUL function, struct, field and constant names, the disable flag, `ulw_helper` and `<omo-ultrawork-reminder>`:
`SessionArming,shared_session_arming,UltraworkClassification,ArmingSnapshot,classify_ultrawork_input,is_ultrawork_input,visible_invocations,strip_quoted_regions,strip_quoted_spans,cannot_contain_keyword,already_embedded,skill_invocation_suppressed,reselect_context_directive,directive_for_model,select_directive_text,rearm_on_compact,mark_armed,track_session,matched_ulw,matched_ultrawork,occurrence_count,ULTRAWORK_REMINDER,TODO_FANOUT_REMINDER,FINAL_RESPONSE_TRANSPORT_LIMIT,is_mass_ulw_input,skill_already_invoked,ulw_helper,omo-senpi-ultrawork-disabled,<omo-ultrawork-reminder>,SENPI_ULTRAWORK_DIRECTIVE`


```
$ node /tmp/t5/overlap-count.mjs docs/cleanroom/specs/ultrawork-contract.md /home/projects/mhc-wt/ulw-sul-ref/crates /tmp/t5-sp | tail -2
files: 62 total: 0
exit=0
$ for f in $(find /home/projects/mhc-wt/ulw-sul-ref/crates /tmp/t5-sp -type f | sort); do node tools/ulw-bench/ngram-overlap.mjs docs/cleanroom/specs/ultrawork-contract.md $f; echo exit=$?; done | sort | uniq -c
     62 exit=0
     62 No matches found.
$ node /tmp/t5/idcheck.mjs docs/cleanroom/specs/ultrawork-contract.md /home/projects/mhc-wt/ulw-sul-ref/crates /tmp/t5-sp | tail -3
maho-omo-skill-pointers/tests/skill_pointers_suppression.rs: 0
maho-omo-skill-pointers/tests/support/mod.rs: 0
total: 0
exit=0
```
The full per-file listing (62 lines, each `: 0`) is in `.omo/evidence/ulw-workflow-overhaul/task-5-review.txt`.

## Negative control

The checkers are shared with the engine-spec review. A seeded copy carrying a SUL test value, a SUL field name and an 8-word `directive.txt` phrase was flagged by all three checks (exit 1 each), then deleted. See `.omo/evidence/ulw-workflow-overhaul/task-5-neg.txt`.

Verdict: clean
