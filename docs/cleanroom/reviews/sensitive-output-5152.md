# Leak-check record: sensitive-output, delta omo v5.1.52

Scope: docs/cleanroom/specs/sensitive-output.md (new in spec commit dbf5abaa). Reviewer: a
separate session, not the spec writer. Reviewer input only.

Compared against: SUL upstream omo v5.1.52
`packages/omo-senpi/src/components/memory/kibitzer/sensitive-output{,-shapes,-lexer}.ts`,
`packages/omo-senpi/src/components/comment-checker/resolver.ts`,
crates/omo/components/maho-omo-comment-checker; MIT senpi v2026.10.27-2
(`~/.cache/sync-5152/senpi-target`, MIT licence) `packages/coding-agent/src/core/sensitive-output.ts`
and `packages/ai/src/utils/secret-{redaction,classifier,lexer}.ts`; MIT
`crates/maho-core/src/sensitive_output.rs` on branch `sync5152/omo-memory` (commit b0c3883a);
MIT `crates/maho-comment-checker/src/binary.rs`; census omo-memory-30, -31, omo-components-08.

## Is the spec still needed?

No clean-room implementation is needed for D1/D2. The SUL kibitzer files are themselves a
mirror of MIT senpi `core/sensitive-output.ts` and senpi-ai `secret-*` (senpi#4283, v2026.10.27-2),
and the other lane ported exactly those MIT sources into `crates/maho-core/src/sensitive_output.rs`
(commit b0c3883a "linear env-assignment and credential-shape redaction (senpi v2026.10.27-2, omo
v5.1.52)", parity row in `crates/maho-core/parity.d/21.md` / `9571.md`). That port has the linear
`ENV_ASSIGNMENT` match with the once-per-name word test on `name[1..]`, the bearer rules, and the
token/delimiter credential-shape pass including percent-escape and HTML-entity reading. The MIT
side therefore already satisfies omo-memory-30/31. The spec is kept as a decision record and
acceptance description; a status paragraph now says so and states that the MIT port wins where
they differ (the spec omits the percent/entity decoding). D3 (comment checker) remains a valid
N-A record.

## Checklist

- [x] No fenced code block from the SUL tree (no fenced blocks).
- [x] No sentence translates a SUL comment **that is not also MIT text**. Several D1 sentences
      ("one left-to-right pass ... alternating tokens and delimiters", "a known key ends at its
      alphabet's boundary, not at the end of a URL or file name") track comments in
      `sensitive-output-shapes.ts`; those comments are copied verbatim from the MIT senpi-ai
      files (`secret-redaction.ts` line 3, `secret-classifier.ts` line 174), so the source of
      the wording is MIT and no SUL expression is carried. Accepted; noted here because the
      writer's "written from the census" provenance line is incomplete.
- [x] The D1 prefix table equals the SUL `PREFIX_RULES` row-for-row. Accepted as interface fact
      (public provider key formats) and, decisively, it is identical in MIT senpi-ai
      `secret-classifier.ts` lines 152-159. The env word list (`API_KEY|SECRET|TOKEN|PASSWORD|AUTH`)
      is also in MIT senpi `core/sensitive-output.ts`.
- [x] No OmO-original identifier proposed; function names are the existing MIT names
      (`redact_sensitive_output`, `redact_sensitive_token_values`, `redact_credential_shapes`).
      `Kibitzer` does not appear.
- [x] OmO-original features absent.

## Behaviour check

- D1 JWT rule was ambiguous ("first at least 8 characters after the dot-free lead"); the
  source counts the first segment including `eyJ`. Reworded.
- D2 test `A=1 TOKEN_X=2 -> second redacted` was wrong: the word test skips the name's first
  character (stated in D2 itself and in both SUL and MIT code), so `TOKEN_X` does not match.
  Replaced with `MY_TOKEN=2` (redacted) and `TOKEN_X=2` (unchanged).

## N-A check

- omo-components-08 (comment-checker `cacheDirName` parameter): N-A justified. MIT
  `maho-comment-checker` (`binary.rs`) resolves the executable from `COMMENT_CHECKER_BINARY`, the
  nearest installed npm package, then `PATH`; it has no download cache and so no cache directory
  name. The SUL change is an internal parameter whose default is unchanged; nothing user-visible.
  (The census "MIT target" note cites `resolver.rs:55`, which is the SUL crate, not the MIT one.)
- omo-memory-30/31: APPLIES, now marked satisfied by the MIT port. Coverage complete.

## Fixes applied to the delta

1. Status paragraph: D1/D2 already satisfied by MIT port b0c3883a; MIT port is authoritative; no
   clean-room implementation from this file.
2. D1 JWT segment-length wording clarified.
3. D2 test corrected (first-character skip).
4. Mapping table marks rows 30/31 as satisfied by the MIT port.

verdict: approved
