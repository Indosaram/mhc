# maho-git (MIT migration Phase 3, step 1: git tools)

| Source (MIT) | Commit | Rust |
|---|---|---|
| can1357/oh-my-pi `crates/pi-iso/src/rcopy.rs` (`git_capture`, `git_apply_with_program`) | f59bbac | crates/maho-git/src/repo.rs |
| oh-my-pi `crates/pi-vcs` query surface used by task isolation and the commit tools | f59bbac | crates/maho-git/src/repo.rs (over the git CLI) |
| oh-my-pi `packages/coding-agent/src/commit/git/diff.ts`, `commit/utils.ts` `renderStat`, `commit/agentic/lock-files.ts` | f59bbac | crates/maho-git/src/diff.rs |
| oh-my-pi `packages/coding-agent/src/commit/agentic/tools/{git-overview,git-file-diff,git-hunk,recent-commits}.ts` | f59bbac | crates/maho-git/src/tools.rs |

Differences from the originals:

- The tools are general session tools (`<builtin:git>`), not commit-agent tools: no shared
  commit state, no diff cache, no scope candidates in `git_overview`.
- Git is driven through the `git` CLI instead of gix/jj-lib; jj repositories are not supported.
- `git_overview` reports the excluded lock files beside the numstat but its `stat` still covers
  them, as in the original.
