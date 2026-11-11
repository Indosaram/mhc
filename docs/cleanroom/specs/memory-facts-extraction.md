# Facts extraction records — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/extraction.rs` (`parse_facts_extraction_record`,
`parse_facts_extraction_jsonl`, `valid_date`). Implemented in `crates/maho-memory/src/facts_extraction.rs`.

The extraction model returns JSONL: one fact per line. Everything here is about refusing bad output, because
whatever survives this parser is written into a memory file that later sessions read as established truth.

## Record shape

Exactly two scopes.

- `project` — `{scope, text, date, receipt?}`
- `person` — `{scope, person: {name, aliases}, text, date, receipt?}`

The allowed key set is exact per scope and for the nested person object. An extra key is an **error**, not
something ignored: a model that emits `confidence` or `slug` has a different contract in mind, and storing the
record would silently discard the meaning of that key. A `project` record carrying `person` is likewise an
error — it is a person fact that lost its scope, and filing it as project truth would attach it to the wrong
topic.

`scope` is matched case-sensitively: `Project` is refused.

## Text

Trimmed, and must be non-empty after trimming. Inner spacing is preserved — only the surrounding whitespace is
the model's formatting, anything inside is the fact's own text.

## Date

`YYYY-MM-DD` **and a real calendar day**. `2026-02-30` and `2026-11-31` are refused, as is `2026-1-01`
(`chrono` alone would accept single-digit fields, so the shape is checked first). Leap days follow the calendar:
`2024-02-29` passes, `2026-02-29` does not.

A fact dated to a day that does not exist would land in a month note no real date can reference again.

## Receipt

Optional. When present it must satisfy the claim-receipt contract in `memory-fact-display.md`; a malformed
receipt refuses the whole record rather than being dropped, because dropping it would silently upgrade an
agent-claimed fact to unattributed.

## JSONL

- Blank and whitespace-only lines are skipped. A streaming model ends with newlines, and failing on those would
  discard an otherwise valid batch.
- Every other line must parse; the first failure aborts the document. Partial application is not offered —
  a half-applied batch leaves the queue head unadvanced and the store inconsistent.
- Error messages name the **1-based** line number, `facts extraction line N: …`.

## Field-check order

The date is validated before the scope, so a record with both a bad date and a missing scope reports the date.
That order is pinned by a test: it is what a caller sees when a model drops the `scope` key and gets its date
wrong, and reporting the later complaint first would send it to fix a field it will not be judged on.

## Receipt storage

The receipt is **validated by** `ClaimReceipt` but **stored as** the value the model sent. Re-serializing the
parsed enum would rewrite it into this crate's shape and change the batch hash, so a batch re-read after an
upgrade would no longer match the recovery envelope that planned it.

## Batch id

`is_uuid_v4` accepts canonical 8-4-4-4-12 form, upper- or lower-case hex, and requires the version nibble to be
`4` and the variant nibble to be one of `8 9 a b`. Uppercase is accepted: a model that upper-cases its id still
produced a unique id, and only shape plus version/variant bits are contractual.

## Records hash

`records_hash(records, agreements)` is what a recovery envelope pins so an applied batch can be proven to match
the planned one.

- With agreements: `{"records": […], "agreements": […]}`.
- With none: a **bare array** of records, not an object wrapping them, and an empty agreement list hashes
  exactly like no agreements at all.

Both details are load-bearing. A recovery envelope written before agreements existed must still validate
against a batch re-read after an upgrade, which fails if the bare case is wrapped or if `[]` is treated as
present. The hash is order-sensitive: two facts swapped between batches is a different batch.
