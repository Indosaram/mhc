# Leak-check record: model-selection, delta omo v5.1.52

Scope: `## Delta (omo v5.1.52, 2026-10-30)` of docs/cleanroom/specs/model-selection.md (spec
commit dbf5abaa, branch sync5152/spec). Reviewer: a separate session, not the spec writer.
Reviewer input only; not an implementer input.

Compared against: crates/omo/components/maho-omo-model-profile; upstream omo v5.1.52
`packages/omo-senpi/src/components/model-profile/{builtin-profiles,index,notice,request-auth}.ts`
and `packages/model-core/src/fable-55-launch.ts`; MIT senpi v2026.10.27-2
(`packages/ai/src/utils/oauth-refresh-error.ts`, `coding-agent/src/core/extensions/types.ts`
`AppendEntryOptions`); MIT crates `maho-model-select` (`profile.rs`, `probe.rs`, `ext.rs`),
`maho-ext-recommended-models`, `maho-ai`; census omo-components-18..21.

## Checklist

- [x] No fenced code block from the SUL tree. The delta has no fenced blocks.
- [x] No sentence translates or paraphrases a SUL comment, doc comment or prompt, **after the
      fixes below**. Before them, three passages tracked SUL comments:
      D1 "no resolver, cache state (cold or warm) or fallback walk can ever see the id" and
      "dropping must never prevent the profile list from loading" echo the data-layer-gate and
      launch-gate comments of `fable-55-launch.ts`; D2 "marked as start-up bookkeeping" with its
      rationale echoes `recordOutcome`'s comment in `index.ts`; D4 "walking the error's cause
      chain, at most 16 links, cycle-safe, not by type identity" restates the comment and the
      `MAX_CAUSE_LINKS = 16` constant of `request-auth.ts`. D3's next-step facts were close to
      the SUL strings of `notice.ts` (`unavailableStep`: "sign in ... or choose a model in the
      composer"); reworded to facts.
- [x] No table mirrors a SUL constant table row-for-row. The only values are the gated model id
      `claude-fable-5-5`, the accepted levels `xhigh`/`max` and the `model-profile:*` type ids:
      interface facts (model ids, enum values) also present in MIT `maho-ext-recommended-models`.
- [x] No OmO-original identifier proposed. Type ids stay neutral `model-profile:*` (SUL uses
      `omo-model-profile:*`); product prefix `mhc`; no `OmO Native` text. The SUL names
      `launchRungs`, `FABLE_55_LAUNCH`, `RefreshUnavailableKeep` do not appear.
- [x] OmO-original features absent. The persona lanes' display names ("Daily · Heavy",
      "Obsessive") and the `caution` text are not specified.

## Behaviour check against SUL (correctness of the delta)

- D1 matches `selectLaunchRungs`/`launchRungs`: flag off drops the gated rung; flag on keeps it
  only with an accepted level; rejection recorded and warned. The original D1 also said "with
  the flag off the chain is exactly the following rung alone", which copies the SUL
  `daily-heavy` row (a single Fable rung) and would delete the MIT chain's `claude-opus-5-5`
  rung that §6.1 sources from MIT data. Fixed: later rungs stay as §6.1 composes them.
  The release flag already exists in MIT `maho-ext-recommended-models` (`FABLE55_RELEASED`);
  the spec now points there instead of inventing a second flag.
- D2 matched SUL in "entry, not message", but the notification levels were wrong: SUL notifies
  a clean applied outcome **not at all**, an applied outcome with auth failures as `warning`,
  unavailable/unknown as `info` only for the unset default without auth failures, and the
  registry-dropped case as `warning`. The delta said "`info` for a clean applied outcome". Fixed
  with the full level list and two extra tests. The start-up-bookkeeping option is an MIT senpi
  v2026.10.27-2 contract (`AppendEntryOptions.startupBookkeeping`); mhc's `append_entry` lacks it,
  recorded as a prerequisite.
- D3 matches `notice.ts`/`index.ts` facts. The auth-failure variant of "unavailable" keeps its
  per-failure advice (SUL appends the next step only when no auth failure occurred); clarified.
  Test text mentioned the TUI, which never applies a profile (§6.7); fixed.
- D4: the original referred to the engine's `StreamError::oauth_refresh_unavailable` family as
  the marker. In mhc that variant is produced for **every** `ModelsErrorCode::OAuth` failure
  (`maho-ai/src/models.rs`, `maho-core/src/sdk.rs`), so following the delta literally would keep
  providers whose refresh was refused, contradicting the delta's own test 28d. Fixed: the delta
  now defines transient vs refused by behaviour and names the MIT senpi v2026.10.27-2
  `oauth-refresh-error.ts` classification as the prerequisite to port.

## N-A check

All four census rows routed to maho-model-select (omo-components-18, -19, -20, -21) are marked
APPLIES; none N-A. Correct: each changes what the user sees or which model a session starts on.
Coverage is complete (census rows naming `MIT: crates/maho-model-select` are exactly 18-21).

## Fixes applied to the delta

1. D1: removed SUL-comment echoes; tied the flag to MIT `maho-ext-recommended-models`; kept the
   §6.1 later rungs instead of "the following rung alone".
2. D2: rewrote without the SUL rationale; corrected notification levels; added the MIT senpi
   start-up-bookkeeping prerequisite; added tests.
3. D3: reworded the next-step facts away from SUL strings; scoped the next step to the
   no-auth-failure case; fixed the mode list in the test.
4. D4: rewrote by behaviour; dropped the SUL constant and comment; replaced the incorrect engine
   marker with the MIT senpi classification prerequisite; reworded test 28b.
5. Leak self-check: noted the reviewer fixes.

verdict: approved
