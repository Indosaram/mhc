# Lock records — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/locks/lock_record.rs`. Implemented from scratch in
`crates/maho-memory/src/lock_record.rs`.

A lock record is the JSON a lock file holds about its holder. Waiters read it to decide whether the
process holding a lock is still alive. Parsing is therefore a liveness judgement, not data loading:

- a record accepted when it should be rejected makes a dead lock look live, and the writer waits
  forever;
- a record rejected when it should be accepted makes a live lock look dead, and two writers proceed
  at once.

Both are silent corruption, so every field is validated and nothing is defaulted.

## Shape

| field | required | rule |
|-------|----------|------|
| `pid` | yes | integer in `1..=u32::MAX`; `0` and negatives rejected |
| `process_start` | yes | non-empty string; opaque token distinguishing a reused pid from the original holder |
| `hostname` | yes | non-empty string |
| `nonce` | yes | non-empty string; unique per acquisition, so two records from one process differ |
| `created_at` | yes | RFC 3339; anything else (a bare date, `yesterday`) rejected |
| `purpose` | yes | non-empty string |
| `run_id` | no | may be absent; `null`, `""`, and non-strings rejected |
| `git_activity_lock` | no | `Some(true)` only for literal `true`; `false` and `"true"` become `None` |

`run_id: null` is rejected rather than treated as absent. Upstream does the same: a record that
serialized `null` came from a writer that meant "no run", and accepting it hides the difference
between "written by a build that has run ids" and "written by one that does not".

## Why `parse_lock_record` returns `Option`

Every caller's reaction to a malformed record is identical — treat the lock as unusable and move on.
A typed error would be discarded at each call site. `create_lock_record` still returns a typed
`LockRecordError` for an empty purpose, because that is a caller bug the caller can fix.

## Creation

`create_lock_record` reads pid, start token, hostname, nonce, clock and OS-lock availability through
the injected `LockHost` trait. Production supplies the real host; tests supply a fake, so record
creation is verified without reading the real clock, hostname or process table.

A process-start token that cannot be read is recorded as the literal `"unavailable"` rather than
failing the acquisition: a lock whose liveness check degrades is better than no lock at all, and the
waiter's stale-lock sweep uses age as its second signal.

Optional fields absent from the record are omitted from JSON entirely, so an old reader sees exactly
the fields it understands.
