# Provenance — maho-model-select

- Method: **C** (clean room). Written from `docs/cleanroom/specs/model-selection.md` only, plus
  MIT code already in the tree (`maho-config`, `maho-ext-api`, `maho-ext-recommended-models`).
- Spec commit: 790aad31 (environment manifest: `docs/cleanroom/provenance/phase1-env.txt`).
- No SUL source (`crates/omo/**`, OmO/COMP checkouts) was available in the implementing checkout.
- Built-in model-profile chains are composed from `maho-ext-recommended-models` (model ids,
  provider rankings, default thinking levels), not from any SUL table.
- Decisions: the error classes of spec §7.2 are deferred to the Phase 2 consumers; the
  provider-specific startup advisory (Q6) is not built; the profile stays skipped in the TUI
  (Q2); message type ids are the neutral `model-profile:*` (Q5).
- License: MIT.

## Unit model-selection-5152 (omo v5.1.52 delta)

- Method: **C** (clean room). Written from the `## Delta (omo v5.1.52, 2026-10-30)` section of
  `docs/cleanroom/specs/model-selection.md` only, spec commit da2b172b, plus MIT code already in
  this tree. No SUL source was opened.
- D1: launch gate (`LAUNCH_GATED_MODEL`, `GATED_ACCEPTED_LEVELS`, `gate_rungs`,
  `build_builtin_profiles`) keyed on the existing MIT flag
  `maho_ext_recommended_models::FABLE55_RELEASED`; `daily-heavy` = gated rung (xhigh), the
  released same-family model from `recommended_fable_model` (xhigh), then the top general model
  (high). `maho-ext-recommended-models` is unchanged (the spec only asks to read its flag/data).
- D2: outcomes go to a session entry (`OutcomeHost::append_entry`) plus a log line and at most
  one notification; no custom message is sent to the model any more.
- D3: mode-dependent next step and the registry-dropped notice.
- D4: `ProbeError` gains `refresh: Option<RefreshFailure>` and `cause` (wrapped errors);
  transient refresh keeps the candidate (`probe_and_resolve_detailed`, `KeptCandidate`).
- Cross-lane prerequisites (not implemented here): the senpi `appendEntry` start-up-bookkeeping
  option on mhc `append_entry`; the transient/refused OAuth refresh split in `maho-ai` /
  `ExtensionFailure` so `RegistryProbe` can fill `ProbeError::refresh`.

## Unit model-selection-5152-d6 (omo v5.1.52 delta D6)

- Method: **C** (clean room). Written from the `### D6` subsection (with its prerequisite
  paragraph), the §4 compatibility facts and D6 tests 1-5 of
  `docs/cleanroom/specs/model-selection.md`, spec commit ab572b379, plus MIT code already in this
  tree (`maho-ext-recommended-models`). No SUL, OmO or upstream source was opened.
- D6: the `daily-normal` Kimi rung is `kimi-k3.1-pro`, taking providers and level from
  `maho-ext-recommended-models` through the existing `recommended_rung`. §4 Kimi coding transform
  gains `kimi-k3.1-pro` → `k3.1-pro` and `kimi-k3.1-pro-highspeed` → `k3.1-pro-highspeed`.
- Tests (`tests/delta_5152_d6.rs`) were written from D6 tests 1-5; test 2 checks the
  non-empty-providers invariant for every built-in rung in both launch-gate states.
