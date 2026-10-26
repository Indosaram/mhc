# skills

Native Senpi skills authored directly against the Senpi tool surface (not ported from Codex or the shared pool). `plugin/scripts/sync-skills.mjs` ships the native registry verbatim; shared-pool skills (ulw-execute, git-master, ...) get senpi overlays at sync time. Earned by score: 10 skill dirs with their own authoring contract.

## WHERE TO LOOK

| Skill | Role |
|-------|------|
| `ultrawork/` | Senpi-native ultrawork directive source; its body is embedded into `src/components/ultrawork/generated-directive.ts` by `plugin/scripts/embed-directive.mjs` (build fails on non-senpi harness tokens). |
| `ulw-plan/` | Read-only planning lifecycle: draft -> plan with explicit approval; `scripts/scaffold-plan.mjs` scaffolds guarded `.omo` artifacts. |
| `ulw-loop/` | Goal/QA lifecycle loop; component-owned native source, shipped verbatim. |
| `ulw-research/` | Claim-graph research orchestration: claims enter `claim-graph.md` as `verified-claims`; unsupported claims stay unresolved/refuted. |
| `adversarial-plan/` | Adversarial cross-critique planning: five critics argue for three rounds through a team, and only surviving points reach the planner that writes the plan; rounds and handoff must not be skipped. |
| `init-deep/` | Hierarchical AGENTS.md generation via a size-formula dag map-reduce (quick scanners -> high writers, ALWAYS-REDUCE); senpi-local override shadowing the shared-pool copy. |
| `dag-library/` | Store a dag definition once and re-run it by name; loads through `plugin/runtime/dag/library.js`. |
| `onboarding/` | Runs a new user's first conversation: tour of what mhc adds (naming only installed skills), optional one-time offers to import another agent's setup, read past sessions (explicit consent before `onboarding_understand`), estimate prompt-caching savings from local mhc logs with `scripts/savings-estimate.mjs`, and propose AGENTS.md only when `scripts/init-deep-gate.mjs` says the repo is eligible. Memory writes need a per-item yes. Clean-room, MIT; tests in `tests/`. |
| `give-me-tips/` | Deep explanations of `Tip:` lines: matches the quote against `mhc --list-tips` (falls back to the tips catalog source), reads the code behind the feature before explaining, works out per-user visibility from `settings.json` (`tips`, `quietStartup`, `tipsHistory`) and the scheduler's gates, and traces component notices that only look like tips. Clean-room, MIT; `tests/static-check.mjs`. |

## CONVENTIONS

- Every skill is a directory with a mandatory `SKILL.md` (YAML front matter, `name:` pinned by the onboarding validator). Supporting material splits into `references/` (loaded selectively by the parent skill) and `scripts/`.
- Orchestration contracts are prose plus exact templates: standalone node prompts with TASK/SCOPE/VERIFY/STOP WHEN, `category` routing, disjoint write scopes, bounded fan-in, final verification node. `dependsOn` is ordering only; data flows through files or eval-generated prompts.
- Markdown headings, serialized field names, and documented paths are contracts: `PLAN_SECTION_HEADERS`, `.omo/drafts/<slug>.md`, `.omo/plans/<slug>.md`, `claim-graph.md`, `verified-claims`, `team_run_id`.
- `scaffold-plan.mjs` targets Node and Bun, `node:*` built-ins only, deterministic output, refuses unsafe paths, symlinks, non-`.md` targets, and invalid slugs; preserves edited artifacts unless `--reset [--force]`.

## ANTI-PATTERNS

- init-deep: the main session NEVER reads raw chunk reports or node outputs (ALWAYS-REDUCE); scanners never write AGENTS.md; existing files are read before overwrite; generated managed blocks are never hand-edited.
- ulw-plan: no self-activation; no implementer dispatch (read-only lane); waits for explicit approval; never begins execution.
- ulw-loop: no sleep/timed polling for async state. ultrawork: no `tmux capture-pane` as visual evidence; paired cleanup required.
- Workers never edit outside the assigned scope and never commit.

## COMMANDS

```bash
(cd skills/onboarding && bash tests/static-check.sh && bash tests/savings-fixture.sh && bash tests/init-deep-gate.sh)
(cd skills/give-me-tips && node tests/static-check.mjs)
node skills/ulw-plan/scripts/scaffold-plan.mjs <slug> [--clear|--unclear] [--draft-only] [--review-required] [--reset [--force]]
```
