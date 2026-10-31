# Leak-check record: model-selection D6 (omo v5.1.52)

Scope: `### D6` of docs/cleanroom/specs/model-selection.md and its D5 row `omo-cores-02`
(spec commit e256bd951, branch sync5152/spec). Reviewer: a separate session, not the spec
writer. Reviewer input only; not an implementer input.

Compared against: upstream omo v5.1.52
`packages/omo-senpi/src/components/model-profile/builtin-profiles.ts`,
`packages/model-core/src/latest-models.ts`, `packages/model-core/src/provider-model-id-transform.ts`;
MIT senpi target `coding-agent/.../builtin/recommended-models/index.ts`; mhc MIT crates
`maho-model-select` (`profile.rs`, `transforms.rs`) and `maho-ext-recommended-models`; census
rows omo-cores-02 and senpi-builtins-b-05.

Verdict: **approved after fixes** (fixes committed in this review).

## Checklist (README rules 1-6)

- [x] No fenced code from SUL; D6 has no fenced blocks.
- [x] No translated SUL comment or prompt, after one fix: "previous-generation id" echoed the
      `latest-models.ts` doc comment on `kimiK3`; reduced to the bare id. The invariant text
      ("never matches through an unlisted provider / gateway") restates §6.4's existing contract,
      not the `builtin-profiles.ts` header comment's wording.
- [x] No row-for-row table. Values used are model ids, provider ids and one level name, all
      interface facts that also appear in MIT senpi recommended-models data.
- [x] No OmO-original identifier (`LATEST_MODEL_BY_FAMILY`, `KIMI_PROVIDERS`, `kimiK3` absent).
- [x] No OmO-original feature. Messages: none mandated.
- [x] Tests are new given/when/then cases.

## Behaviour check

- Upstream `daily-normal` rung 2 is `LATEST_MODEL_BY_FAMILY.kimi` = `kimi-k3.1-pro` on
  `kimi-coding`, `kimi-for-coding`, `moonshotai`, `opencode-go` at `max`. MIT senpi data ranks
  `kimi-k3.1-pro` with exactly those providers in that order at `max`. D6 is correct.
- Non-empty-providers invariant: every upstream builtin rung (all four lanes, including the
  launch-gated Fable rung and the `gpt-5.6-sol` tail) lists a non-empty provider set. In mhc,
  `ranked_providers` returns an empty list for unranked ids, so the invariant test catches a
  stale id. Holds once the data is current.
- Mapping `omo-cores-02 -> D6`: correct; that row is the `LATEST_MODEL_BY_FAMILY.kimi` move and
  names `maho-model-select/src/profile.rs:100` as the MIT site. Reason column reworded to name
  the data prerequisite instead of internal lane jargon.

## Fixes made

1. Missing prerequisite: mhc's `maho-ext-recommended-models` still lists `kimi-k3` (census
   senpi-builtins-b-05). As written, test 1 would fail against the current data and the
   "only unranked rung is `kimi-k3`" claim was false for mhc today. Added a Prerequisite
   paragraph and qualified the claim with "once the recommended data is current".
2. Missing transform: upstream maps `kimi-k3.1-pro` -> `k3.1-pro` (and the `-highspeed` variant)
   on `kimi-coding` / `kimi-for-coding`; spec §4 and mhc `transforms.rs` only know `kimi-k3`.
   Without it the rung cannot match on its two top-ranked providers. Added the compatibility
   fact and test 5.
3. Test 3 was open-ended ("moves to the next rung, or reports unavailable"). Made it exact:
   only `acme-gateway/kimi-k3.1-pro` available -> `Unavailable` listing all three rungs.
4. Added test 4 (positive match through `kimi-coding/k3.1-pro`, reasoning `max`, Claude rung
   skipped), implementable with `resolve_profile` / `ProfileResolution::Resolved`.
5. §6 reference for the empty-providers meaning narrowed to §6.4.

## Notes (out of D6 scope, not fixed)

- Upstream omo GLM rung providers (`zai`, `zai-coding-cn`, `opencode-go`) differ from MIT
  senpi's GLM ranking (`zai-coding-plan`, `opencode-go`). §6.1 sources from MIT data, so mhc
  follows senpi; flagged for the maintainer only.
