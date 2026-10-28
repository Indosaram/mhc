# Phase 3 step 2 — team runs and task panes (implementer notes)

- Worktree: `/workspace/work/mhc-clean` (branch `cleanroom/phase1`). Before starting, both local
  branches lagged the remote: `origin/mit/skills-relicense` was at b9a16d42 (step 1 commits
  fe7b120f, 1188227a, 9865609a, b9a16d42) while `mhcg` was at 3b918d03 and `cleanroom/phase1` at
  1188227a. The clean worktree held uncommitted edits to `Cargo.lock`,
  `crates/maho-cli/{Cargo.toml,src/cli/{default_extensions,delegation,mod}.rs}`,
  `crates/maho-isolation/src/owner.rs`, `scripts/mit-profile-smoke.py` and an untracked
  `crates/maho-cli/src/cli/git_tools.rs`; each was compared with b9a16d42 and found byte-identical
  (the step 1 wiring before it was committed), so they were reverted/removed and both branches
  fast-forwarded to b9a16d42. Nothing was lost.
- Not opened: the SUL paths listed in `phase1-env.txt` (skip-worktree stubs in this checkout), in
  particular `team-core`, `tmux-core`, `senpi-task`; the omo-gated `maho-cli` modules;
  `assets/omo*`; `.omo/`; any OmO/COMP source. Searches used `git grep` scoped to `crates/maho-*`,
  `docs` and `scripts`; no recursive grep over `crates/`.
- Sources (MIT; LICENSE checked before reading; clones under `/workspace/work/upstream`, pinned in
  PINS.md):
  - `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 (already pinned):
    `packages/coding-agent/src/subprocess/terminal-launch/{tmux,cmux,shared,types,request}.ts`,
    `packages/tui/src/{terminal-multiplexer.ts,multiplexers/tmux.ts,tmux.ts (lines 1-80)}`,
    `packages/tui/test/helpers/fake-tmux.ts`; file list of `packages/coding-agent/src/task/`.
  - `tmustier/pi-agent-teams` @ 235fac8622faed4b0f048ca285237189f590bc5a (LICENSE MIT,
    package.json MIT): `README.md` (features, quick start), `extensions/teams/leader-inbox.ts`
    (lines 15-80, 300-380), `extensions/teams/leader-teams-tool.ts` (action list only).
  - `HazAT/pi-interactive-subagents` @ c100577ebf7393a11d098ad9810ec6c269dcfc30 (LICENSE MIT):
    `pi-extension/subagents/cmux.ts` (lines 40-100, 770-860, 1105-1215).
  - `JokerQianwei/pi-tmux` @ 53af731148c4cd5f5132a7f02900ecbb5817fbb4 (LICENSE MIT): README only.
  - `code-yeongyu/senpi` @ 26acd9353 (already pinned): `examples/extensions/subagent/index.ts`
    header (parallel subagent modes) only.
- Spec step (separate commit 8737cacc, before the code): `docs/cleanroom/specs/team.md`. Amended
  with the implementation commit's follow-up: the pane follower ends with the lead process, the
  `pane` task field, pane errors reported by `team_members`, panes of stopped members close as
  their tasks settle.
- MIT call sites read in the tree: `crates/maho-task/src/{isolation,manager,extension,runner,
  record,settings,ids,summary,lib}.rs`, `crates/maho-task/Cargo.toml`,
  `crates/maho-ext-api/src/types.rs` (`register_command`, `CommandHandler`),
  `crates/maho-cli/src/cli/{delegation,default_extensions,git_tools,mod}.rs`,
  `crates/maho-cli/Cargo.toml`, `crates/maho-isolation/{Cargo.toml,PROVENANCE.md}`,
  `parity.d/maho-isolation.md`, `scripts/mit-profile-smoke.py`, `docs/MIT-PROFILE.md`,
  `docs/cleanroom/specs/{README,config-keys,task}.md`, `PINS.md`.
- Naming: neutral names only. Crate `maho-team`; tools `team_create`, `team_members`,
  `team_message`, `team_wait`, `team_stop`; command `/team`; message type `team-update`;
  variables `MAHO_TEAM_PANES`, `MAHO_TEAM_MUX`, `MAHO_TEAM_PANE_LAYOUT`, `MAHO_TEAM_PANE_LINGER`;
  `maho-cli` module `cli/team_view.rs`. The `task.team.*` and `teams.<name>` keys are the
  existing config interface (config-keys.md).
- Results: `crates/maho-team` (see its PROVENANCE.md and `parity.d/maho-team.md`); `maho-task`
  gains `TaskAddon`/`TaskScope`, `group`/`pane`/`quiet` task fields and
  `TaskObserver::{started_in,finished_in}`; MIT profile glue in `crates/maho-cli`. The default
  (`omo`) build links `maho-team` through `maho-cli` but mounts nothing from it.
- Tests: crate unit tests (settings, tmux/cmux argv, detection, pane observer), integration tests
  with a fake task runner (parallel limit, notices, queued and direct follow-ups, wait/timeout,
  stop, wall-clock limit, limits/names), a fake `tmux` script, and a real tmux 3.5a server
  (Debian arm64 packages unpacked under `/workspace/work/tmuxpkg`, wrapper
  `/workspace/work/bin/tmux`); the smoke script runs a two-member team with real `mhc -p`
  children, headless and attached to a private tmux server.
