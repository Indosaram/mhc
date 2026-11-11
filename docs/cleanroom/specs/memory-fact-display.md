# Fact display — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/display.rs`. Implemented in
`crates/maho-memory/src/fact_display.rs`.

A stored fact keeps its full provenance. The display drops it, because the same fact read in a note and read
in the memory index must agree on the words, while the stored note stays an honest record of where they came
from.

## Receipt parsing

A receipt is `{ "source": … }`:

| source | shape | rendered as |
|---|---|---|
| `user` | `userQuote`, `approves` (16 lowercase hex) | `user replied "<quote>" to proposal <id>; a record, not the user's approval` |
| `agent` | optional `downgraded: true` | `agent suggestion, not a user decision` (+ `(claimed as the user's, but no matching user words)` when downgraded) |
| `inferred` | — | `inferred, not a user decision` |

Refused, and then the display falls back to the plain fact text rather than inventing a provenance claim:

- an empty or whitespace-only `userQuote`;
- an `approves` that is not exactly 16 lowercase hex characters (an abbreviated or uppercase id cannot name
  the proposal the receipt claims to name);
- a `downgraded` key that is present but not `true` — `false` is a contradiction the writer never produces,
  so the receipt is malformed rather than merely not downgraded;
- any unknown `source`.

The user line always ends with `a record, not the user's approval`. That clause is the point of the receipt:
a quoted approval must never be read as standing permission.

## Quote budget

The quoted speech is whitespace-collapsed, then cut to **300 UTF-16 units**, with `...` appended only when
something was removed. UTF-16 because the original limit applied to a JS string; counting `chars()` would
let an astral character through as one unit and exceed the stored budget. A quote at exactly the budget is
not truncated.

## Suffix stripping

`strip_receipt_suffix` removes trailing `(source: …)` parentheticals that the writer produces, repeatedly, so
a line that accumulated stacked suffixes reads as plain text. Recognition is limited to known shapes:

- `user said "…"` / `user replied "…"`;
- `agent suggestion…`, `inferred…`, `mixed…`.

Anything else is the fact's own prose. A fact may legitimately mention a source in parentheses
(`the record cites (source: an internal document)`), and stripping that would edit what the person wrote. An
unterminated parenthetical is likewise left alone.

## Display

`fact_display_text` = stripped text, plus ` (source: <rendered>)` when the stored provenance is one of the
known shapes. An unrecognised provenance is dropped from the display rather than printed raw, because a raw
parenthetical is not a sentence a reader can interpret.
