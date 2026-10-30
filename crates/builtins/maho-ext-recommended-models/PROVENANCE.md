# Provenance — maho-ext-recommended-models

## Unit model-selection-5152-d6 (omo v5.1.52 delta D6 prerequisite)

- Method: **C** (clean room). Changed from the D6 prerequisite paragraph of
  `docs/cleanroom/specs/model-selection.md`, spec commit ab572b379, only. No SUL, OmO or upstream
  source was opened.
- Change: the Kimi entry of `RECOMMENDED_DEFAULT_MODELS` is `kimi-k3.1-pro` at `max` (replacing
  `kimi-k3`), and `ranked_providers("kimi-k3.1-pro")` returns the Kimi ranking `kimi-coding`,
  `kimi-for-coding`, `moonshotai`, `opencode-go`. Nothing else the spec does not state was
  changed (`canonical_model_id` aliases are untouched).
- License: MIT.
