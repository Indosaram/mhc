# Provenance — maho-context-files

- License: MIT.
- Method: **C** (clean room) on MIT inputs. Built from `docs/cleanroom/specs/init-deep.md`,
  written from MIT sources and public behaviour only. "init-deep" (AGENTS.md generation) is a
  general feature per the plan's precedent table (Claude Code / Codex `/init`); the hierarchical
  per-directory form is implemented from the new spec.
- Origins (license verified as MIT before reading):
  - `can1357/oh-my-pi` @ `f59bbac3d56e8b485fef466388b1cb5be4b93123` (LICENSE: MIT):
    `packages/coding-agent/src/prompts/agents/init.md` — what a project context file should cover
    (purpose, architecture, key directories, commands, conventions, important files, tooling,
    testing; concise and practical). Its wording is not copied; the prompts in `src/prompt.rs` are
    new.
  - `code-yeongyu/senpi` @ `26acd935303869fd624f203dd8beb41f66ca4e5d` (LICENSE: MIT):
    `packages/coding-agent/src/core/extensions/builtin/nested-agents-md/core/{types,find-agents-md-up}.ts`
    — `AGENTS.md` is the file name and subdirectory files are loaded nearest-first below the root,
    which is why per-directory files must not repeat their parents.
- No SUL source (`crates/omo/**`, in particular `init-deep-advisor`; the omo `init-deep` skill;
  the omo-gated `maho-cli` modules; `assets/omo*`; `.omo/`; OmO/COMP sources) was opened.
  Thresholds, scoring, prompts, names and the advisor state file are new.
- Tests were written from the spec; no upstream or OmO fixtures were reused.
