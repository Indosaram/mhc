# Leak-check record: gateway, delta omo v5.1.52

Scope: `## Delta (omo v5.1.52, 2026-10-30)` of docs/cleanroom/specs/gateway.md (spec commit
dbf5abaa). Reviewer: a separate session, not the spec writer. Reviewer input only.

Compared against: crates/omo/components/maho-omo-gateway (`src/thread/attachment_fence.rs`);
upstream omo v5.1.52 `packages/omo-senpi/src/components/thread/attachment-fence.ts`; MIT
`crates/maho-gateway/src/*` and gateway.md §1-§9; census omo-components-28, -29.

## Checklist

- [x] No fenced code block from the SUL tree (no fenced blocks).
- [x] No sentence translates or paraphrases a SUL comment. "deny-by-default attachment fence"
      is the census row's own summary of the behaviour, not the SUL comment wording.
- [x] No table mirrors a SUL constant table; the `UNFENCED` command list is not reproduced.
- [x] No OmO-original identifier proposed. `unknown_session` and `session_identity_mismatch`
      appear only as quoted refusal codes in the N-A reason (not proposed identifiers).
- [x] OmO-original features absent.

## N-A check

Both rows (28, 29) are N-A. Justified against the MIT design:

- Row 28 changes which host-socket session commands require an attach before sending. MIT
  `maho-gateway` has no socket client and no attach step: delivery is a file store plus
  per-session inbox drained by the receiving session (gateway.md §2, §5; `crates/maho-gateway/src`
  is `store.rs`, `message.rs`, `tools.rs`, `cli.rs`, `extension.rs`, with no RPC client). The
  fence protects a mechanism that does not exist there.
- Row 29 keeps typed attach refusals on that same socket path. The user-observable counterpart
  in MIT (a send to a session that is gone or mismatched) is already specified as "no live
  session" / "ambiguous" target errors (gateway.md §4). Nothing user-visible is lost.

The rows were PORT-NEEDED in the census because the census routed the whole SUL gateway crate
to `crates/maho-gateway`; the MIT crate's different transport makes them N-A, which the brief
allows. Coverage complete.

## Fixes applied to the delta

None needed.

verdict: approved
