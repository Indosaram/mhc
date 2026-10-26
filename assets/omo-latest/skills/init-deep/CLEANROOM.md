# Clean-room record: init-deep

## Inputs used
- Functional spec: `/workspace/work/cleanroom/init-deep/spec.md` (clean-room side A).
- MIT base: `/workspace/work/cleanroom/init-deep/base/SKILL.md`, from lazycodex commit `75b017473c71d669dd9aed0a10e950e70724b283` (MIT).
- mhc Rust sources under `/workspace/work/mhcg/crates` (to pick real tool names and paths only): `workflow` tool (`maho-omo-task/src/dag_tool.rs`: actions start/wait/snapshot/retry/send, `definition{key,name,nodes}`, node fields `id, prompt, category, load_skills, dependsOn, description`), `task.dag.max_nodes_per_run` default 64 (`omo-config-core/src/schema/task.rs`), JS eval prelude `read()` / `print()` / `tool.<name>` (`maho-codemode/src/kernels/js/prelude.rs`), lsp tool names, `todowrite`, `task_status`/`task_send`.

## Not read
- No SUL-1.0 source was opened, read, grepped, or diffed: nothing under `/workspace/work/mhcg/assets/omo-latest/` or `/workspace/work/mhc/assets/`. Test runs of the scripts used synthetic repos under `/tmp`, not the mhc tree.

## Decisions applied (from the dispatcher)
- Full rewrite. State dir `.mhc/` (`.mhc/init-deep/` temporary, `.mhc/init-deep.json` snapshot). Graph runner: mhc `workflow` tool from an eval cell. Max nodes default 64. Fallback without `workflow`: `task` calls in batches with the same file contracts. Parallel skill referred to generically ("parallel orchestration skill"). Base scoring matrix, decision rules, root template, file-writing rule reused. Codex harness section, `lazycodex-*` roles and `multi_agent_v1` removed.
- Own additions (not in spec, implementation choices): helper scripts `scripts/size_route.py` (measurement, formula, routing, bin-packing, ownership lookup), `scripts/verify.py` (PASS/FAIL lines), `scripts/snapshot.py`, `scripts/local_mode.sh`; managed-block markers `# >>> mhc init-deep local (managed) >>>` / `# <<< ... <<<`; a `cleanup` node for `--create-new` ordering; digest `path | score N | role` line format so the verifier can parse it.

## Requirement checklist
| Req | Where | Status |
|---|---|---|
| I-1 purpose | intro | done |
| I-2 constant main-session context | intro, Core invariant | done |
| I-3 modes + argument-hint frontmatter | frontmatter, Usage | done |
| I-4 five-phase todo | Workflow | done |
| C-1 no report reads; verify lines + ≤1 digest | Core invariant, Repair loop | done |
| C-2 structural bounded fan-in | Core invariant | done |
| C-3 repairs through graph | Core invariant, Repair loop | done |
| S-1 single-step measurement | 1.1, size_route.py | done |
| S-2 formula 400 KiB / ÷12 | 1.2, size_route.py | done |
| S-3 routing inline / multi-run / dag, plan + dirs | 1.3, size_route.py | done |
| S-4 create-new ordering | 1.4, cleanup node, inline step 3 | done |
| N-1 inline path | Inline path | done |
| N-2 removed barrage/scaling table/bash set | (absent) | done |
| M-1 one-step graph build via SDK | Phase 2 eval cell (`tool.workflow`) | done |
| M-2 scanners quick, no skills, facts only | Phase 2 | done |
| M-3 rigid numbered template | Scanner prompt | done |
| M-4 report path, ≤5k, six sections, `none` | Scanner prompt | done |
| W-1 writers unspecified-high, load init-deep, self-sufficient body | Phase 3, Node instructions | done |
| W-2 writer steps + digest ≤2k | Node instructions → Writer | done |
| W-3 writer scope + self-check | Node instructions → Writer | done |
| V-1 root writer digests only | Phase 4, Root-writer node | done |
| V-2 verify node checks + PASS/FAIL lines | Phase 4, verify.py | done |
| V-3 repair loop | Repair loop | done |
| K-1 matrix/rules/output reused | Scoring and location | done |
| K-2 LSP / ast-grep (`sg`) source fix | Scoring matrix | done |
| K-3 who applies it | Scoring and location | done |
| T-1 file-writing rule | Templates | done |
| T-2 root template + gates | Templates | done |
| T-3 subdir template + justification line | Templates | done |
| P-1 mode ask + fallback | 5.1, snapshot.py | done |
| P-2 snapshot JSON fields | 5.2, snapshot.py | done |
| P-3 managed exclude block | 5.3, local_mode.sh | done |
| P-4 local→committed removal, no backup | 5.3, local_mode.sh | done |
| P-5 edge cases | 5.1 | done |
| P-6 list nested files | 5.4 | done |
| X-1 delete `.mhc/init-deep/` | 5.4 | done |
| F-1 sizing + cleanup lines | Final Report | done |
| A-1 / A-2 / A-3 anti-patterns | Anti-Patterns | done |

## Acceptance checks run
1. Frontmatter name/description unchanged + argument-hint: PASS.
2. Five phases / five todo items: PASS.
3. Tiny synthetic repo (67 KB): route `inline`, no `.mhc/init-deep/` created; `verify.py --all` PASS; snapshot written: PASS (no live agent run; scripts only).
4. 10.8 MB synthetic repo: N_quick=28, N_high=3, 33 nodes; 28 chunks, 3 writers, no directory owned twice, chunk bytes sum to S, vendor/dist/min/lock/binary excluded: PASS for the plan. The graph itself was not launched (no mhc runtime here).
5. Main session reads no `reports/*`: by construction in the instructions; not observed live.
6. Verify output one line per file; FAIL cases (missing, line limits, no justification, repeated parent block) detected: PASS.
7. Line limits + justification enforced by verify.py: PASS.
8. Snapshot has 5 fields, correct types, 13-digit timestamp; fileCount correct with a newline in a file name: PASS.
9. `local_mode.sh add` twice → exactly one block; user lines kept; `remove` leaves no backup file; worktree path resolves: PASS (GNU sed only; BSD not available to test).
10. Final report has sizing + cleanup lines; `.mhc/init-deep/` removal step present: PASS.
11. grep for codex/lazycodex/multi_agent/omo/OmO names: none.

## Open items
- mhc's init-deep advisor crate (`maho-omo-init-deep-advisor`) currently reads/excludes `.omo/init-deep.json` and plain `AGENTS.md` exclude lines, not `.mhc/init-deep.json` or a managed block. Per the dispatcher's decision this skill uses `.mhc/`; the advisor needs aligning (or the path changed) before they agree. *(Integrator, 2026-10-27: resolved on the advisor side; it now reads `.mhc/init-deep.json`, falls back to the legacy `.omo/init-deep.json`, and excludes both from drift.)*
- The exact JS shape of node fields (`dependsOn`, `load_skills`) and `res.details.run_id` was taken from `dag_tool.rs`; not exercised in a live session.
