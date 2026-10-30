# Leak-check record: onboarding, delta omo v5.1.52

Scope: `## Delta (omo v5.1.52, 2026-10-30)` of docs/cleanroom/specs/onboarding.md (spec commit
dbf5abaa). Reviewer: a separate session, not the spec writer. Reviewer input only.

Compared against: crates/omo/components/maho-omo-onboarding; upstream omo v5.1.52
`packages/omo-senpi/src/components/onboarding/{component,session-history-runtime-loader}.ts`;
MIT `crates/maho-onboarding/src/*` and onboarding.md §1-§6; census omo-components-07, -02.

## Checklist

- [x] No fenced code block from the SUL tree (no fenced blocks).
- [x] No sentence translates or paraphrases a SUL comment, message or prompt. The tour prompt
      text of `component.ts` is not reproduced.
- [x] No table mirrors a SUL constant table.
- [x] No OmO-original identifier proposed.
- [x] OmO-original features absent (the guided tour is only named as absent).

## N-A check

- omo-components-07 (session-history loader wording): N-A justified, but the original reason
  ("the in-process MIT session-history scan") was wrong: MIT onboarding has no session-history
  scan at all (`grep history crates/maho-onboarding/src` finds nothing; onboarding.md §1 has only
  the `provider` and `model` steps). Reason corrected; the shared reinstall-step rule this row
  also carried is covered by feedback.md D4, so nothing user-visible is lost.
- omo-components-02, onboarding half (tour path through the bundled-skill resolver): N-A
  justified; MIT onboarding has no tour step and resolves no bundled skill. The skill-command
  half is APPLIES in skills.md D2.

Coverage complete (row 07 is the only row routed to `crates/maho-onboarding`, plus the shared
row 02).

## Fixes applied to the delta

1. Intro and row-07 reason corrected: MIT onboarding has no session-history scan, not an
   in-process one; pointer to feedback.md D4 added.

verdict: approved
