# Leak-check record: feedback

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/feedback.md. Reviewer input only; not an implementer input.

## Leak self-check

- [x] No fenced code from the SUL tree; no code blocks at all.
- [x] No sentence translates a SUL comment, doc comment or user-facing message; all notification
      wording is left to the implementer and only required facts are listed.
- [x] Lists that mirror SUL constants are interface facts only (section ids, RPC names, result
      and refusal values, wire fields, env names, file names, id patterns). The log-field
      allowlist and the secret-word list are described by category, not reproduced row by row.
- [x] No proposed identifier is an OmO-original name (no type or function names are proposed);
      `senpi` (client wire field) and `omo-senpi` (state directory) appear only as interface facts.
- [x] No OmO-original feature is specified; the anonymous install/session hashes are dropped
      for the MIT profile.

## Review (leak check)

Reviewer: separate session from the spec writer; compared against the SUL component
`maho-omo-feedback` (doc comments, message strings, constant tables, unit tests).

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (no fenced blocks at all);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt (after
      the fixes below);
- [x] no list or table mirrors a SUL constant table row-for-row except interface facts (after
      the fixes below);
- [x] no proposed identifier is an OmO-original name;
- [x] OmO-original features are absent.

Fixes:

- §1 overview: rewrote wording that echoed the SUL module doc ("the exact report that leaves the
  machine").
- §3: "must be idempotent" (doc-comment wording) restated as a property.
- §4.2 config: the secret-key word list and the size/limit exemption list reproduced the SUL
  regex alternatives row-for-row; replaced by a required minimum category set plus examples,
  leaving the exact set to the implementer.
- §4.2 logs: removed phrases taken from the SUL doc comment ("the engine's own logs",
  "content-free fields").
- §4.2 session: removed the doc-comment phrase "custom extension entries never appear".
- §4.3 email: restated the two-halves check without the SUL comment's wording.
- §4.4 and §5.2 step 1: reworded sentences that paraphrased SUL doc comments on retryable
  refusals and "goes exactly as the draft shows it".
- §6 step 5: dropped the hint list that summarised the SUL refusal messages one-to-one.
- §8 tests 3, 4, 5, 6, 7: the given values reproduced SUL unit-test fixtures (README rule 5);
  replaced with new inputs that test the same behaviour.
