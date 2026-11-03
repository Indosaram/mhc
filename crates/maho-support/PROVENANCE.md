# Provenance — maho-support

- Method: **C** (clean room). Written from `docs/cleanroom/specs/utils.md` only.
- Spec commit: see `docs/cleanroom/provenance/phase1-env.txt` (base commit recorded there).
- No SUL source (`crates/omo/**` or OmO/COMP checkouts) was available in the implementing
  checkout; the environment manifest lists every file that was present.
- License: MIT.
- Delta (omo v5.1.52): `canonicalize_lenient` / `contains_path` follow symlinks before `..`
  (utils.md "Delta" D1), written from that delta section only; tests `delta_d1_*` in
  `tests/support.rs` follow its numbered tests 1-7.
