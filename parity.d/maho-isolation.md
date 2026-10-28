# maho-isolation (MIT migration Phase 3, step 1: task workspace isolation)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `crates/pi-iso/src/rcopy.rs` (worktree add/remove, `seed_dirty_state`, recursive copy) | f59bbac | crates/maho-isolation/src/backend.rs, src/tree.rs |
| oh-my-pi `crates/pi-iso/src/tree.rs` | f59bbac | crates/maho-isolation/src/tree.rs |
| oh-my-pi `packages/coding-agent/src/task/worktree.ts` (`ensureIsolation`, `captureIsolationBaseline`, `captureDeltaPatch`, `commitToBranch`) | f59bbac | crates/maho-isolation/src/lifecycle.rs |
| oh-my-pi `packages/coding-agent/src/task/isolation-runner.ts` (`runIsolatedSubprocess`, `mergeIsolatedChanges`, `retainIsolationWorkspace`) | f59bbac | crates/maho-isolation/src/lifecycle.rs |
| oh-my-pi `packages/coding-agent/src/task/isolation-ownership.ts` | f59bbac | crates/maho-isolation/src/owner.rs |
| oh-my-pi `packages/coding-agent/src/task/settings.ts` (`task.isolation.*`, `isolation.backend`) | f59bbac | crates/maho-isolation/src/settings.rs |
| clean-room spec docs/cleanroom/specs/isolation.md | fe7b120f | plain-mode capture/write-back, patch location, notes |

Differences from the originals:

- Backends: only the `rcopy` behaviour (git worktree inside git, recursive copy outside) plus a
  forced `copy`; copy-on-write backends fall back with a note. The worktree is not detached from
  the source's git dir (it shares refs; the child runs on a detached HEAD).
- Baseline and result are `add -A` snapshot trees written through a temporary index, and the
  delta is one tree-to-tree `git diff --binary --full-index` (oh-my-pi replays staged, unstaged
  and untracked patches onto synthetic trees). Commits the child made are folded into the delta.
- Branch mode squashes the delta into one commit `Changes from isolated task <id>` on
  `mhc/task/<id>` (oh-my-pi: `omp/task/<id>`, keeps the agent's own commits) and cherry-picks it.
- Non-git directories are isolated by copy with manifest-based change detection and an
  all-or-nothing write-back (oh-my-pi refuses isolation outside git).
- No nested repositories, submodule handling, AI commit messages, kept-alive children or
  `worktree` CLI. Stale isolations are swept by `TaskIsolation::clear_stale` when the MIT
  delegation extension is built.
