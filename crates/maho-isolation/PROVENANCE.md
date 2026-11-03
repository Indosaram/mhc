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
- Copy-on-write backends (markers-11, sync 5.1.52): `can1357/oh-my-pi` `crates/pi-iso` @
  `3a2758b68f7c8e6bdf22c3a76b86c8675e5e0c9e` (read from `~/agent-parity-facts/oh-my-pi`;
  `crates/pi-iso/LICENSE` MIT, Copyright 2025-2026 Can Bölük, 2026 Stencil Labs, Inc., checked
  before reading): `src/lib.rs` → `src/iso/mod.rs`, `src/cow.rs`, `src/apfs.rs`, `src/btrfs.rs`,
  `src/zfs.rs`, `src/linux_reflink.rs`, `src/overlayfs.rs`, `src/windows_block_clone.rs`,
  `src/projfs.rs`, `src/tree.rs` → the same names under `src/iso/`; `src/rcopy.rs` →
  `src/iso/rcopy.rs` (dispatch onto `src/backend.rs`); `packages/coding-agent/src/task/worktree.ts`
  `parseIsolationBackend`/`formatIsolationBackend` and the `ensureIsolation` candidate fallback →
  `src/backend.rs`, `src/settings.rs`. Adapted: no `async_trait`/`diff.rs` (the lifecycle captures
  snapshot trees), `std::sync::Mutex` instead of `parking_lot`.
- Not ported: git-dir detaching and submodule handling, nested repositories, AI commit messages, kept-alive
  children, the `worktree` CLI.
- No SUL source (`crates/omo/**`, in particular `isolation-core`, `senpi-task`, `team-core`; the
  omo-gated `maho-cli` modules; OmO/COMP sources) was opened.
- Tests were written from the spec against real temporary git repositories; no upstream or OmO
  fixtures were reused.
