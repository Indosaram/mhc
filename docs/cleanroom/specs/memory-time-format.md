# memory-time-format

One implementation of "RFC 3339 text to Unix milliseconds" for this crate.

## Why this module exists

Four modules converted stored instants to milliseconds independently:
`facts_failures_schema` (`parse_rfc3339_millis` / `format_rfc3339_millis`),
`failure_backoff` (`parse_millis` / `format_millis`), `failure_selection`
(`parse_millis`), and `payload_cap` (`parse_millis`). Three of the `parse_millis`
bodies were byte-identical; the formatting pair differed in how it handled an
unrepresentable value.

The duplication is not cosmetic. Every one of these call sites compares instants
to make a decision:

- `failure_selection` asks whether a range is still in backoff;
- `failure_backoff` computes `next_eligible_at` from the current attempt time;
- `payload_cap` orders conversations by starvation age;
- `facts_failures_schema` rejects a stored instant that is not in the exact shape
  this crate writes.

Two of them sharing an accepted-input set while a third accepted slightly more
would let one module call an entry eligible while another called it parked, with
no single place to see the disagreement. The shared module makes the accepted
input set one thing.

## Behaviour

`parse_millis(text) -> Option<i64>`

- Accepts RFC 3339 with `Z` or a `±HH:MM` offset, with or without fractional
  seconds. Delegates to `chrono`, which this crate already depends on.
- Rejects a bare `YYYY-MM-DD`, an out-of-range month or day, and any non-instant
  text.
- An offset is normalised to UTC; two spellings of one instant compare equal.

`format_millis(millis) -> Option<String>`

- Renders `YYYY-MM-DDTHH:MM:SS.sssZ`, matching `Date.prototype.toISOString()`.
- A whole second gains an explicit `.000` field. Stored text is compared as
  strings in `require_instant`, so the fraction may never be omitted.
- `None` when the millisecond value is outside the representable range.

`format_millis_or_empty(millis) -> String`

- For callers that render unconditionally.
- An unrepresentable value renders as the empty string, not a placeholder date: a
  blank timestamp is visibly wrong, while a plausible-looking wrong instant would
  be trusted and compared.

## Not ported

Upstream `support/time.rs` also carries `now_millis`, `now_iso`, and a hand-rolled
`civil_from_days` / `parse_rfc3339` pair. Those are not reproduced:

- This crate depends on `chrono`, so a hand-rolled civil-date implementation would
  be a second, less-tested copy of what the dependency already does.
- `now_millis` / `now_iso` have no caller here; the modules that need the current
  time take it as an input parameter so their behaviour is testable without a
  clock.

## Verification

`crates/maho-memory/src/time_format.rs` tests: an instant round-trips; a whole
second gains `.000`; an offset normalises to UTC; four non-instant strings are
rejected; an unrepresentable value renders empty.

The three consolidated call sites keep their pre-existing tests, which are the
real check: `failure_selection`'s backoff-eligibility cases,
`failure_backoff`'s schedule cases, and `payload_cap`'s starvation-ordering cases
all still pass, so the shared parse agrees with what each module did before.
