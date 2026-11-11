# Support: random identifiers — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/support/random.rs`. Implemented in
`crates/maho-memory/src/random_ids.rs`.

Two identifiers the lock and reflection machinery needs: a plain id and a UUID-shaped one. Neither is a
capability, a bearer token, or anything an attacker is assumed unable to guess. They exist so concurrent
callers do not collide.

## Uniqueness argument

The seed is `pid:nanos:counter`.

- **counter** — a process-local `AtomicU64`. Two calls in one process never share a seed, even inside the same
  nanosecond and the same thread.
- **pid** — separates concurrent processes.
- **nanos** — separates a process from a previous run that happened to get the same pid.

The digest of that seed is SHA-256; the first 32 hex characters are the id. Truncation is fine because
collision resistance is not the requirement — uniqueness within a realistic working set is.

A clock that leaps backwards cannot repeat an id, because the counter has already advanced. A system clock
before the epoch yields a `0` nanosecond component rather than a panic, which is still unique by counter and
pid.

## The UUID shape

`random_uuid` reformats that id into five dash-separated groups of 8-4-4-4-12 and overwrites two nibbles:

- index 12 becomes `4` (version),
- index 16 becomes `8` (variant).

This deliberately does **not** derive the variant from the digest bits. The value is only ever consumed by
interfaces that want something UUID-shaped; deriving real RFC 4122 bits would buy nothing and would make the
output depend on digest internals. The constant is intentional, not a bug, and a future reader must not
"fix" it into bit arithmetic without a caller that needs it.

## Not included

No dependency on `uuid` or `getrandom`. The uniqueness properties above are met without either, and the crate
already hashes with SHA-256.
