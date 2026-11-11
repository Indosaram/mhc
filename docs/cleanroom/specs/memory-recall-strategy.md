# Recall strategy selection — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/recall/strategy.rs` and `haystack.rs`. Implemented in
`crates/maho-memory/src/recall_strategy.rs`.

Recall can score three ways. Which one runs is decided per query from the corpus shape, not configured:

| Strategy | When |
|---|---|
| `Hybrid` | 200 or more notes |
| `Bm25` | any query contains CJK, or ≥10% of corpus letters are CJK |
| `Substring` | otherwise |

## Order matters

The size check runs **first**. A large English corpus goes hybrid; being English never pulls a large corpus back
to substring scoring. Only after size has not decided do the query and corpus script checks run.

The corpus CJK share is computed **lazily**. When size or a CJK query has already decided, the O(corpus) scan is
skipped entirely. `choose_recall_strategy_with` takes the share as a closure so this ordering is observable in a
test rather than assumed.

## Thresholds

`LARGE_CORPUS_MIN_DOCUMENTS = 200`. At that size the corpus has outgrown what exact substring matching ranks well,
and the extra cost of running both scorers buys better ordering.

`CJK_CORPUS_MIN_SHARE = 0.1`. Deliberately low. A corpus does not need to be mostly CJK to break substring
scoring — it only needs enough that exact substring matches stop being a useful signal. CJK text has no spaces, so
substring matching over it degenerates into long accidental overlaps.

## Counting

The share is CJK code points over **letters**, where a letter is alphabetic or CJK. Digits, punctuation, and
whitespace are excluded from the denominator: a note full of numbers should not dilute the share toward zero and
hide a CJK corpus.

An empty corpus, or one with no letters at all, reports `0.0`. Dividing by zero would make the threshold
comparison meaningless rather than false.

A CJK query forces BM25 regardless of corpus size below the hybrid cutoff — if the person typed CJK, the scorer
must handle it even when the corpus itself is Latin.

## Haystack

`normalized_haystack` is `description` and `body` joined by a newline, then normalized (lowercased, whitespace
collapsed) — the same normalization transcript search uses, so both share one definition of "normalized".

The reference memoizes per-document haystacks in a `WeakMap`. That is a pure performance optimization, so the port
recomputes; a cache here would need invalidation on document mutation for no behavioural gain.
