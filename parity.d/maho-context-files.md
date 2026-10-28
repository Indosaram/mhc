# maho-context-files (MIT migration Phase 3, step 5c: init-deep and its advisor)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `packages/coding-agent/src/prompts/agents/init.md` (what a project context file covers; one `AGENTS.md` at the root) | f59bbac3d | crates/maho-context-files/src/prompt.rs (new wording; extended to one file per significant directory) |
| code-yeongyu/senpi `packages/coding-agent/src/core/extensions/builtin/nested-agents-md/core/{types,find-agents-md-up}.ts` (`AGENTS.md`; nearest-first loading below the root) | 26acd9353 | src/prompt.rs (children summarised, parents not repeated), src/generate.rs (deepest first) |
| clean-room spec docs/cleanroom/specs/init-deep.md | this step | analysis (`src/analyze.rs`), directory choice and plan (`src/plan.rs`), generation through child `mhc -p --tools read,grep,find,ls` (`src/generate.rs`), `mhc init-deep` (`src/cli.rs`), `/init-deep [update]` and the once-per-repository advisor (`src/extension.rs`, `src/advisor.rs`), `init_deep.*` keys |

Differences from oh-my-pi's `/init`: no parallel research subagents (one model run per directory,
each may read files with read-only tools); hierarchical output; an advisor suggests the command.
