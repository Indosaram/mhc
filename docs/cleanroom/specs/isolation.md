# Workspace isolation and git tools — behaviour spec (MIT profile)

Status: spec for Phase 3 step 1. Written from MIT sources only: `can1357/oh-my-pi` @ f59bbac3d
(`crates/pi-iso` rcopy backend and tree copy, `packages/coding-agent/src/task/{worktree,
isolation-runner,isolation-ownership,settings}.ts`, `src/prompts/tools/isolation-{summary,error}.md`,
`src/commit/agentic/tools/{git-overview,git-file-diff,git-hunk,recent-commits}.ts`,
`src/commit/agentic/lock-files.ts`, `src/commit/git/diff.ts`, `src/commit/utils.ts`), the existing
MIT interface `maho_task::isolation::WorkspaceIsolation` (docs/cleanroom/specs/task.md) and the
config key list `task.isolation.*` (config-keys.md). Parts without an MIT original are marked NEW.

## 1. Where isolation plugs in

`maho-task` calls `prepare(task_id, cwd)` before a child starts when the task is `isolated` and
`finish(workspace, succeeded)` after it exits; the text `finish` returns is appended to the task's
`<task-result>` notes. A resumed isolated task gets a fresh isolation prepared from the parent's
current state. Nothing here changes `maho-task`.

## 2. Settings (`task.isolation`, read from the merged config view of the task's cwd)

| Key | Values | Default | Meaning |
|---|---|---|---|
| `enabled` | bool | false | (owned by maho-task) `isolated` defaults to this |
| `backend` | `auto`, `rcopy`, `worktree` (NEW alias), `copy` (NEW), `apfs`/`btrfs`/`zfs`/`reflink`/`overlayfs`/`projfs`/`block-clone` | `auto` | how the isolated directory is materialised |
| `apply` | bool | true | apply a successful child's changes back to the parent checkout |
| `merge` | `patch`, `branch` | `patch` | how changes are captured and integrated |
| `commits` | `generic`, `ai` | `generic` | commit message style; `ai` is accepted and treated as `generic` (no model call here) |

Base directory for isolation directories: `$MAHO_WORKTREE_DIR` when set to an absolute path,
otherwise `<agent dir>/wt`. Each task gets `<base>/t<digest>/m`, the digest a stable hash of the
repository root and task id (oh-my-pi layout).

Backends: `auto`/`rcopy`/`worktree` → git worktree when the source is inside a git repository,
recursive copy otherwise. `copy` → recursive copy (of the repository root including `.git` when it
is a git repository whose `.git` is a directory; refused for a linked worktree). The copy-on-write
backends of oh-my-pi are not implemented: choosing one falls back to the `auto` choice and the
result note says so ("fell back").

## 3. Prepare

1. Resolve the root: `git rev-parse --show-toplevel` from `cwd`; outside git, `cwd` itself is the
   root (copy backend only). The child's cwd is the same relative position inside the isolation.
2. Remove anything at the task's base dir, create it and write the owner marker
   `.mhc-isolation-owner.json` = `{pid, id, startToken?}` (Linux `/proc/<pid>/stat` field 22) before
   materialising.
3. Git worktree: `git -C <root> worktree add --detach <m> HEAD`, then mirror the parent's live
   state: staged diff (`--binary --cached`) applied to index and tree, unstaged diff applied to the
   tree, and every untracked, non-ignored path copied (symlinks recreated, mtimes kept). Copy: copy
   every entry recursively, keeping file modes and mtimes; symlinks are recreated, not followed.
4. Baseline: git mode records the isolation's own snapshot tree — a temporary index seeded from
   `HEAD` (`read-tree`), `add -A`, `write-tree` — so later drift in the parent is never counted as
   the task's work. Plain mode records a manifest `path → (size, mtime)` of the copy.
5. Any failure removes the base dir and is reported as `isolation failed: <reason>` (the task fails
   without running).

## 4. Finish

- Failed or cancelled child: nothing is captured or applied; the isolation is removed; note
  `Isolation: the task did not succeed; its isolated changes were discarded.`
