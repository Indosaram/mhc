# Provenance — maho-isolation

- License: MIT.
- Method: **P** (port of MIT originals) for the worktree/copy backends, ownership marker and the
  capture → merge-back → retain → cleanup lifecycle; **C** (clean-room, from
  `docs/cleanroom/specs/isolation.md`, commit fe7b120f) for the parts without an MIT original:
  the plain (non-git) change detection and write-back, patch artifact location, settings
  aliases and the mhc result notes.
- Origin (license verified as MIT before reading): `can1357/oh-my-pi` @
  `f59bbac3d56e8b485fef466388b1cb5be4b93123` (LICENSE: MIT, Copyright 2025 Mario Zechner,
  2025-2026 Can Bölük, 2026 Stencil Labs, Inc.; `crates/pi-iso/LICENSE` MIT, Copyright
  2025-2026 Can Bölük, 2026 Stencil Labs, Inc.):
  - `crates/pi-iso/src/rcopy.rs` (git worktree `add --detach`/`remove --force`,
    `seed_dirty_state`, recursive copy keeping mtimes) → `src/backend.rs`, `src/tree.rs`;
  - `crates/pi-iso/src/tree.rs` (`copy_dir_contents`, `copy_symlink`, `remove_existing`) →
    `src/tree.rs`;
  - `packages/coding-agent/src/task/worktree.ts` (`ensureIsolation` base-dir/`m` layout and
    owner-first ordering, `captureIsolationBaseline` "the isolation's own state is the
    baseline", `captureDeltaPatch` via synthetic trees on a temporary index, `commitToBranch`
    tree = parent + delta) → `src/lifecycle.rs` (adapted: both baseline and result are
    `add -A` snapshot trees, the delta is one tree-to-tree diff);
  - `packages/coding-agent/src/task/isolation-runner.ts` (failed runs skip capture, patch
    artifact per task, retain the workspace under a unique `.retained-*` sibling when capture
    fails, `mergeIsolatedChanges` reverse/forward `--check` idempotence, branch cherry-pick with
    the branch kept on failure) → `src/lifecycle.rs`;
  - `packages/coding-agent/src/task/isolation-ownership.ts` (owner marker with pid and process
    start token, liveness check) → `src/owner.rs`;
  - `packages/coding-agent/src/task/settings.ts` (`task.isolation.{enabled,apply,merge,commits}`,
    `isolation.backend` values) → `src/settings.rs`.
- Not ported: copy-on-write backends (APFS, btrfs, ZFS, reflink, overlayfs, ProjFS, block clone),
  git-dir detaching and submodule handling, nested repositories, AI commit messages, kept-alive
  children, the `worktree` CLI.
- No SUL source (`crates/omo/**`, in particular `isolation-core`, `senpi-task`, `team-core`; the
  omo-gated `maho-cli` modules; OmO/COMP sources) was opened.
- Tests were written from the spec against real temporary git repositories; no upstream or OmO
  fixtures were reused.
