# Phase 3 step 1 — task workspace isolation and git tools (implementer notes)

- Worktree: `/workspace/work/mhc-clean` (branch `cleanroom/phase1`). Before starting, both local
  branches lagged the remote: `origin/mit/skills-relicense` was at 3b918d03 (step 5 commits
  d37d2eff, fb0daeb9, 3b918d03) while `mhcg` was at 01a8bfc3 and `cleanroom/phase1` at 8a69a9e7.
  The clean worktree held uncommitted edits to `Cargo.lock`, `crates/maho-cli/{Cargo.toml,
  src/main.rs,src/cli/{default_extensions,mod}.rs}`, `scripts/mit-profile-smoke.py` and an
  untracked `crates/maho-cli/src/cli/usage_stats.rs`; each was compared with 3b918d03 and found
  byte-identical (the step 5 wiring before it was committed), so they were reverted/removed and
  both branches fast-forwarded to 3b918d03. Nothing was lost.
- Not opened: the SUL paths listed in `phase1-env.txt` (skip-worktree stubs in this checkout),
  in particular `isolation-core`, `senpi-task`, `team-core`, `git-master`, `git-bash-mcp`; the
  omo-gated `maho-cli` modules; `assets/omo*`; `.omo/`; any OmO/COMP source. One recursive
  `grep -rn` over `crates/` (looking for `struct ToolDefinition`) was started by mistake; it timed
  out after 120 s with no output and was replaced by `git grep` limited to `crates/maho-*`. In
  this checkout the `crates/omo/**` files are stubs, and no output from them was seen.
- Sources read (MIT; LICENSE checked first; checkout `/workspace/work/upstream/oh-my-pi`, already
  pinned in PINS.md): `can1357/oh-my-pi` @ f59bbac3d56e8b485fef466388b1cb5be4b93123 (root LICENSE
  MIT; `crates/pi-iso/LICENSE` MIT):
  - `crates/pi-iso/src/{lib.rs (backend kinds, auto order), rcopy.rs, tree.rs, diff.rs (header)}`,
    `crates/pi-iso/Cargo.toml`, `crates/pi-vcs/Cargo.toml` (file list only);
  - `packages/coding-agent/src/task/{isolation-runner.ts, worktree.ts (lines 1-800),
    isolation-ownership.ts, settings.ts (isolation keys)}`,
    `src/prompts/tools/{isolation-summary,isolation-error}.md` (message structure only; texts
    rewritten);
  - `packages/coding-agent/src/commit/agentic/tools/{git-overview,git-file-diff,git-hunk,
    recent-commits}.ts`, `commit/agentic/lock-files.ts`, `commit/git/diff.ts`,
    `commit/utils.ts` (`renderStat`).
- Spec step (separate commit fe7b120f, before the code): `docs/cleanroom/specs/isolation.md`
  fixes settings, backends, the prepare/finish contract, the plain (non-git) path that has no MIT
  original, stale cleanup and the git tool surface.
- MIT call sites read in the tree: `crates/maho-task/src/{isolation,manager,extension,settings,
  lib,runner}.rs`, `crates/maho-task/{Cargo.toml,PROVENANCE.md}`, `crates/maho-tools/src/
  definition.rs` (tool contract), `crates/maho-config/src/load.rs` (`load_native`),
  `crates/maho-telemetry/Cargo.toml`, `crates/maho-cli/src/cli/{delegation,browser_automation,
  default_extensions,mod}.rs`, `crates/maho-cli/Cargo.toml`, `scripts/mit-profile-smoke.py`,
  `docs/MIT-PROFILE.md`, `docs/cleanroom/specs/{README,config-keys,task}.md`, `PINS.md`,
  `parity.d/maho-telemetry.md`.
- Naming: neutral names only. Crates `maho-isolation`, `maho-git`; mount `<builtin:git>`;
  `maho-cli` module `cli/git_tools.rs` (isolation wired in the existing `cli/delegation.rs`);
  branch prefix `mhc/task/`; variable `MAHO_WORKTREE_DIR`; marker `.mhc-isolation-owner.json`
  (oh-my-pi's `omp/task/`, `OMP_WORKTREE_DIR`, `.omp-isolation-owner.json` renamed). Tool names
  `git_overview`, `git_file_diff`, `git_hunk`, `recent_commits` are oh-my-pi's (MIT).
- Results: `crates/maho-isolation`, `crates/maho-git` (see their PROVENANCE.md and
  `parity.d/maho-{isolation,git}.md`); MIT profile glue in `crates/maho-cli`. The default (`omo`)
  build links both crates but mounts neither, so its behaviour is unchanged.
- Tests: crate unit + integration tests against real temporary git repositories and plain
  directories (worktree mirroring of staged/unstaged/untracked state, drift exclusion, child
  commits, patch apply, already-present and conflicting patches, apply off, branch mode with and
  without cherry-pick, plain copy write-back and conflict retention, forced copy of a git repo,
  fallback note, refusal of an isolation root inside the workspace, owner marker sweep); the smoke
  script runs a real isolated `mhc -p` child.