- Successful child, git mode: snapshot the isolation the same way and diff baseline tree → current
  tree (`git diff --binary --full-index`). Write the delta to `<base>/patches/t<digest>-<id>.patch` (NEW location). Empty delta → note `Isolation: no changes.`
  - `merge: patch`, `apply: true`: if the reverse patch applies and the forward does not, the work
    is already present (no-op). Else if the forward patch applies (`git apply --check`), apply it
    to the parent's working tree → `Isolation: applied the task's changes (<n> files).` Else note
    that the patch was not applied, with the patch path, and keep it.
  - `merge: branch`: build commit `HEAD-of-baseline tree + delta` (temporary index, plain apply,
    then `--3way`) with message `<task description>` (generic style), on branch
    `mhc/task/<id>`. With `apply: true`, cherry-pick it onto the parent's checked-out HEAD
    (`cherry-pick --no-edit`); on success delete the branch, on failure `cherry-pick --abort` and
    keep the branch → note names the branch. Commits the child made itself are part of the delta
    (the snapshot covers HEAD + working tree), squashed into the one commit.
  - `apply: false`: nothing touches the parent; note names the patch (or branch).
- Successful child, plain mode (NEW): walk the copy; changed = new path, or size/mtime differ from
  the manifest and content differs from the parent; removed = in the manifest, missing in the copy.
  The patch artifact is a unified text diff (binary files listed by name). With `apply: true`, a
  file is written back (or removed) only when the parent's file still matches the manifest entry;
  otherwise it is reported as a conflict and the isolation is kept.
- When the capture itself fails (cannot write the patch, git error), the isolation is retained:
  the base dir is renamed to `<base>.retained-<time>-<rand>` and the note names it.
- Otherwise the isolation is removed (`git worktree remove --force` + `worktree prune` for git
  worktrees, then the base dir).

## 5. Cleanup of stale isolations (NEW surface, oh-my-pi ownership rule)

`clear_stale(base)` removes every `t*` directory (and `*.retained-*` only when asked) whose owner
marker names a process that is no longer alive (or whose start token changed), running
`git worktree prune` in the recorded repository. Live owners are never touched.

## 6. Git tools (`<builtin:git>`, read-only)

All take the session cwd, require a git repository and never write. Lock files (`Cargo.lock`,
`package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `bun.lock`, `bun.lockb`, `go.sum`,
`poetry.lock`, `Pipfile.lock`, `uv.lock`, `composer.lock`, `Gemfile.lock`, `flake.lock`,
`pubspec.lock`, `Podfile.lock`, `mix.lock`, `gradle.lockfile`) are left out of the analysis and
listed as excluded.

- `git_overview {staged?=true, include_untracked?}` → JSON `{files, stat, numstat[{path,
  additions, deletions}], untrackedFiles?, excludedFiles?}`; `stat` lines ` <path> | <n> +++---`
  (at most 40 of each sign) and a summary line `N file(s) changed, A insertion(s)(+), D
  deletion(s)(-)`.
- `git_file_diff {files[1..10], staged?=true}` → the diffs, files ordered by priority (binary
  assets lowest, tests 10, docs/data 20, other 50, manifests 70, shell/sql 80, source 100), each
  `=== <file> ===`; a diff over 30 lines keeps the first 15 and last 10 lines with
  `[…N ln elided…]`; total budget 120,000 characters, the rest named as truncated. `(no diff)`
  when empty.
- `git_hunk {file, hunks?[1-based], staged?=true}` → the selected hunks of that file's diff;
  binary diff → `Binary file diff; no hunks available.`; none matched → `(no matching hunks)`.
- `recent_commits {count?=8, 1..50}` → JSON `{commits[subjects], stats{scopeUsagePercent,
  commonVerbs, summaryLength{min,max,average}, lowercaseSummaryPercent, topScopes}}` where the
  summary strips a `type(scope): ` prefix.

Outside a git repository each tool returns an error result `not a git repository: <cwd>`.

## 7. Not covered (gaps)

Copy-on-write backends, nested (non-submodule) repositories and submodule detaching, AI commit
messages, kept-alive (parked) children, a `worktree` CLI/slash command, and agentic commit
splitting.

## Leak self-check

- [x] no SUL file was opened while writing this spec (`isolation-core`, `git-master`,
      `git-bash-mcp`, the omo-gated `maho-cli` modules and `crates/omo/**` were not read);
- [x] no OmO-original concept or name appears; identifiers are neutral (`maho-isolation`,
      `maho-git`, `<builtin:git>`, `mhc/task/<id>`, `MAHO_WORKTREE_DIR`) or oh-my-pi tool names;
- [x] wording is new; message texts are rewritten, not copied from the oh-my-pi templates.
