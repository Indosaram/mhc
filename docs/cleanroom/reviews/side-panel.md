# Leak-check record: side-panel

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/side-panel.md. Reviewer input only; not an implementer input.

## Leak self-check

- No SUL code blocks; the only literal commands are `git` invocations (interface behavior).
- Prose written from observed behavior and config facts; no SUL comments, doc text or prompt
  text reproduced. User-facing messages are described by the facts they carry.
- Constant values (32/80/60 widths, caps 12/24/6/8/5000/50, 1 s/2 s/5 s timings) are stated as
  behavioral requirements; config keys and the `--omo-side-panel` flag and `/side-panel-diff`
  command are marked interface facts.
- Proposed names are neutral (`mhc-panel:` scheme, `--side-panel` alias); internal widget keys,
  type names and on-disk marker names of the SUL are not carried over.
- OmO-original features (the background reviewer wake log shown in the SUL memory section,
  plan-continuation state) are not specified.

## Review (leak check)

Reviewer: a separate session from the spec writer. Compared against `crates/omo/components/maho-omo-side-panel`
(comments, strings, `constants.rs`, unit tests).

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt
- [x] no list or table mirrors a SUL constant table row-for-row except marked interface facts
- [x] no proposed identifier is an OmO-original name
- [x] OmO-original features are absent

Fixes:
- §1: dropped the "persistent switch" wording, which echoed the SUL flag description string.
- Test 8: replaced the SUL unit-test vectors (width 5, `"a"`, `"abcdefghij"` → `"abcd…"`) with new values.
- Test 17: replaced the SUL git-parse fixture (`new`/`old`, 2/3 + 5/1 → `+7/-4`) with a new scenario.
- Test 21: replaced the SUL test's literal list of rejected URLs with categories.
- Test 24: replaced the SUL popup test vectors (24 rows → 17/13, 40 rows → 27, kitty Esc) with new values.
