# Recall candidate selection — clean-room spec (strategy dispatch)

Behaviour reference: `crates/omo/memory-core/src/recall/select.rs`.
Implemented in `crates/maho-memory/src/select.rs`.

## Three scorers, chosen per call

`select_candidates` runs one of three scorers, chosen by `recall_strategy::choose_recall_strategy`:

| Strategy | When | Why |
| --- | --- | --- |
| `Substring` | small corpus, no CJK in the query or corpus | exact, cheap, no index to build |
| `Bm25` | the query has CJK, or the corpus share of CJK exceeds the threshold | substring scoring degenerates on CJK: a "word" boundary is not a space |
| `Hybrid` | the corpus holds at least `LARGE_CORPUS_MIN_DOCUMENTS` documents | past that size recall quality matters more than one extra index pass |

Size wins over script: a large English corpus still goes hybrid. The corpus CJK share is computed **lazily** —
`choose_recall_strategy_with` takes a closure so the scan never runs when the size or query check already
decided.

`SelectOptions::strategy` overrides the choice. It exists for tests and the benchmark, which must pin a scorer
rather than depend on corpus size.

## Score scale

Every returned score is a relevance in `0.0..=1.0`, higher is better, whichever scorer produced it. Ties break
on path, so the same corpus and query always return the same order.

A single scorer's output is normalized against its own best hit (`rank_to_scores`). It is **not** passed
through `fuse`: RRF weights are rank-based and deliberately flat, so fusing one list with an empty one would
rescale every score to `1/(RRF_K+1)` relative to the best — the same order, a different number.

## Added terms widen the search under every strategy

When the caller supplies expansions, the widened BM25 order is appended after the documents that hold every
query word. This happens for `Substring` too.

The alternative — only widening on the BM25 and hybrid paths — would drop a synonym exactly when it mattered:
a substring pass never looks at a document that shares no query word with the query, so the one document the
synonym was needed to find is the one document it cannot see.

Ranks in the widened result are re-assigned from the combined order, because the two lists were scored on
different scales and only their relative positions are comparable.

## The excerpt shows the word that found the document

Each candidate's excerpt is centered on the first query term that appears in its body. The term list used
for centering includes the caller's added terms, not just the query's own.

Without the added terms, a memory reached only through a synonym shows a window on text that does not
contain the word the search used: the reader sees unrelated prose and cannot tell why the memory was
returned. The list falls back to the body head only when no term matches at all.

## Defect this slice fixes

`recall_strategy.rs` (253 lines) was fully ported and completely unreferenced: `select_candidates` always fused
substring and BM25, so all three strategies behaved identically and the strategy module was dead code. Nothing
failed, because no test pinned a strategy — the behavior was uniform and therefore "correct".

The wiring adds two tests that were impossible before: a corpus that resolves to each of the three strategies,
and a pinned `Substring`/`Bm25` pair that both return matching documents. The expansion test that had been
passing by accident now exercises the widening pass that existed upstream and was missing here.

A second defect was found by testing the real path rather than the helper: a document found only through an
added term had its excerpt centered on the query words, which do not occur in it at all.

## Scope boundary

Extraction, corpus loading, ledger bookkeeping and rendering are other modules. This one scores and orders
documents it is given; it reads no file, takes no lock, and calls no git.
