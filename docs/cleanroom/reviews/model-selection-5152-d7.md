# Leak-check record: model-selection D7 and new mapping rows (omo v5.1.52)

Scope: `### D7` of docs/cleanroom/specs/model-selection.md and the D5 rows added in spec commit
cf9b447a0 on sync5152/spec: omo-cores-01, 03, 04, 05, 06, 09, 10. Reviewer: a separate session,
not the spec writer. This record is for reviewers only; implementers must not use it as input.

Compared against: upstream omo v5.1.52 (diff base 0a401eb8c8)
`packages/model-core/src/fable-55-launch.ts`, `model-resolution-pipeline.ts` and its test,
`category-model-requirements.ts`, `context-limit-resolver.ts`,
`model-capabilities/{bundled-snapshot,supplemental-entries}.ts`; mhc MIT
`crates/maho-model-select/src/delegate.rs`, `crates/maho-ai/data/models.json`,
`crates/builtins/maho-ext-recommended-models/src/lib.rs`; task.md §5 and Q4.

## Checklist (README rules 1-6)

- [x] No fenced code blocks.
- [x] No translated SUL comment, after one fix. "without availability proof" echoed the upstream
      pipeline comment ("has no availability proof here"). The behaviour paragraph was
      rewritten in own words ("without any availability evidence").
- [x] No row-for-row table. Only the gated id and invented providers appear.
- [x] No OmO-original identifier (`isLaunchGatedModel`, `FABLE_55_*`, `launchRungs` absent).
- [x] No OmO-original feature. Built-in category names are not reproduced.

## Behaviour check (D7)

- Upstream filters the gated id in two places only: the cold-cache category-default branch when
  a connected-provider list exists, and the cold-cache chain walk when a connected-provider list
  exists. The first-run branch (no list) is left alone. D7 matches this, and the "Unchanged"
  paragraph now names the first bullet of §5.3 step 5 explicitly.
- `delegate.rs` has the same step structure (`is_connected`, `resolve_chain` with
  `connected`), so D7 maps onto existing code.
- Tests 1-5 agree with the text. Test 3 exercises the `.` → `-` normalisation of D1.
- Reachability was added: mhc ships no built-in category data (task.md §5), so D7 only matters
  for built-in data added later or for callers passing such data. The rule stays as a safeguard.

## N-A reasons

- omo-cores-01 → D1: correct (gate, flag, accepted levels, recorded and warned rejections).
- omo-cores-03: **reason fixed.** It claimed the `maho-ai` catalog "already lists the new Kimi
  ids". `crates/maho-ai/data/models.json` has no `kimi-k3.1*` entry. The reason now says that
  capability data reaches mhc through the MIT-direct `maho-ai` catalog lane, not through this
  spec. N-A still holds: `maho-model-select` keeps no capability table.
- omo-cores-04: N-A holds; there is no capability snapshot to merge.
- omo-cores-05: N-A holds. `maho-model-select` resolves no context limits (no such code), and
  the session engine reads them from the catalog.
- omo-cores-06 → D7: correct. Reason narrowed to the exact path.
- omo-cores-09/10: upstream adds the gated rung and a split Kimi rung to built-in category
  chains. mhc ships no built-in category or agent chains (task.md §5: "No built-in category
  data is shipped"), so N-A holds. Reasons reworded to cite task.md §5 and Q4.

## Fixes made

1. Rewrote the D7 behaviour paragraph in own words. Named the step-5 first-run bullet as
   unchanged and added the reachability note.
2. Corrected the omo-cores-03 N-A reason (false catalog claim).
3. Narrowed the omo-cores-06 reason and reworded the omo-cores-09/10 reasons to cite task.md §5.

verdict: approved
