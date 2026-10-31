# Leak-check record: feedback, delta omo v5.1.52

Scope: `## Delta (omo v5.1.52, 2026-10-30)` of docs/cleanroom/specs/feedback.md (spec commit
dbf5abaa). Reviewer: a separate session, not the spec writer. Reviewer input only.

Compared against: crates/omo/components/maho-omo-feedback (`index.rs`, `submit.rs`,
`runtime.rs`); upstream omo v5.1.52 `packages/omo-senpi/src/components/feedback/{submit,runtime,
index,runtime-loader}.ts` and `components/reinstall/reinstall-step.ts`; MIT `crates/maho-feedback`
and `crates/maho-cli/src/bin/mhc_launcher.rs`; census omo-components-06, -15, -16, -17.

## Checklist

- [x] No fenced code block from the SUL tree (no fenced blocks).
- [x] No sentence translates or paraphrases a SUL comment or message, **after the fixes below**.
      D3 "a refresh would spend the single-use refresh token" restated the doc comment of
      `cliAccessToken` in `runtime.ts`; removed. D2's message list ("says to update the client",
      "says to retry later", `Request ID: <id>`) tracked the SUL strings of `resultMessage`
      ("Update OmO and try again", "Try again later", " Request ID: ") one-to-one; restated as
      facts with the label left to the implementer. D4 fixed `reinstall mhc with: <command>`,
      the exact shape of SUL `reinstallStep()`; now stated as a requirement (name the command)
      without the SUL string.
- [x] The D2 code table mirrors the SUL `SERVICE_REFUSALS` table row-for-row. Accepted as an
      interface fact: the left column is the intake service's closed public error-code list
      (wire values the client must interpret), the right column is the spec's own refusal enum
      already defined in §5.2; the delta marks it as a service contract. Grouping rows by reason
      (rather than one row per code) is the writer's own layout.
- [x] No OmO-original identifier proposed. `feedback.report_rejected` is not reproduced ("event
      name free"); `x-omo-content-sha256` and `OMO_*` env names are wire/launcher interface
      facts.
- [x] OmO-original features absent.

## Behaviour check against SUL

- D1 matches `submit.ts` (version `1.0.0`; length and SHA-256 over the exact body bytes).
- D2 matches `refusalFor` including precedence (known code first, then 401/403, 413, 429, >=500,
  else rejected/unknown), the request-id filter (1-128 of `A-Za-z0-9_-` and redactor-clean), and
  the 409 `conflict` already-sent rule checked first.
- D3: the original said the token is attached to `prepare`/`submit`; SUL reads the store only in
  the command flow before `submit` (prepare computes `signedIn` from a caller token). Fixed to
  "for `submit`". Added the "less than 30 s left" case and "warning before sending" (SUL notifies
  before calling submit).
- D4: the original described a "feedback component cannot be loaded" message. MIT `maho-feedback`
  is linked into the binary and has no loader; the SUL message only exists because omo ships the
  runtime as a sidecar. The user-observable part that does apply is the reinstall-step rule and
  `OMO_REINSTALL_COMMAND`, which the mhc launcher's missing-installation messages
  (`mhc_launcher.rs`, `REINSTALL`) can honour. Rewritten accordingly; precedence between
  `MAHO_REINSTALL_COMMAND` and the legacy name made testable.

## N-A check

All four rows (06, 15, 16, 17) are APPLIES; none N-A. Coverage is complete. Row 06 is kept as
APPLIES (user-visible env interface) with its reason narrowed to the product's
missing-installation messages, since MIT has no feedback loader.

## Fixes applied to the delta

1. D2 messages restated as facts, not SUL strings.
2. D3: removed the doc-comment rationale; token decided for `submit` only; warning before send;
   added the near-expiry test.
3. D4 rewritten for the MIT design (no feedback loader; reinstall-step rule for the product's
   missing-installation messages); tests extended.
4. Mapping-table reason for omo-components-06 updated.

verdict: approved
