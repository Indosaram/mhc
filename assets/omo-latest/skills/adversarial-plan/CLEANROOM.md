# Clean-room record: adversarial-plan

- **Implementer inputs:** `/workspace/work/cleanroom/hyperplan/spec.md` only, plus read-only confirmation that `team_create`, `team_delete`, `team_status`, `team_list`, `task_send` exist in mhc (`/workspace/work/mhcg/crates/omo/components/maho-omo-task/src/tools.rs`) and that `ulw-plan` is the shipped planning skill (`maho-cli/src/senpi_install.rs` lists `skills/ulw-plan/SKILL.md`).
- **Not read:** nothing under `/workspace/work/mhcg/assets/omo-latest/` or `/workspace/work/mhc/assets/` (SUL-1.0 text).
- **Authorship:** every prompt, template, table and sentence written fresh. Critics named by function (`minimalist`, `breakage`, `evidence`, `structure`, `alternatives`); no upstream persona names.
- **License:** MIT, (c) 2026 Indo Yoon (`LICENSE`).

## Decisions / drops (spec §10)
- Published as `adversarial-plan`; planner referred to as "the plan skill" (`ulw-plan` given as the current mhc example, configurable).
- All-caps mode banner replaced by one neutral line.
- Precondition error rewritten neutral; no product names or CLI flags.
- Category keys kept (live mhc identifiers), with a note that projects can remap them.
- Planner agent: "via `subagent_type` if a planner agent is configured" — no config-file reference.
- Team spec directory path not mentioned (only inline spec is used).

## Files
- `SKILL.md`, `LICENSE`, `CLEANROOM.md`, `tests/static-check.mjs` (`node tests/static-check.mjs`)

## Acceptance checklist (spec §9)
Static check of SKILL.md text: 18/18 PASS (each §9 item is encoded as an explicit rule; grep for upstream skill/persona/product names returns nothing).
| # | Check | Result |
|---|---|---|
| 1 | child session stops before `team_create` | encoded; live not run |
| 2 | team tools absent → explanatory stop | encoded; live not run |
| 3 | one `team_create` + ≤1 retry without evidence on `deep-low` failure | encoded; live not run |
| 4 | 3 gated rounds; round 3 = only own attacks | encoded; live not run |
| 5 | bundle: no conceded, 4 groups + provenance | encoded (template); live not run |
| 6 | one foreground `task`, plan skill in `load_skills`, full bundle + obligations | encoded; live not run |
| 7 | plan shown unchanged + one provenance line | encoded; live not run |
| 8 | `team_delete force:true` after planner; `team_list` empty | encoded; live not run |
| 9 | no upstream names/prompt text | PASS (grep) |

Behavioural checks 1–8 require a live mhc session with team tools; not executed here.
