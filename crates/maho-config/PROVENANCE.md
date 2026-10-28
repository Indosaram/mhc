# Provenance — maho-config

- Method: **C** (clean room). Written from `docs/cleanroom/specs/config.md` and
  `docs/cleanroom/specs/config-keys.md` only, plus MIT code already in the tree
  (`maho-support`, `maho-core`, `maho-ext-api`, `maho-ext-config-reload`).
- Spec commit: 790aad31 (see `docs/cleanroom/provenance/phase1-env.txt` for the environment
  manifest of the implementing checkout).
- No SUL source (`crates/omo/**`, OmO/COMP checkouts, `assets/omo.schema.json`) was available
  in the implementing checkout.
- Decisions taken on the spec's open questions: the OpenCode import plan
  (`2026-07-opencode-config-unification`) is not implemented; `OCX_PROFILE` and
  `OPENCODE_CONFIG_DIR` are still read; the on-disk names (`~/.omo`, `omo.jsonc`, migration ids,
  lock and journal formats) and the hot-reload registration id `omo` are kept for compatibility.
- License: MIT.
