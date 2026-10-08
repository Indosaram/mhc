# Pre-existing home-path inventory failure

Command: bun tools/check-omo-sites.mjs --comp /home/projects/mhc-archive/session-state/codex/omo-components
Exit: 1. Complete output: check-omo-sites.log.

Pinned COMP total is 34 as expected. Native rename total is 17 as expected. Native keep total is 22 rather than baseline 17. The five additional matched literals occur in four files added after the imported baseline: senpi-task/src/team/registry.rs (1), team/storage.rs (1), tools/task/plan_review_contract.rs (1), isolation/runtime.rs (2, one in a source-contract comment). They are not declared in tools/omo-sites.json.

IsolationRuntime::sweep_roots explicitly preserves HOME/.omo/wt as pinned producer source identity, distinct from the .maho agent/config profile root (runtime.rs:198-208). Team paths and plan-review matching are project-level .omo paths. No path has been renamed and no baseline expectation or failing test has been changed merely to obtain a pass. This tooling row remains unresolved pending pinned-source reconciliation of the additions.
# Accepted repair

The COMP baseline sites and expected totals remain unchanged. A separate native_additions inventory accounts for the four source-equivalent senpi-task files, with pinned/source citations. The checker applies both inventories to native paths but only the baseline inventory to COMP; absent/changed/unlisted files still fail. Selected latest isolation producer packages/senpi-task/src/isolation/runtime.ts:104 at 455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b explicitly retains HOME/.omo/wt.

Acceptance command bun tools/check-omo-sites.mjs --comp /home/projects/mhc-archive/session-state/codex/omo-components exited 0: COMP 34/34, native keep 22/22, native rename 17/17, all 24 files matched. Prior failure is historical evidence, not a current blocker.
