# Transcript search — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/search/query.rs` and `search/engine.rs`. Implemented in
`crates/maho-memory/src/transcript_search.rs`.

Search answers "which past conversation said this". This module is the pure half: what a message is matched on,
how a query is parsed, and how matches are ranked. Reading the session store stays at the boundary.

## What is searched

One haystack per message, newline-joined from: message type, content text, reasoning, summary, each tool call's
name and arguments, the tool return, and the function response.

Tool calls are included on purpose. "Which command produced this" is a common query, and the answer is in the
arguments rather than in prose.

## Query parsing

A query is a list of terms and quoted phrases. Matching is case- and accent-folded, and a term matches when the
haystack contains it as a word. The scoring is deliberately odd and deliberately kept: a message scores *higher*
the **later** a term appears, so a hit near the top of a long transcript ranks below the same hit near the bottom.
This is the reference behaviour, not a bug to be fixed here.

An astral character costs two units, because the ranking was computed over JS strings. The `50 − term_length`
bonus saturates at zero, so a very long term cannot push a score negative.

## Date filter

Inclusive on both bounds. A message with no date or an unparseable one stays **eligible**, and an unparseable
bound is ignored.

Search exists to find things. Hiding a message because its timestamp is malformed is the worse failure, so every
malformed input on this path resolves toward including the message.

## Conversations: `search_transcripts`

`search_transcripts` applies the above over a `TranscriptProvider` — anything that can hand back conversations.
The ordering of decisions matters:

1. **An empty or unparseable query returns nothing.** A search matching no term must not degrade into a full dump
   of every message, which is what returning "everything below the limit" would look like.
2. **A limit of zero returns nothing**, and a negative limit is treated as zero. The default is 100.
3. **A hidden conversation is skipped** unless `include_hidden` is true.
4. **`conversation_id` keeps only that conversation.**
5. **Messages are scored, then date-filtered.** A message that does not match any term is dropped before its date
   is ever considered.
6. **Sort by score, and break ties with the later date first.** An undated message sorts last against a dated one
   with the same score, because its date is treated as the empty string.
7. **Truncate to the limit** after sorting.

`created_at` on a result is the document's date, or the epoch (`1970-01-01T00:00:00.000Z`) when it has none. The
document keeps its own `date`; only the convenience field is defaulted, so a caller can tell "no date" from
"dated at the epoch" by looking at the document.

Ordering is by `partial_cmp`, with a `None` (NaN) comparison treated the same as equality. A NaN score therefore
falls through to the date tie-break rather than panicking or sorting arbitrarily.
