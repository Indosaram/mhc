# Reflection orphan sweep — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/orphan_sweep.rs`. Implemented in
`crates/maho-memory/src/orphan_sweep.rs` (pure selection + porcelain parsing; the git removal calls stay
in the caller).

Three surfaces outlive a killed run: registered worktrees, stray run directories, and branches. All three
answer one question — does this belong to a live run, or to one young enough to still be starting?

The dangerous direction is reclaiming a **live** run's worktree: that deletes an in-progress reflection's
output. Every rule is therefore written to under-select.

## Naming

| Surface | Shape | Example |
|---|---|---|
| branch | `memory/reflection-<epoch>-<run>` | `memory/reflection-1700000000000-run-a` |
| legacy branch | `reflection/run-<run>` | `reflection/run-7` |
| worktree dir | `<root>/<epoch>-<run>` | `/refl/1700000000000-run-a` |
| stray dir | `<root>/<epoch>-<run>` | `/refl/1700000000000-run-a` |

The epoch prefix must be strictly digits, so `memory/reflection--run-1` and
`memory/reflection-abc-run-1` do not parse and are never selected.

**The prefix is stripped before splitting on `-`.** The prefix contains hyphens; splitting the raw name
would take `memory` as the epoch and reject every real branch. A worktree **directory** has no prefix at
all — it is named after the suffix — so the strip falls through to the whole name for the directory case.

## Selection

1. A name that does not parse as a reflection owner is never selected. An unfamiliar name belongs to
   someone else.
2. A live run id protects every surface spelled with it. Legacy runs were recorded as both `run-<n>` and
   `reflection-run-<n>`, so either spelling protects.
3. An already-**absent** worktree directory is selected immediately: nothing can be writing into a
   directory that no longer exists, so it needs no grace window.
4. Otherwise the owner's epoch must be at or before `now - grace` (15 min default, inclusive at the
   boundary). The window covers the gap between a prelaunch directory appearing and its ledger recording
   the run, plus clock skew between the process that named it and the one sweeping.
5. A name **without** an epoch (legacy `reflection/run-<n>`) passes step 4: there is no minting time to
   compare, and the live-id check has already had its chance.

## Porcelain parsing

`git worktree list --porcelain`: `worktree <path>` opens a record, `branch refs/heads/<name>` fills it, and
`detached`/`bare` simply leaves the branch empty.

The last record is flushed **after** the loop as well as before each new one — the final record has no
successor to trigger a flush, and dropping it would leak the newest worktree, which is exactly the one most
likely to be live.

`present` is resolved through an injected predicate rather than `Path::exists` so the pure decision is
testable.

## Receipts

One receipt per attempted removal, carrying `kind`, `target`, `removed`, and an optional `detail`. A failing
removal never aborts the sweep: one unremovable worktree must not strand every other leftover.
