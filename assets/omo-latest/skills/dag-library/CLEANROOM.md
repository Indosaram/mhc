# Clean-room record: dag-library

- **Implementer inputs:** `/workspace/work/cleanroom/dag-library/spec.md` only, plus read-only confirmation of identifiers in mhc Rust/kernel code under `/workspace/work/mhcg/crates` (`omo/components/maho-omo-task/src/dag_tool.rs` for `WORKFLOW_TOOL_NAME = "workflow"`; `maho-codemode/src/extension/kernel_preludes.rs` and `maho-codemode/assets/kernels/js/worker-runtime.js` for the async `read`/`env`/`tool` kernel globals).
- **Not read:** nothing under `/workspace/work/mhcg/assets/omo-latest/` or `/workspace/work/mhc/assets/` (SUL-1.0 text) was opened, grepped or diffed.
- **Authorship:** all prose, code, examples and tests written from scratch.
- **License:** MIT, (c) 2026 Indo Yoon (`LICENSE`).

## Decisions
- Spec §6 option **A**: helper shipped as `scripts/dag-lib.mjs` (JS, also a CLI) and `scripts/dag_lib.py` (Python twin).
- Override env var: `MHC_DAG_LIBRARY` only. The legacy upstream variable is **not** accepted (instruction: no upstream product names).
- Library dirs: `<cwd>/.mhc/dags` and `<home>/.mhc/dags` (per assignment; overrides the spec's `.omo/dags`).
- Graph-authoring skill referenced softly as `mass-ulw` "or whatever authoring skill your build ships".
- Dropped as upstream-specific (spec §10): SDK-root JS module / `OMO_DAG_SDK_ROOT`, legacy env var name, `.omo/` dir name.

## Files
- `SKILL.md`, `LICENSE`, `CLEANROOM.md`
- `scripts/dag-lib.mjs`, `scripts/dag_lib.py`
- `references/example-nightly-check.json` (follows the TASK/DELIVERABLE/SCOPE/VERIFY/STOP WHEN contract, has a verify node, uses `{{key}}` paths)
- `tests/acceptance.test.mjs` (`node --test tests/acceptance.test.mjs`), `tests/test_dag_lib.py` (`python3 tests/test_dag_lib.py`)

## Acceptance checklist (spec §9)
| # | Check | Result |
|---|---|---|
| 1 | save writes one valid `<scope>/.mhc/dags/nightly-check.json` | PASS (both scopes; overwrite refused without force) |
| 2 | two default runs → different run_ids, reused false | PASS against mock `workflow` |
| 3 | same suffix → reused true on 2nd | PASS against mock |
| 4 | empty suffix keeps key verbatim | PASS |
| 5 | no `{{` left; `{{key}}` == rotated key | PASS |
| 6 | `../x`, `a/b` rejected clearly | PASS |
| 7 | env > project > user; `;` delimiter honoured | PASS |
| 8 | missing → lists searched dirs | PASS |
| 9 | JS/Python start + blocking wait → terminal status in an mhc session | PASS against mock only; **live mhc run not performed** |
| 10 | failed node recovered via `retry` on same run_id | PASS against mock only; **live not performed** |

Node: 11/11 tests pass. Python twin: all checks pass.
Mock limitation: checks 2/3/9/10 model the spec's documented `start` idempotency, `wait detach:false`, and `retry`; they must be re-run in a live mhc code-mode session. Also unverified live: whether Python-cell `tool.*` helpers are sync or coroutines (SKILL.md covers both).

## Integrator edits
- 2026-10-27: `SKILL.md` no longer points at `mass-ulw` for graph authoring (that skill was retired from the bundle); it now says to compose the graph with the `workflow` tool, or whatever graph-authoring skill the build ships.
