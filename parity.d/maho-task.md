# maho-task (MIT migration Phase 2, step 2)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `packages/coding-agent/src/task/agents.ts`, `task/discovery.ts` | f59bbac | crates/maho-task/src/agents.rs |
| oh-my-pi `src/discovery/helpers.ts` (`parseAgentFields`) | f59bbac | crates/maho-task/src/agents.rs |
| oh-my-pi `src/prompts/agents/task.md`, `scout.md` (adapted) | f59bbac | crates/maho-task/src/agents/{task,scout}.md |
| oh-my-pi `src/task/output-manager.ts` | f59bbac | crates/maho-task/src/ids.rs |
| oh-my-pi `src/task/parallel.ts` (`Semaphore`) | f59bbac | crates/maho-task/src/manager.rs |
| oh-my-pi `src/task/result-summary.ts`, `prompts/tools/task-summary.md` | f59bbac | crates/maho-task/src/summary.rs |
| oh-my-pi `src/task/index.ts` (async jobs), `task/restore.ts`, `prompts/tools/task-follow-up.md` | f59bbac | crates/maho-task/src/{manager,extension}.rs |
| oh-my-pi `src/task/types.ts` (`canSpawnAtDepth`, batch schema) | f59bbac | crates/maho-task/src/{settings,extension}.rs |
| (no MIT original) process runner, state dir, isolation/observer traits | — | crates/maho-task/src/{runner,record,isolation}.rs, spec docs/cleanroom/specs/task.md |

Differences from oh-my-pi: children are `mhc -p` processes, not in-process sessions; results are
read from the child's stdout file instead of a `yield` tool; follow-ups use `task_resume` (no IRC);
`agent://` / `proc://` URLs are replaced by the output file path and `task_output`; no structured
output schemas, workpool, seance, eval tools or isolation backend in this step.
