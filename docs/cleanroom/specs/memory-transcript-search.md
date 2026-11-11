# Transcript search — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/search/query.rs`. Implemented in
`crates/maho-memory/src/transcript_search.rs`.

Search answers "which past conversation said this". This module is the pure half: what a message is matched on,
how a query is parsed, and how matches are ranked. Reading the session store stays at the boundary.

## What is searched

One haystack per message, newline-joined from: message type, content text, reasoning, summary, each tool call's
name and arguments, the tool return, and the function response.

Tool calls are included on purpose. "Which command produced this" is a common query, and the answer is in the
arguments rather than the prose.

Content is either a string, or an array of parts of which only objects carrying a non-empty `text` contribute.

## Query parsing

Bare words become **terms**; `"quoted runs"` become **phrases**.

An unclosed quote abandons phrase parsing for the whole query and falls back to plain whitespace splitting. The
alternative — treating the rest of the input as half a phrase — silently swallows every later word, and someone
who typed one `"` by accident should still get results.

## Matching: all-or-nothing

A message matches only when **every** term and **every** phrase is present. This is an AND, not a relevance
blend: a message missing one term is not a weaker match, it is not a match.

Comparison is on normalized text — lowercased, whitespace runs collapsed. `normalize_text` is idempotent so a
caller can normalize a haystack once and score it against many queries.

## Ranking

Score per phrase: `offset × 0.1`. Score per term: `offset + (50 − term_length)`.

Two consequences are deliberate, and the first looks like a bug until you know it is the reference:

- **A later match ranks higher.** A hit near the top of a long transcript scores below the same hit near the
  bottom. This is the reference behaviour and is preserved rather than corrected.
- **A shorter term earns a bigger bonus**, so a distinctive short word outranks a common long one.

Offsets are counted in **UTF-16 units**, not bytes or chars, because the reference ranking was computed over JS
strings. An astral character costs two units.

The `50 − term_length` bonus saturates at zero, so a very long term cannot push a score negative.

## Date filter

Inclusive on both bounds. A message with no date or an unparseable one stays **eligible**, and an unparseable
bound is ignored.

Search exists to find things. Hiding a message because its timestamp is malformed is the worse failure, so every
malformed input on this path resolves toward including the message.
