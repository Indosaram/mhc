# Leak-check record: config-keys D2 `model_pad` (omo v5.1.52)

Scope: `### D2. model_pad` of docs/cleanroom/specs/config-keys.md and its mapping rows
omo-cores-11 and omo-cores-12 (spec commit cf9b447a0 on sync5152/spec). Also the rewritten
"Rows that were checked" paragraph. Reviewer: a separate session, not the spec writer. This
record is for reviewers only; implementers must not use it as input.

Compared against: upstream omo v5.1.52 `packages/omo-config-core/src/schema/model-pad.ts`,
`model-pad.test.ts`, `config.ts`, `memory.ts` (diff base 0a401eb8c8); mhc MIT
`crates/maho-config/src/schema.rs` (`sections`, `typed_harness_block`, `profile_body`, `root`);
docs/cleanroom/specs/config.md §4.4 and §6.

## Checklist (README rules 1-6)

- [x] No fenced code blocks. The JSON in the tests is user config.
- [x] No translated SUL comment, after one fix. The "Meaning" paragraph tracked the upstream
      doc comment on the schema closely: a two-by-two grid with columns daily/geeky and rows
      normal/heavy, an unset label keeping the built-in word, and the square's name and model
      staying in `model_profiles.<id>`. It was rewritten as a plain statement: each label key is
      a `family` or `tier` value, the string is the caption a client may show, and model
      selection never reads it.
- [x] No row-for-row constant table. Only user-file key names and types appear (interface
      facts).
- [x] No OmO-original identifier (`OmoModelPadSchema` etc. are absent).
- [x] No OmO-original feature. The pad UI itself is not specified, only that the key is accepted.

## Behaviour check

- Shape: both objects are strict, four optional string labels, no defaults. Matches upstream.
- Placement: upstream adds the key to the config, layer, typed-harness and profile schemas. In
  mhc, `sections()` feeds both `typed_harness_block` and `profile_body`. The spec's "joins the
  key list of a typed harness block, which also makes it valid inside `profiles.<name>`" is
  therefore accurate.
- Validation: upstream's strict parse rejects the whole document. mhc is lenient (config.md
  §4.4), and the spec correctly maps an unknown key to the unknown-keys diagnostic and a
  non-string label to invalid-value pruning. Tests 1-7 agree with the text and with §4.4/§6.1-6.3.
- omo-cores-12 → D1: the same projection defaults (0 / 40960), confirmed in upstream
  `memory.ts`.
- The "checked, not cited here" paragraph points to task.md D6, sensitive-output.md D4,
  gateway.md and telemetry.md. Spot-checked: omo-senpi-task-b-39/40 are in task.md,
  omo-components-08 is in sensitive-output.md, omo-components-28 is in gateway.md.

## Fixes made

1. Rewrote the D2 "Meaning" paragraph in own words, removing the paraphrase of the SUL doc
   comment.

verdict: approved
