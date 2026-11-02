# Leak-check record: ultrawork (engine behavior spec)

Independent reviewer record for `docs/cleanroom/specs/ultrawork.md` (plan todo 5). This record is reviewer input only, not implementer input. The reviewer session is separate from the spec writers of todos 3 and 4.

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

## Pre-fix identifier check (spec as received)

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

## Findings and fixes

| # | Spec location | SUL source | Finding | Result |
| --- | --- | --- | --- | --- |
| 1 | section 7.1 and vectors 1-5 | `maho-omo-ultrawork/src/index.rs:84` (`UltraworkClassification`) | The field names `matched_ulw`, `matched_ultrawork` and `occurrence_count` were the SUL struct's field names. These are internal identifiers, which README-ulw clause 2 requires to be neutral. | Renamed to `short_form_seen`, `long_form_seen` and `visible_mentions` throughout. Behavior is unchanged. |
| 2 | vector 21 | `maho-omo-ultrawork/tests/arming.rs:19,35,258` | The input `my_ulw_helper` embedded the SUL test value `ulw_helper` (the memory-note leak pattern of SUL test values reused as examples). | Replaced with `parse_ulw_flags`, which tests the same suppression case. |
| 3 | section 2.2 step 3 | `index.rs:7` (`ULTRAWORK_REMINDER`) | The reminder was described clause by clause in the SUL constant's order. After the first fix the spec still named the literal `<omo-ultrawork-reminder>` tag, which the orchestrator leakcheck `--ids` list flags (pass 2 verifier finding). | Pass 1 rewrote the wording. Pass 2 removed the tag from the implementer spec: the notice now carries one fact, with wording and markup left to the implementer. The tag-compatibility fact stays only in the parent-only annex `docs/cleanroom/specs/ultrawork-integration.md:121`. |
| 4 | section 6.2 | `maho-omo-todo-fanout-reminder/src/reminder.rs:1-9` | The four numbered principles mirrored the four SUL steps (SIZE / COMPUTE / TELL / KEEP). The pass-1 rewrite was still a four-bullet list in SUL order and reused 'spawn and coordination overhead', 'disjoint write' and the three-part status-upkeep list (pass 2 verifier finding). | Pass 2 rewrote it as one outcome paragraph in the spec's structure: the model makes an explicit fan-out choice, judged on independence and net time saved, and tells the user. The checklist-upkeep fact is one generic clause. None of the flagged phrases remain (the grep exits 1). Pass 3 restored the sizing fact as its own first clause: before choosing, the model weighs how much work the whole new list represents, by item count and per-item effort. This is not the SUL wording, and the n-gram check, idcheck and leakcheck still print 0. |
| 5 | section 10 header | none | The header said 38 vectors, but the section has 39. | Corrected to 39. This is not a leak. |
| 6 | section 9.1 | `senpi-task/src/completion/notification.rs:100-103` | `<state_dir>/completion-results/<task_id>.txt` and `local://` match the SUL spill path. | Kept as is (`ok`). The path and URI are on-disk/user-visible interface facts that the parent reads back. The limits (8,000 / 24,000) differ from the SUL `32_000`, and the batch manifest is new behavior. |
| 7 | vectors 19-20, 28-32 | `arming.rs:14-15,25,45`; `mass-ulw/tests/input.rs:5` | `ulw-loop`, `ulw-plan`, `mass ulw`, `mass-ulw` and `ulw mass` also appear as SUL test inputs. | Kept as is (`ok`). They are published skill names or user keywords allowed by README-ulw clause 2, and every surrounding sentence is new. Behavior note, not a leak: SUL arms on bare `ulw-loop`/`ulw-plan` (`arming.rs:25,45`), while vectors 19-20 suppress them. This is a spec decision for the parent to confirm in todo 14. |
| 8 | vector 38 and section 9 | `completion_tests.rs:445,456,549` | The SUL tests use `"x".repeat(32_001)` and `2_000`. The spec uses 10,000, 8,000, an emoji at unit index 8,000, and 5 children of 6,000 each. | `ok`: none of the values is a SUL test value. |
| 9 | section 1.3 | `index.rs:24-39` | The masked-region list (fences, inline code, quotes, blockquotes, URLs, injected tags, relay headers, negation) covers the same categories as the SUL masks. | `ok`: these are behavior categories required for compatibility. No regex, tag name list, or SUL verb list (`load/use/run/invoke/start/trigger/arm/enable`) is reproduced. |

