# Memory expiry — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/memfs/expiry.rs`. Implemented in
`crates/maho-memory/src/expiry.rs`.

A memory file can declare `expires:` in its frontmatter. From that instant on it stops being projected: it is
still on disk, still in history, but no longer part of what the agent is told.

## Accepted values

Only instants that name the **same moment on every device**:

- `2026-10-28` — the start of that UTC day.
- `2026-10-27T06:33Z` — with `Z`.
- `2026-10-27T15:33:00+09:00` — with a numeric offset.

Seconds and milliseconds are optional; a fraction is truncated at millisecond precision, the way `Date.parse`
reads it.

## Refused values

An **offset-less datetime** (`2026-10-27T06:33`) is refused, not assumed to be UTC. A memory store is shared
across machines; the same string would expire the file at a different instant per timezone, and "expired on the
laptop but not the desktop" is worse than no expiry at all.

Also refused: free text, mis-shaped values, out-of-range time fields (`24:00`, `:60`), an impossible calendar
date (`2026-02-30`, `1900-02-29`), and an empty fraction (`…07.Z`).

## Malformed means no expiry

Every refused value reads as `None`, which `is_memory_expired` treats as never expired. This direction is
deliberate: a typo must not silently hide content the person wrote. The cost is that a broken `expires` keeps
a file alive longer than intended, which is visible and recoverable — the opposite mistake is invisible.

## Boundary

Expiry is inclusive: at `expires_at_ms` exactly, the file is already expired. A file expiring "on the 28th" is
gone at `2026-10-28T00:00:00.000Z`, not at the end of that day.

## Reading

The value comes from the frontmatter's `extra` map under `expires`, decoded through the same scalar reader as
every other frontmatter value, so `expires: "2026-10-28"` and `expires: 2026-10-28` read identically. Leading
and trailing whitespace is tolerated.
