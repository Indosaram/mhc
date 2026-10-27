# Phase 2 step 2 — task / subagent delegation (implementer notes)

- Worktree: `/workspace/work/mhc-clean` (branch `cleanroom/phase1`, fast-forwarded to
  `mit/skills-relicense` @ 5a1ee1f7 before starting; already up to date). The SUL paths listed in
  `phase1-env.txt` were absent from the working tree and were not opened; neither were the
  omo-gated `maho-cli` modules (`omo_mount`, `omo_shipped`, `memory_runtime`, `recall_wiring`,
  `kibitzer_*`, `task_runners`, `task_session`, `kernel_tools_host`, `host_transport`,
  `facts_child`, `doctor.rs`), `.omo/`, `assets/omo*` or any OmO/COMP source.
- Source read (MIT, LICENSE checked first; checkout in `/workspace/work/upstream/oh-my-pi`):
  `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 —
  `packages/coding-agent/src/task/{types,agents,discovery,output-manager,parallel,result-summary,
  workpool (head only),restore (head only),index (grep for the async/background paths)}.ts`,
  `src/discovery/helpers.ts` (`parseAgentFields`), `src/prompts/agents/{task,scout,reviewer}.md`,
  `src/prompts/tools/{task,task-summary,task-async-contract,task-follow-up}.md`.
- MIT call sites read in the tree: `crates/maho-cli/src/cli/{default_extensions,doctor_mit,
  runtime (print mode),help.txt,mod}.rs`, `crates/maho-ext-api/src/{lib,types}.rs`,
  `crates/maho-tools/src/definition.rs`, `crates/maho-lsp/src/{extension,hooks,post_edit}.rs`,
  `crates/maho-model-select/src/{delegate,grammar,lib}.rs`, `crates/maho-config/src/{schema,load,
  env}.rs`, `crates/builtins/maho-ext-hooks/src/index.rs` (deferred `send_message` pattern),
  `docs/cleanroom/specs/{config-keys,model-selection}.md`, `scripts/mit-profile-smoke.py`,
  `scripts/check-mit-profile.py`, `docs/MIT-PROFILE.md`.
- Behavior spec written first for the parts without an MIT original (child-process runner, state
  directory, interfaces): `docs/cleanroom/specs/task.md`.
- Results: `crates/maho-task` (P + C, see its PROVENANCE.md and `parity.d/maho-task.md`),
  `crates/maho-cli/src/cli/delegation.rs` (MIT glue, `<builtin:task>` in the MIT profile).
