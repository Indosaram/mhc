# Provenance — maho-git

- License: MIT.
- Method: **P** (port of MIT originals), spec `docs/cleanroom/specs/isolation.md` §6 for the tool
  surface.
- Origin (license verified as MIT before reading): `can1357/oh-my-pi` @
  `f59bbac3d56e8b485fef466388b1cb5be4b93123` (LICENSE: MIT, Copyright 2025 Mario Zechner,
  2025-2026 Can Bölük, 2026 Stencil Labs, Inc.):
  - `crates/pi-iso/src/rcopy.rs` (`git_capture`, `git_apply_with_program`: git invoked with
    `-C`, stdin written while stderr drains on a thread) → `src/repo.rs` `Git::run_with_input`;
  - the query set of `crates/pi-vcs` used by `packages/coding-agent/src/task/worktree.ts` and the
    agentic-commit tools (`diffText`, `numstat`, `changedFiles`, `lsFiles`, `logSubjects`,
    `readTree`/`applyPatch --cached`/`writeTree` on a temporary index) → `src/repo.rs`,
    reimplemented over the `git` CLI (pi-vcs uses gix/jj-lib);
  - `packages/coding-agent/src/commit/git/diff.ts` (`parseFileDiffs`, `parseFileHunks`, hunk
    header parsing), `commit/utils.ts` (`renderStat`), `commit/agentic/lock-files.ts`
    (lock-file list) → `src/diff.rs`;
  - `commit/agentic/tools/git-file-diff.ts` (priority table, 30/15/10-line truncation,
    120k-character budget), `git-overview.ts`, `git-hunk.ts`, `recent-commits.ts` → `src/diff.rs`,
    `src/tools.rs`.
- Not ported: the commit agent itself (scope candidates, split-commit planning, diff cache,
  changelog proposal), jj support.
- No SUL source (`crates/omo/**`, in particular `git-master`, `git-bash-mcp`, `isolation-core`;
  the omo-gated `maho-cli` modules; OmO/COMP sources) was opened.
- Tests were written for this crate from the spec against real temporary git repositories; no
  upstream or OmO fixtures were reused.
