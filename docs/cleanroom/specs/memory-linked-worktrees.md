# Linked worktrees — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/identity/git_layout.rs` plus `support/paths.rs`. Implemented in
`crates/maho-memory/src/git_layout.rs`.

## The problem

A session may run in a linked worktree (`git worktree add ../feature-x`) while memory is keyed by the main
checkout. Identity then has to answer "which repository does this directory really belong to".

Running `git rev-parse` on every turn is too slow and fails where git is not installed, so this reads the on-disk
layout directly through a `GitLayoutProbe` trait (`entry_kind`, `read_text`, `real_path`). Production supplies a
filesystem probe; tests supply an in-memory map.

## Resolution

`resolve_linked_worktree_source(cwd, probe) -> Option<LinkedWorktreeSource>`:

1. Resolve `cwd` against the process directory and walk **up** to the nearest `.git`.
2. A `.git` **directory** means an ordinary checkout → `None`. A nested repository inside a repository belongs to
   itself, not to an outer worktree.
3. A `.git` **file** is a pointer: first line `gitdir: <path>`, resolved against the checkout root.
4. Read `commondir` in the admin directory, resolved against the admin directory.
5. Require the admin directory's `gitdir` back-pointer to canonicalize to the marker file.
6. Resolve the main working tree from `commondir`.

Returned paths:

- `hash_source` = main working tree joined with the subdirectory the caller started in.
- `slug_source` = the main directory's name, or the subdirectory's name when the caller started below the root.

So a session in `linked/src` of a linked worktree returns `hash_source = <main>/src`, `slug_source = "src"`.

## The back-pointer check

Step 5 is the security boundary. Without it, any file shaped like a pointer redirects memory to an unrelated
repository — including a forged or stale marker left by a deleted worktree. The check compares canonicalized
paths, so a symlinked marker still matches.

Both pointers must be present and non-empty; a missing `gitdir` or `commondir` rejects the whole resolution.

## Main working tree

Given the resolved `commondir`:

| Condition | Result |
|---|---|
| ends in `.git` | parent directory |
| `[core] worktree = <path>` | that path (allows `bare = false` in a shared admin dir) |
| `[core] bare = true` | the common directory itself, slug with a trailing `.git` stripped |
| otherwise | `None` |

The config reader is deliberately minimal: it walks lines, tracks whether it is inside `[core]`, and reads only
`bare` and `worktree`. A key outside `[core]` is ignored, and leaving the section stops the scan.

`bare` accepts `true`, `yes`, `on`, `1` case-insensitively; anything else is false.

Trailing comments are cut only at `" #"` or `" ;"` — a `#` inside a path (`/a#b`) is not a comment because git
requires the leading space. Values keep interior quotes-trimming only at the ends.

## Path normalization

`resolve_from(base, path)` mirrors `path.resolve`: join when relative, then collapse `.` and `..` **lexically**.
`..` at the filesystem root is dropped rather than escaping it.

This matters because git records `commondir` as `../..`; without lexical collapsing the `common` path would keep
inner `..` segments and never match `.git` by name.
