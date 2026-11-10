# Lock acquisition — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/locks/acquire.rs`. Implemented from scratch in
`crates/maho-memory/src/lock_acquire.rs`.

A lock file is the only thing stopping two processes from rewriting the same memory file. Its two
failure modes are asymmetric and both silent:

- **Two owners at once** — a memory write is replaced by a concurrent one, with no error anywhere.
- **A dead owner that never yields** — every later writer waits forever on a lock nobody holds.

Everything below follows from refusing both.

## Publication is atomic and never overwrites

```
write record JSON to <lock>.candidate-<nonce>   (create_new, fsync)
hard_link(candidate, <lock>)
  Ok              -> acquired
  AlreadyExists   -> someone else holds it
remove_file(candidate)                          (always)
```

`hard_link` is the primitive because it fails atomically when the target exists. A
`create`+`write` sequence would expose an empty lock file to readers and would overwrite on a
`truncate(true)` bug. The candidate is always unlinked — including when the link lost — so a failed
acquire leaves no `*.candidate-*` debris for the sweep to reason about.

## Release only removes your own lock

`release(path, nonce)` re-reads the record and removes the file **only** if `record.nonce == nonce`.

Without that check, this sequence destroys mutual exclusion: A's lease is reclaimed as stale, B
acquires, A finishes and deletes the file — then C acquires while B still believes it is exclusive.

## Reclamation needs proof, not age

| situation | verdict |
|-----------|---------|
| owner record on another host | `Unknown` — this process cannot observe that host's process table |
| owner on this host, liveness probe says alive | `Alive` |
| owner on this host, liveness probe says dead | `ProvenDead` |
| parseable owner, no probe available | `Unknown` |
| unreadable record, older than the grace window | `ProvenDead` |
| unreadable record, inside the grace window | `Unknown` |

Only `ProvenDead` permits removing the file. `Unknown` is deliberately **not** `Dead`: reclaiming a
lock whose owner may still be running hands the resource to a second writer.

An unreadable record is normally a publisher mid-write — the file exists, the body has not landed. The
5 s `INCOMPLETE_LOCK_GRACE_MS` window is what separates "being written" from "abandoned"; treating
fresh partial locks as stale would delete locks that are about to be held.

## Acquisition is a bounded two-attempt loop

Attempt the link; if it lost, classify the owner. If `ProvenDead`, unlink and retry **once**. A second
loss is reported as contention, not retried forever: a live writer that keeps winning is a fact the
caller must see, and an unbounded loop would hang the agent turn.

## Scoped execution

`with_lock` releases on both success and body failure. A release failure is swallowed **by design** —
the body's own error is what the caller acts on, and a lock left behind is reclaimed by the same
`ProvenDead` path once its owner exits.

## What is injected

`Liveness` and `LocalHost` are traits, and `now` is a parameter. Dead-owner reclamation is therefore
tested deterministically without spawning, killing or waiting on real processes.
