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
