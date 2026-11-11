# Recall query planner — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/recall/planner.rs`. Implemented in
`crates/maho-memory/src/recall_planner.rs`.

Recall cannot ask what to look for, so it guesses from recent conversation text. `plan_recall_queries(texts,
tool_texts) -> Vec<String>` returns up to four queries (six when tool text is present).

## Why the output is short and lexical

A wide query matches everything and ranks nothing. A query full of filler matches the same common memories every
turn, so recall looks like noise. The planner therefore emits a handful of **distinctive** terms plus up to two
two-word phrases.

## Tokenizing

Lowercase, then split on any non-alphanumeric character. Character class, not whitespace: CJK has no spaces, so
whitespace splitting would collapse an entire Japanese sentence into one token.

## Term filtering

A term is kept when it reaches the minimum length **for its script** and is not a stopword.

- ASCII: 3 characters.
- Non-ASCII: 2 characters. Two characters are already a word in Korean and Japanese; demanding three would drop
  most of them.

Three stopword lists apply: English function words, Korean conversational filler (`그냥`, `진짜`, `그래서`), and —
only for tool text — command names. Command names earn their own list because `git`, `grep` and `ls` appear in
nearly every command and distinguish nothing.

## Ranking

Ordered by, in precedence:

1. **Newest text first** — the most recent thing said is the most likely subject.
2. **Fewer texts** — a term in one message is about that message; a term in every message says nothing.
3. **Longer** — a longer term is more specific.
4. **Earlier position** — a tie-break that makes the output deterministic.

The first two single terms are taken.

## Phrases

Up to two adjacent kept-term pairs, from the **newest** text that yields any, emitted quoted (`"deployment
pipeline"`) so the search treats them as a unit rather than two independent terms.

Pairs containing a stopword are skipped: `"deployment the"` is not a phrase anyone wrote.

## Tool text

Paths and commands the session touched add terms. Two rules make them useful rather than noisy:

- A term from a **path-like** text (`notes/rota.md`, `crates/x`) is promoted above other tool terms. A path names
  a concrete thing a note is likely to mention. The promotion is a stable re-sort, so within the path-derived
  group the normal ranking still applies.
- Tool terms are excluded when they repeat a user single (already planned) or are a command name.

Path-like means: contains `/`, or has a final extension of at most six alphanumeric characters. The extension
limit keeps `release.candidate` from counting as a path.

## Caps

| Cap | Value |
|---|---|
| Single terms from conversation | 2 |
| Single terms from tool text | 2 |
| Phrases | 2 |
| Queries without tool text | 4 |
| Queries with tool text | 6 |

Empty tool text behaves exactly like no tool text, so a caller need not pre-filter.
