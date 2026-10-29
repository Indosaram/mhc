# Leak-check record: question-gates

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/question-gates.md. Reviewer input only; not an implementer input.

## Leak self-check

- Fenced code blocks: none.
- Written from observed behavior; no sentence translates a SUL comment, doc comment or prompt;
  tool parameter descriptions are not quoted.
- The header list, label prefixes and tool/parameter names are marked interface facts (callers
  and planners must use the same strings); no other SUL table is mirrored.
- Proposed identifiers (`maho-approval-gates`, `approval-gates`) are neutral; no OmO-original
  names are used.
- No OmO-original feature (maximum-orchestration mode, fan-out mode, personas, plan state file)
  is specified.

## Review (leak check)

Reviewer session: separate from the spec writer; compared against
`crates/omo/components/maho-omo-question-gates` (`gate_rule.rs`, `index.rs`, inline tests).

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (none at all);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt;
- [x] no list or table mirrors a SUL constant table row-for-row except interface facts (tool
      names, parameter names, the gate header values, the label prefixes);
- [x] no proposed identifier is an OmO-original name;
- [x] OmO-original features are absent.

Fixes:
- §2.1 trimming: "including BOM, U+0085 not trimmed" restated the SUL code comment on its trim
  set; now stated as the external reference behaviour (JavaScript `trim`) with the two test
  consequences.
- §2.1 closing paragraphs: "question text and option descriptions are never inspected" echoed
  the SUL doc comment ("never reads prose"), the partial-header/label examples were the SUL test
  vectors, and "recommended headers planners should use ... fits the 12-character limit"
  paraphrased the SUL constant's doc comment (which names an OmO planner); rewritten as a neutral
  rule statement and an interface-fact note.
- §5 tests 1-7, 9, 10, 13: header/label/malformed vectors were copied from the SUL inline tests
  (`Next`/`Approve`, `Next step`/`Approve plan`, `Approve, skip review`, `Review flow`, `todo`,
  `Authorization`, ...); replaced by new vectors covering the same rules.