## Checklist (README.md, as amended by README-ulw.md)

- [x] No fenced code block is Rust, TypeScript or JS from the SUL tree. The spec has no fenced code; its inline literals are test inputs.
- [x] No sentence is a translation or paraphrase of a SUL comment, doc comment or prompt (after fixes 3 and 4).
- [x] No list or table mirrors a SUL constant table row for row (after fix 4). The mass keyword list and the config keys are marked as interface facts.
- [x] No proposed identifier is an OmO-original or SUL-internal name (after fix 1).
- [x] Only the features that README-ulw clause 1 brings in scope are specified. No persona prompt set and no plan-continuation state format are specified.

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
$ bun /home/projects/mhc/.omo/ulw-execute/leakcheck.mjs docs/cleanroom/specs/ultrawork.md /home/projects/mhc-wt/ulw-sul-ref --ids "$(cat /tmp/t5/ids.txt)"
{"spec":"docs/cleanroom/specs/ultrawork.md","sulFiles":51,"overlapLines":0,"idLines":0}
exit=0
```
The `--ids` list in `/tmp/t5/ids.txt` holds the SUL function, struct, field and constant names, the disable flag, `ulw_helper` and `<omo-ultrawork-reminder>`:
`SessionArming,shared_session_arming,UltraworkClassification,ArmingSnapshot,classify_ultrawork_input,is_ultrawork_input,visible_invocations,strip_quoted_regions,strip_quoted_spans,cannot_contain_keyword,already_embedded,skill_invocation_suppressed,reselect_context_directive,directive_for_model,select_directive_text,rearm_on_compact,mark_armed,track_session,matched_ulw,matched_ultrawork,occurrence_count,ULTRAWORK_REMINDER,TODO_FANOUT_REMINDER,FINAL_RESPONSE_TRANSPORT_LIMIT,is_mass_ulw_input,skill_already_invoked,ulw_helper,omo-senpi-ultrawork-disabled,<omo-ultrawork-reminder>,SENPI_ULTRAWORK_DIRECTIVE`

The same leakcheck on the pass-1 spec (`git show e61321220:docs/cleanroom/specs/ultrawork.md`) printed `idLines:1` (line 73, the tag) and `exit=1`. This confirms the pass-2 fix and the `--ids` list.


```
$ node /tmp/t5/overlap-count.mjs docs/cleanroom/specs/ultrawork.md /home/projects/mhc-wt/ulw-sul-ref/crates /tmp/t5-sp | tail -2
files: 62 total: 0
exit=0
$ for f in $(find /home/projects/mhc-wt/ulw-sul-ref/crates /tmp/t5-sp -type f | sort); do node tools/ulw-bench/ngram-overlap.mjs docs/cleanroom/specs/ultrawork.md $f; echo exit=$?; done | sort | uniq -c
     62 exit=0
     62 No matches found.
$ node /tmp/t5/idcheck.mjs docs/cleanroom/specs/ultrawork.md /home/projects/mhc-wt/ulw-sul-ref/crates /tmp/t5-sp | tail -3
maho-omo-skill-pointers/tests/skill_pointers_suppression.rs: 0
maho-omo-skill-pointers/tests/support/mod.rs: 0
total: 0
exit=0
```
The full per-file listing (62 lines, each `: 0`) is in `.omo/evidence/ulw-workflow-overhaul/task-5-review.txt`.

## Negative control

A seeded copy at `/tmp/t5/seeded-ultrawork.md` added the SUL test value `"ulwultrawork"`, the field `matched_ulw`, and the `directive.txt:17-18` phrase. Running the same commands flagged it:

- `ngram-overlap.mjs` printed `Found 9 matches.` and exited 1.
- `overlap-count.mjs` reported a total of 24 and exited 1.
- `idcheck.mjs` reported 3 hits and exited 1.

The copy was then deleted. The full output is in `.omo/evidence/ulw-workflow-overhaul/task-5-neg.txt`.

Verdict: clean
