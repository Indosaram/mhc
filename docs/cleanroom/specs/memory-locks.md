# Lock domains — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/locks/domains.rs`. Implemented from scratch in
`crates/maho-memory/src/locks.rs`.

Two writers each believing they hold the same lock is how one memory write silently replaces
another. These paths must therefore be canonical in both directions: the same logical lock always
lands on the same file, and two different locks never collide.

## The nine domains

| domain | path |
|--------|------|
| `memory-write` | `memory-write.lock` |
| `reflection-scheduler` | `reflection-scheduler.lock` |
| `reflection-finalize` | per run, see below |
| `transcript-state` | per transcript, see below |
| `skills-usage` | `skills-usage.lock` |
| `facts-queue` | `facts-queue.lock` |
| `facts-runs` | `facts-runs.lock` |
| `notice` | `notice.lock` |
| `receipts` | `receipts.lock` |

Serde renames match the protocol strings exactly; these names are on disk and in logs, so a rename is
a compatibility break.

## Transcript locks

`transcript-state-<16 hex>.lock`, digest = first 16 hex chars of SHA-256 of the transcript id.

Hashed rather than named because a transcript id is a session identifier: arbitrarily long, able to
contain path separators, and not guaranteed to be filesystem-safe on Windows. A fixed-width digest is
safe everywhere and cannot escape the lock directory. Empty ids are rejected — every empty id would
otherwise share one lock.

## Reflection finalization locks

A run id that is already filename-safe (`[A-Za-z0-9][A-Za-z0-9._-]{0,79}`) is used verbatim:
`finalize-<run>.lock`. A human reading the lock directory can then tell which run holds it.

Anything else is slugged (non-safe runs of characters become `-`, trimmed of leading/trailing `.` and
`-`, cut to 48 chars) and suffixed with the digest: `finalize-<slug>-<16 hex>.lock`.

The digest is not decoration. Slugging is many-to-one — `run a` and `run-a` both become `run-a` — so
without the suffix two distinct runs would serialize against each other and one would wait forever on
a lock held by an unrelated run.

Rejected outright: empty, whitespace-only, `.`, `..`. A run id that only becomes safe after rewriting
is a caller bug; silently renaming it produces a lock file the caller cannot find again.

## Divergence from upstream

Upstream's fast path also excludes leading `.`, so `.hidden` takes the slug path. This port applies
the same rule through the `[A-Za-z0-9]` first-character requirement, and pins it with a test:
`.hidden` must not produce a dotfile lock.
