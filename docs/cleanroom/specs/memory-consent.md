# Memory consent — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/provenance/claim.rs` and `consent.rs`. Implemented in
`crates/maho-memory/src/consent.rs` (receipt parsing and rendering live in `fact_display.rs`).

## Why this gate exists

A memory file can carry `provenance: {"source":"user", ...}`, which reads as "the user approved this". That
stamp is exactly what makes a stored decision look authoritative months later — including to an agent that has
lost the original conversation. So the interesting question is not how to render the stamp, it is **who is
allowed to produce it**.

The rule: an agent may claim `source: user`, but the claim only survives if the user's actual next message was
a short explicit approval.

## Consent phrases

A closed list, matched against the whole normalized reply:

- English: `yes`, `ok`, `okay`, `do it`, `go ahead`, `delete it`, `yes delete it`.
- Korean: `응`, `네`, `예`, `좋아`, `그래`, `지워`, `지워줘`, `삭제해`, `삭제해줘`, `진행해`.

A sentence is not consent. `yes please do that now` and `yes, but keep the old one` are not in the list, and
neither is anything else that merely contains the word yes. The list is closed on purpose: widening it to
substring matching would let almost any reply verify an approval.

## Normalization

Before matching: whitespace collapsed, lowercased, and **at most one** final `.` or `!` removed. So `Yes.` and
`  yes  ` are the same approval.

One character is not removed: an internal apostrophe is kept only when it sits between two letters
(`don't`). A quote that ends the message, or touches punctuation, is not part of a word, so the message fails
the shape check.

## Shape check

A reply only counts if it is shaped like a plain reply: letters (ASCII or Hangul), digits, spaces, and internal
apostrophes, ending in a letter or digit.

This rejects a quote that carries code, punctuation or an emoji. Those are pasted content, not a one-word
answer, and a message can contain `yes` while being about something else entirely. Failing the shape check
downgrades the receipt rather than erroring: the fact is still stored, just honestly labeled.

## Resolution

| claim source | condition | receipt |
|---|---|---|
| `user` | some exchange's reply matches the quote **and** is a consent phrase | `User { user_quote, approves: proposal_id }` |
| `user` | no match | `Agent { downgraded: true }` |
| `agent` | — | `Agent { downgraded: false }` |
| absent / unknown | — | `Inferred` |

`downgraded: true` is not an error state. It records what happened: the write said the user approved, and the
conversation did not agree. The rendered text says so explicitly — "claimed as the user's, but no matching
user words".

## Proposal id

`proposal_id` is the first 16 hex characters (8 bytes) of SHA-256 over the whitespace-normalized proposal, so a
receipt names *which* proposal it answers. Whitespace differences do not change the id; different proposals do.

## The authority rule

`MEMORY_AUTHORITY_RULE` is shipped copy, not logic. It states that memory is reference only and never carries
the user's authority, so that a future reader — human or agent — is told plainly that a receipt is a record,
not a permission. `USER_QUOTE_RULE` is the instruction shown to a model writing a claim.
