# BM25 ranking — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/recall/bm25.rs`. Implemented from scratch in
`crates/maho-memory/src/bm25.rs`.

Memory documents are ranked with Okapi BM25, `k1 = 1.5` and `b = 0.75`, over lowercased tokens.
Only documents that share at least one term with one of the queries are returned, best first.

## Tokenization

Text is lowercased, then split into word runs and CJK runs. A CJK run of three or more characters
contributes the whole run **and** every adjacent pair, so a two-character query finds a longer
phrase without a word segmenter. In addition each Han character of the run is indexed on its own,
but those single characters are excluded from the document length used for normalization — they
repeat what the run already counted, and counting them twice would bias long CJK documents.

`is_han_character` is true only for a one-character Han token; an entire phrase is not a "Han
character" for this purpose.

## Length normalization

`length` is the token count minus the stand-alone Han characters. Average length is the mean over
the corpus, with `1.0` substituted when it would be zero.

## Expansions

A query may be widened with synonyms (weight 0.75), keywords (0.75), related terms (0.4) and a
single note line (0.4). A term already present in the query is never added, and a term appearing in
several tiers keeps its **highest** weight.

Expansion terms are only scored for documents that are not a full match. They are additive on top
of the query score.

## Full-match lift

A document containing **every query unit** is a full match. Full matches are placed above every
partial match and receive the best partial score as a lift, so a page that answers the whole query
never loses to one that repeats a single word.

The lift only exists when the query has expansions — with no expansions there is no partial
ranking to lift above, so every result is reported as a partial match.

## Determinism

Ties break on `RecallDocument::path`, so the same corpus and query always produce the same order,
and reordering the input slice does not change any score.
