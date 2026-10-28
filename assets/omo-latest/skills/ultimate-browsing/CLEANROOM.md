# Clean-room record — `ultimate-browsing` skill

## Inputs used
- Spec: `/workspace/work/cleanroom/ultimate-browsing/spec.md`.
- MIT base: `/workspace/work/cleanroom/ultimate-browsing/base/` — lazycodex commit `75b017473c71d669dd9aed0a10e950e70724b283` (MIT, see `ATTRIBUTION.md`).

## Statement
No file under `/workspace/work/mhcg/assets/omo-latest/` or `/workspace/work/mhc/assets/` (SUL-1.0 text) was opened, read, grepped or diffed. The routing tree, description and section text were written fresh from the spec.

## Scope notes
- Only `SKILL.md` and `engine/AGENTS.md` differ from the base. Engine code, YAML, templates, tests (fixtures untouched, incl. trailing-newline ones), `scripts/*` and `references/**` are byte-identical to the base.
- No Codex-only harness sections, `lazycodex-*` roles or `multi_agent_v1` in the base. Three "Codex" words in `engine/validators.py` / `engine/waf_detector.py` are historical code comments in engine files the spec says are identical; left as is. A subagent mention in new text uses mhc's `task` / `team_*` / `workflow`.
- The owned engine is referred to in new text as "the owned browser engine (bundled with the `browser` skill)". Pre-existing base lines (e.g. `~/.local/state/omo-cookies`, the `loadOmowright` code block) were kept.

## Requirement → change checklist
| Req | Change |
|---|---|
| 1.1 | Frontmatter description rewritten: JS rendering, clicks/forms, screenshots, persistent login, WAF/bot-blocked hosts (native readers + stealth Chrome), browsing lane of multi-source research with screenshot provenance; excludes plain search and unblocked static fetches only. |
| 2.1 | Intro rewritten to "everything a plain fetch cannot finish"; delegation sentence to `browser` removed; cheapest-tier-first kept. |
| 2.2 | Phase 0 tree rebuilt: "user's browser can open" branch removed; Tier 1 routes for URL extraction and article/blog/Reddit/HN/arXiv added; blocked/403 and yt-dlp kept; archive-snapshot route kept (optional per spec); Tier 1.5 unchanged; Tier 2 routes = empty/partial, interaction, screenshot/render/video, QA/dogfooding, each 2a→2b; login-spanning/own-account → 2b directly; web-search exclusion at the bottom. |
| 2.3 | Tier 1 "When" lists content extraction first; escalation rule adds persistent login. |
| 3.1 | Tier 2 "When" = any real interaction or Tier 1/1.5 failure; blocked-host restriction and handoff sentence removed. Added a pointer to the `browser` skill's Step 0 when `OMO_BROWSER_ENGINE` is set. |
| 3.2 | Tier 2b heading/trigger → logged-in pages generally; procedure text unchanged. |
| 4.1 | AGENTS.md provenance row label "Import commit" → "Vendoring commit". |
| (extra) | Anti-patterns: one line on returning URL + screenshot provenance when used as a research lane via `task` / `team_*` / `workflow`. |

## Acceptance checks
- Routing: JS-only page, screenshot, form fill, QA web app → Tier 2 (this skill); HN thread → Tier 1. ✔
- Simple search routes away (tree bottom); unblocked static fetch excluded in the description. ✔
- Interaction/screenshot/QA show 2a → 2b; login/account → 2b directly. ✔
- `python -m pytest engine/tests`: **25 passed, 18 subtests passed**. ✔
- No omo/ultrawork/ulw/Sisyphus names in new text. ✔
