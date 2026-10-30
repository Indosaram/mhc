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
