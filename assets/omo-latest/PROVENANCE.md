# `assets/omo-latest` — provenance note

The full L-IF-2 authoring receipt lives at **`authoring/assets.md`** (the required receipt path).
This file records only the overlay-side provenance.

| Item | Value |
| --- | --- |
| Overlay | `assets/omo-latest` (repository-owned latest skill assets) |
| Bytes taken from | `/home/indo/T9-Mac/maho-code-lanes/upstream-omo-audit-20261006` @ `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b` |
| Pinned OMO (unchanged) | `/home/indo/code/oh-my-openagent` @ `77f3067f157a4f88e6d8ed48b3a6c338654402ed` |
| Pinned senpi (unchanged) | `fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407` |
| Selection | the 25-name latest set (`manifest.json.selection` = `targetNames`) |
| Retired | `start-work` |
| Precedence | `packages/omo-senpi/skills` over `packages/shared-skills/skills` |
| Copy mechanism | byte-exact file copy (mode preserved), filtered by `skill-source-filter.mjs` semantics |
| Installed loader | `OMO_SENPI_SKILLS_ROOT` → `<install>/skills` (`manifest.json.loader`) |
| git-bash sibling | native binary name `omo-git-bash`; staged from the single `--git-bash-binary` flag (required by the selected `--skill-source latest` staging) |
| Browser engine runtime | `browser` ships a build-materialized engine runtime (`runtime/browser/omowright/{index.js,page-bundle.js,manifest.json}`) produced by `assets/omo-latest/stage-omowright-runtime.mjs` from `omowright` `github:code-yeongyu/omowright#293ca5002cbd4c8b0c104c5934683385ef7e0d3a`; staged from the single `--omowright-runtime` flag (required by the selected `--skill-source latest` staging). Packaging of upstream skill content only — no desktop/browser implementation, engine authorization or extension bridge. |

`PINS.md` is not modified by this overlay or by either staging mode.

## License

omo-derived assets ship under `LicenseRef-SUL-1.0` (`LICENSES/SUL-1.0.md`).
`skills/browser/ATTRIBUTION.md` and the other per-skill `ATTRIBUTION.md` files carry their own
third-party notices; no third-party license text was stripped or rewritten.

The browser engine runtime is materialized from the pinned `omowright` git dependency and carries
its own `manifest.json` (version, source digest, per-file sha256, immutable commit). It is not
committed (`runtime/.gitignore`), and its third-party terms are the upstream package's; nothing in
this overlay relicenses it.
