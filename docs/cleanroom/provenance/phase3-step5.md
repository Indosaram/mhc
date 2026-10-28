# Phase 3 step 5 — agent home, first-run setup, project context files (init-deep)

## What the three items cover
Plan §2 lists `agent-home`, `onboarding` and `init-deep-advisor` as C (behaviour spec). Without
opening them, MIT call sites and MIT upstreams show what they are:
- agent home: the senpi agent dir (`maho-core::config`) inside `~/.omo`, plus a first-run marker
  that MIT test fixtures pre-create (`onboarding-completed` in `~/.omo` and in
  `<agent dir>/omo-senpi/omo-native`);
- onboarding: first-run setup — oh-my-pi (MIT) ships a versioned setup wizard (`omp setup`,
  provider sign-in and model scenes) that is general prior art;
- init-deep: AGENTS.md generation, general per the plan's precedent table (Claude Code / Codex
  `/init`, oh-my-pi `init.md`); the hierarchical form and the advisor are specified anew.
None is OmO-original in the sense of §0.3, so all three are implemented (no removal).

## Inputs
- `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 (MIT, already pinned; LICENSE
  checked): `packages/tui/src/setup/{wizard.ts,setup-version.ts,scenes/types.ts}`,
  `packages/coding-agent/src/{modes/setup.ts,commands/setup.ts}`, the setup gate in `src/main.ts`,
  `packages/coding-agent/src/prompts/agents/init.md`.
- `code-yeongyu/senpi` @ 26acd935303869fd624f203dd8beb41f66ca4e5d (MIT, already pinned):
  `packages/coding-agent/src/config.ts` (as ported in maho-core),
  `src/core/extensions/builtin/nested-agents-md/core/{types,find-agents-md-up}.ts`.
- MIT mhc code: `maho-core` (config paths, `AuthStorage`, `SettingsManager`, `ModelRuntime`),
  `maho-config` (writer, schema), `maho-model-select` (model profiles), MIT test fixtures for the
  marker file name.
- Specs written for this step: `docs/cleanroom/specs/{agent-home,onboarding,init-deep}.md`.

No SUL path, `crates/omo/**` (in particular `agent-home`, `maho-omo-onboarding`,
`init-deep-advisor`), the omo `init-deep` skill, `assets/omo*`, `.omo/` or omo-gated maho-cli
module was opened. No upstream was newly cloned. senpi's `.omo/init-deep.json` (an artifact of the
omo tool in the senpi repository) was seen in a file listing and deliberately not opened.

## Decisions
- Three crates (MIT, publish = false, PROVENANCE.md): `maho-agent-home`, `maho-onboarding`,
  `maho-context-files`. maho-cli wires them through neutral modules `cli/user_home.rs`
  (`mhc home`), `cli/first_run.rs` (`mhc setup`, first-run gate) and `cli/project_context.rs`
  (`mhc init-deep`, `<builtin:context-files>`), all `cfg(not(feature = "omo"))`; the entries are
  dispatched in `main.rs` under the same cfg. The default `omo` build links the crates
  (non-optional dependencies, like maho-gateway) but neither mounts nor dispatches them.
- maho-config accepts top-level `setup` and `init_deep` sections (map of any, like `gateway`).
- Setup writes the default model twice: `settings.json` (`defaultProvider`/`defaultModel`) for
  interactive sessions, and a `setup` model profile in `omo.jsonc` for the session-start model
  profile (which does not apply in TUI mode).
- The first-run gate prompts only when no model is usable; otherwise it records the state silently.
- init-deep children run `mhc -p --no-session --tools read,grep,find,ls`; parents get their
  children's summaries; existing files are kept unless `--update`. The advisor runs only inside git
  repositories, walks at most 50 000 entries, once per repository.

## Verification (local only, no CI)
See the step report in the plan (§10); commands: crate tests, clippy --all-targets -D warnings and
rustfmt for the three crates; maho-config tests; maho-cli MIT clippy (--lib --bins -D warnings) and
MIT lib tests; check-mit-profile; full mit-profile-smoke with browser + tmux (new blocks: agent
home, setup, first-run gate in tmux, init-deep with the advisor); `cargo check -p maho-cli`
(default features) in mhcg.

## Workspace notes
- A workspace rollback removed the first copy of `docs/cleanroom/specs/agent-home.md` before it was
  committed; it was rewritten and committed immediately, and every later sub-step was committed and
  bundled (`/workspace/work/p3s5.bundle`) right away.
