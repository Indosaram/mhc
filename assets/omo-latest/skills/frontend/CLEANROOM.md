# Clean-room record: `frontend` skill

## Inputs used
- Spec: `/workspace/work/cleanroom/frontend/spec.md` (behavioural delta only).
- Base: `/workspace/work/cleanroom/frontend/base/` = lazycodex commit `75b017473c71d669dd9aed0a10e950e70724b283` (MIT / Apache-2.0 per `ATTRIBUTION.md` and `LICENSE-Apache-2.0.txt`).
- **No SUL-1.0 material was read.** Nothing under `/workspace/work/mhcg/assets/omo-latest/` or `/workspace/work/mhc/assets/` was opened, read, grepped, or diffed. All new prose and code was written by the implementer from the spec.

## Files changed (everything else is byte-identical to base)
`SKILL.md` (rewritten), `references/design/README.md` (rewritten), `references/design/stylegallery.md`, `references/perfection/README.md`, `scripts/perfection/check-accent-stripes.ts` (header comment only), and generic-rename edits in `references/design/{clone-from-url,print-paged-media,image-to-code-skill}.md` and `references/designpowers/{README,UPSTREAM,lane-a-direction,lane-b-execution,lane-d-memory,orchestration,routing}.md`. No base file was deleted.

## Requirement -> change checklist
| Req | Kind | Status | Where |
|---|---|---|---|
| P1 | CHANGE | Done: craft-first bar; "quiet correct screen is a finish" and "ambience opt-in/off for internal tools" removed. Brief/subtract retained as aiming tools | SKILL.md intro; README "The bar" |
| P2 | CHANGE | Done: name refs + reason, read exactly those, reviewability framing; base "rows stack / small change reads few" guidance removed | SKILL.md "Phase 0 route" |
| P3 | ADD | Done | README "Where effort goes wrong" |
| P4 | ADD | Done (3D lit focal object, deep background, per-brand material, mechanisms from ambience-skill) | README "Focal object and atmosphere" |
| P5 | ADD | Done | SKILL.md intro; README |
| R1 | CHANGE | Done: mandatory-first README + stacked-reads table | SKILL.md |
| R2 | DROP | **Partly declined**: greenfield / new-screen / React-tooling / lazyweb / image-only rows removed (moved to workflow, README gate, quick routes); the small-change row is **kept** as the "nearest-sibling shortcut" | SKILL.md |
| R3 | CHANGE | Done: four rulesets (design L0/LA/LA-stacking/LB/React row; perfection table + 30-renders note + presets/3-5 runs/median/JSON; ui-ux-db three CLIs + domain and stack lines; designpowers replaces-nothing) | SKILL.md library |
| R4 | ADD | Done (14 rows) | SKILL.md "Quick routes" |
| R5 | ADD | Done (all seven axioms incl. don't-flatten and dual-reviewer done rule) | SKILL.md "Shared axioms" |
| W1 | CHANGE | Done: four branches, four parallel greenfield lanes (dispatched via mhc `task` / `team_*` / `workflow`), Research Log rule; README keeps the implicit-patterns extraction branch | SKILL.md; README Phase 0 |
| W2 | ADD | Done | SKILL.md; README Phase 0 |
| W3 | DROP | **Declined**: small-change / nearest-sibling rule kept; React gate keeps "a small change included" | SKILL.md; README |
| W4 | CHANGE | Done: direction committed inside greenfield branch, boldest defensible, generic-dark-SaaS self-check | SKILL.md; README |
| W5 | CHANGE | Done: both gates required, no exceptions | SKILL.md; README |
| B1 | DROP | **Declined**: per-screen brief kept incl. `Layout:` line and `before` captures | SKILL.md §1 |
| B2 | DROP | **Declined**: Build rules section kept (motion bullet now points to axioms; added a surface-depth bullet) | SKILL.md §2 |
| B3 | DROP | **Declined**: Subtract step kept (added: never strips depth from elements that keep a job) | SKILL.md §3 |
| L1 | DROP | **Declined**: full draft guard kept; accent-stripes lint now the named required lint, draft-guard lint also run | SKILL.md draft guard |
| L2 | DROP | **Declined (spec's orchestrator option)**: `check-draft-guard.ts`, `draft-guard/`, fixtures kept; docs name `check-accent-stripes.ts` as required and draft-guard as broader extra | SKILL.md, README checklist, perfection README |
| L3 | CHANGE | Done: `collect-files.ts` kept (not inlined, so identical rules hold); exit codes documented 0/1/2; base behaviour for unreadable target (stderr + exit 2) kept | check-accent-stripes.ts header; SKILL.md |
| D1 | CHANGE | Done: ambition-first step 2, mandatory high-craft A + B, never fall to taste-skill; "expensive", "pretty", "high-variance" added; Layer B as concrete reference note; taste-skill row warning in SKILL.md | README step 2; SKILL.md Layer A |
| D2 | CHANGE | Done: image-first default for expressive greenfield with no reference | README step 4 |
| D3 | DROP | **Declined**: redesign step keeps before/after cases and `before` capture; "never on greenfield, audit wasted" kept | README step 3 |
| D4 | CHANGE | Done: steps 1-10 each have triggers + action; step 8 names both silent CSS contracts; step 9 extraction list; step 10 extraction + retrofit checklist | README routing |
| D5 | CHANGE | Done: nine numbered stacking rules, same semantics | README "Stacking" |
| D6 | CHANGE | Done: full accent-stripe anti-pattern inline with exceptions | README anti-patterns |
| D7 | CHANGE | Done: 11 items incl. no-emoji, no generic-default drift, accent-stripes exit 0 as final item (draft-guard kept alongside, per L2 decline); loaded-files / unclear-question moved to Final notes | README checklist |
| D8 | ADD | Done (16 rows) | README "Quick lookup" |
| D9 | CHANGE | **Capability parts done**: flatness = defects for every brief; perceptual ramp / dimensional hero / signature moment; done = fresh-evidence pass **and** no minimum-effort surface. **DROP parts declined**: brief, `Layout:`, required-content, `before` still passed to QA; **round cap kept** (budget from visual-qa's own step), with leftover flatness/defects reported as open debt rather than done. "Operational brief done within budget" sentence removed | README Phase Final; SKILL.md §4 |
| D10 | ADD | Done | README "Final notes" |
| D11 | CHANGE | Done: own subsection | README "Open Design" |
| S1 | DROP/CHANGE | **DROP declined** (`Layout:` line, format, placement, QA use kept); **CHANGE adopted**: pattern decisions carried into DESIGN.md as named decisions, scroll owner + load-bearing constraints, pattern beside its problem | stylegallery.md |
| S2 | CHANGE | Done as superset: skip recorded in DESIGN.md Research Log; placeholder line + scroll owner still written (needed because `Layout:` line is kept) | stylegallery.md |
| S3 | ADD | Done: catalog first then pattern page; private-repo CLI/MCP rule; mechanics-vs-pattern guidance | stylegallery.md |

## Other edits requested by the orchestrator
- Harness/persona renames: `/ulw-plan` and `/ulw-execute` -> "the harness's plan/execute command"; "Prometheus" -> "the planner"; "OpenAgent" -> "harness"; "dual-oracle" -> "dual-reviewer"; `ulw-research`/`mass-ulw` -> generic wording (designpowers non-vendor docs, clone-from-url, print-paged-media).
- Codex-only wording in `image-to-code-skill.md` generalised to "the harness" (section headings renamed; rules unchanged).
- No `lazycodex-*` roles or `multi_agent_v1` references existed in base; none in out. Subagent fan-out for greenfield lanes names mhc `task` / `team_*` / `workflow`.
- Kept skill/command names: `/visual-qa`, `/review-work`, `open-design`, `programming`, `ultimate-browsing`, `visual-qa`. `.omo/` state paths and the `omowright` browser library name left as-is (tooling, not personas). `ATTRIBUTION.md` untouched (its "Codex/OpenCode frontmatter" line is a factual provenance note).
- All brand, Layer A, ui-ux-db, designpowers vendor, and perfection files kept.

## Acceptance checks (run with bun 1.x, Python 3.12)
1. `check-draft-guard` still appears (L2 declined), but only as a broader extra check; SKILL.md, README checklist, and perfection README all name `check-accent-stripes.ts` as the required lint. PASS (option branch).
2. `check-accent-stripes.ts fixtures/accent-stripes/positive` -> exit 1; `negative` -> exit 0 (same as base). Missing target -> exit 2, no args -> exit 2. Draft-guard fixtures: negative 0, positive 1. PASS.
3. SKILL.md has mandatory-first route, four ruleset sections, four-branch workflow with four parallel lanes + Research Log, quick-routes table, shared axioms incl. don't-flatten and dual-reviewer. PASS.
4. N/A by design: Brief/Build/Subtract, small-change rule, and `Layout:` line were all kept (DROPs declined).
5. README step 2 mandates high-craft A + B for expressive briefs and forbids falling to `taste-skill`; step 4 defaults to image-first for expressive greenfield. PASS.
6. Phase Final requires fresh-evidence pass **and** no minimum-effort surface. **Deviates by decision**: iteration budget kept (capped QA rounds); unresolved items are reported as open debt, not done.
7. Every router-named reference file exists (scripted check; only false hits were the project-side names `design-system.md` / `design-tokens.md`). 95 files in `references/design/`, ui-ux-db and designpowers vendor intact. PASS.
8. No `ulw`, Prometheus, oracle, lazycodex, multi_agent_v1, or Codex in SKILL.md, design/README, stylegallery, perfection README, designpowers docs, image-to-code. PASS.
