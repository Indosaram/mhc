# Identity layout — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/identity/layout.rs` plus `support/paths.rs`. Implemented in
`crates/maho-memory/src/identity_layout.rs`.

## Two trees under one identity

```
<memory-root>/agents/<safe-id>/
├── repo/            # git-backed content: the memory the agent reads and writes
└── runtime/         # machine state: locks, transcripts, queues, caches
```

Only `repo/` is memory. Everything under `runtime/` is disposable. The importer reads `repo/` and must never walk
`runtime/` — a transcript or a lock record is not a note, and importing one would put machine state in front of
the user as remembered content.

The two trees are disjoint: neither is a prefix of the other.

## Root

`<home>/.omo/memory` by default. `MAHO_MEMORY_HOME` overrides it.

A **blank** override counts as unset, not as "the empty path". An unset variable usually arrives as an empty
string, and trusting it would resolve memory into whatever directory the process started in.

A relative override resolves against the process working directory.

## Identity paths

`build_identity_paths(root, id)` produces every path the runtime needs, all derived from one `root`:

| Group | Paths |
|---|---|
| content | `repo` |
| concurrency | `locks` |
| conversation | `transcripts` |
| reflection | `reflection`, `reflection_sessions` |
| worktrees & viewers | `worktrees`, `viewers` |
| queues | `push_queue`, `facts_queue`, `facts` |
| notices & receipts | `notices`, `tool_receipts` |
| recall | `recall`, plus `recall_ledger` and `recall_pending` under it |
| accounting | `memory_usage`, `memory_maintenance` |

All runtime paths except `repo` are under `runtime/`. There are 14 runtime subdirectory names; the struct is the
authority on the exact set, since a path builder is what callers actually use.

`recall_ledger` and `recall_pending` are nested inside `recall` rather than being siblings, so recall state can be
discarded or relocated as one unit.

## Path resolution

`resolve_from(base, path)` joins `path` onto `base` unless `path` is already absolute. Absolute paths pass
through untouched, so a caller that already resolved a path is never silently re-rooted.

## Home directory

`HOME`, then `USERPROFILE`, then `HOMEDRIVE` + `HOMEPATH`, else `"."`. Blank values are skipped at each step: a
blank `HOME` that shadowed a valid `USERPROFILE` would put memory in the working directory.
