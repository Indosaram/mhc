# Recall selection — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/recall/select.rs`. Implemented from scratch in
`crates/maho-memory/src/select.rs`.

Selection answers two questions: which memories to surface for a query, and what text to show for
each. It runs two independent rankers and fuses them.

## The two rankers

**Substring** positions a document by the earliest place any query term occurs and by how much of
the query it covers. Coverage is the fraction of query terms present, so a document that uses every
query word outranks one that repeats a single word. The score is `position_factor * coverage`,
clamped to `0.0..=1.0`.

**BM25** ranks with the [`memory-bm25`](memory-bm25.md) scorer, and every score is divided by the
best score in the pass so it lands in `0.0..=1.0`.

Both rankers see the description and the body.

## Fusion

The two ranked lists are fused by **reciprocal rank**: a candidate at rank `r` (1-based) votes
`1 / (60 + r)`. A path present in both lists accumulates both votes, so the two rankers agreeing
beats one ranker being enthusiastic. Candidates are matched by path, never by position. The fused
weights are normalized against the best weight, so the returned `score` is again a relevance in
`0.0..=1.0`, higher is better, whatever produced it.

## Exclusions

Paths already surfaced this session are never returned again. `exclude_paths` drops memories already
visible in the transcript. Both are applied before scoring, not after the cap, so an excluded
document cannot consume a slot that a fresh one would have taken.

## Excerpts

Built only for the candidates that survive the cap. Whitespace is collapsed, the window is 200
characters centered on the earliest occurrence of any query term, and the window is clamped at the
end of the body. With no match the excerpt is the head of the body.

## Determinism

Ties break on path at every sort. The same corpus and query always return the same order, and a
document's rank never depends on the position of its neighbours in the input slice.
