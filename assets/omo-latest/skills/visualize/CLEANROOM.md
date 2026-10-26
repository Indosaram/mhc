# Clean-room record: visualize

- **Implementer inputs:** `/workspace/work/cleanroom/visualize/spec.md` only, plus the Apache-2.0 `/workspace/work/cleanroom/frontend/base/scripts/perfection/check-accent-stripes.ts` (invoked, not copied).
- **Not read:** nothing under `/workspace/work/mhcg/assets/omo-latest/` or `/workspace/work/mhc/assets/` (SUL-1.0 text).
- **Authorship:** SKILL.md, checklists, scripts and example page written from scratch.
- **License:** MIT, (c) 2026 Indo Yoon (`LICENSE`).

## Decisions / drops (spec §10)
- Display is capability-based: host HTML display tool → preview tool → **file + `browser` skill (default in mhc)**. `show_html_page` / `html_preview` / `html_render` kept only as optional capability names.
- Host behaviours (theme injection, scripts-off-first, link confirmation) phrased as "if/may".
- `data-scientist` and `frontend` are soft references; accent-stripe script path is `$SKILL_DIR/../frontend/scripts/perfection/…`, skipped with a note if absent. `npx tsx` documented as fallback when `bun` is missing.
- Browser bridge tool not named (upstream-prefixed identifier); skill points to the `browser` skill.
- Added own helpers: `check-inline-page.mjs` (static rule check), `preview-local.py` (optional 4-view Playwright render), `build-example.mjs` (reference page generator).

## Files
- `SKILL.md`, `LICENSE`, `CLEANROOM.md`
- `scripts/check-inline-page.mjs`, `scripts/preview-local.py`, `scripts/build-example.mjs`
- `references/example-data.csv`, `references/example-chart.html`, `references/example-empty.html`
- `acceptance/*.png` — screenshots from the checks below

## Acceptance checklist (spec §9), run on `references/example-chart.html` and `example-empty.html`
| # | Check | Result |
|---|---|---|
| 1 | loads with network blocked; no external src/href | PASS (Chromium offline + request blocking: 0 blocked requests; grep clean) |
| 2 | figure visible with JS disabled | PASS (`--no-js` screenshots) |
| 3 | colours/fonts only `var(--…, fallback)` from list | PASS (`check-inline-page.mjs` clean; negative test catches 7 rule types) |
| 4 | accent-stripe lint exits 0 | PASS (`npx tsx check-accent-stripes.ts`, exit 0; bun not installed in sandbox) |
| 5 | dark/light × 720 px/390 px screenshots, no clipping | PASS (no horizontal overflow, 0 clipped, 0 console errors; screenshots inspected) |
| 6 | "how computed" block with actual aggregation | PASS (`data-role="method"` contains the formula used) |
| 7 | empty data → designed empty state | PASS (`example-empty.html`) |
| 8 | in mhc falls back to file + browser, no non-existent tool call | encoded in SKILL.md step 0; live mhc run not performed |

Note: Chromium was installed in the sandbox via apt for the local preview; the browser-skill preview path itself was not exercised.
