# Failure backoff — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/failures_backoff.rs`. Implemented in
`crates/maho-memory/src/failure_backoff.rs`.

A failing facts extraction must not be retried at full speed: the retry can be what keeps the failure
alive (a provider quota, a sandbox still booting).

## Schedule

`BACKOFF_MS = [60_000, 5 * 60_000, 30 * 60_000, 120 * 60_000]`, indexed by `streak.clamp(1, 4) - 1`.

The schedule stops growing at the fourth step. A fifth consecutive failure **parks** the entry instead
of waiting two hours for a sixth attempt. `delay_for` clamps at both ends, so a corrupt streak of 0 or
999 still yields a valid delay.

## Parking

`state` is `Parked` and `next_eligible_at` is `None` when any of:

- the record was already parked
- the reason parks immediately: `payload_entry_oversize`, `secret_like_content`
- the streak reaches `PARK_AT_STREAK` (5)

The two immediate-park reasons are not about severity but about futility: an oversized entry stays
oversized, and content that looks like a secret stays rejected however often it is resubmitted.
Backing off would only delay the conclusion and burn provider calls.

**Parking is sticky.** A later failure never moves a parked record back to backoff, and `parked_at`
keeps the original park instant rather than the latest failure instant. Only an explicit clear (a
success or a retry request) leaves the parked state; a failure after that starts a fresh streak.

## Idempotence

`apply_failure` skips a target whose existing record already has the incoming `failure_id`. Replaying
the same failed attempt — after a crash between the provider call and the ledger write — must not
advance the streak, or a retried write would park a healthy range early.

## Ledger

Sorted by `conversation_id`, then `end_snapshot_line`, then `end_message_id`, so the file is stable
across rewrites and a diff shows only real changes.

`clear_on_success` drops records whose `(conversation, end_message)` completed. `clear_for_retry`
requires every filter that was supplied to match; an empty filter clears the whole ledger, which is the
deliberate "forget all failures" path.

## Detail

Flattened: each run of control characters becomes one space (a multi-line provider error should not
become a wall of blanks), then trimmed, then capped at 512 bytes. Truncation walks character boundaries,
so a stored detail is always valid UTF-8 and a Korean error message is cut at 170 characters rather than
mid-codepoint. An empty or control-only detail is dropped rather than stored as whitespace.
