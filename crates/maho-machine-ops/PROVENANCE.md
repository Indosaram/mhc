# Provenance — maho-machine-ops

- Upstream source: `senpi` (`packages/machine-ops`) at tag `v2026.11.19-5` (`99a26322f4a81d5ed95f0fd9f292938a6afbd8ff`), selectively ported ahead of the `PINS.md` pin (`59d58cee5`).
- Method: File-for-file Rust port preserving module layout, types, limits, policy hooks, and conformance suite.
- License: MIT.
- Boundary: Engine-free host operation layer; depends only on `maho-pty`, `vt100`, and standard/workspace utility crates, never on `maho-core`, `maho-agent`, `maho-ai`, or `maho-tools`.
