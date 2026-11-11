# Persisted recall terms — clean-room spec (binary store codec)

Behaviour reference: `crates/omo/memory-core/src/recall/bm25_store.rs`.
Implemented in `crates/maho-memory/src/bm25_store.rs`.

## What the store is for

Building a recall index means tokenizing every memory note. A new process can skip that work by reading each
note's terms back from a store in `.git/` — but only if the store is provably about the same notes, made with
the same tokenizer, and complete.

The store is a **derived cache and never a source of truth**. It lives in `.git/`, so it is never committed or
synced. A decode that finds anything wrong returns `None`, and recall tokenizes the corpus from scratch. There
is no repair path and no partial trust: a half-usable store is worse than no store, because it would silently
rank a corpus against a term list that only some notes contributed to.

## Layout

Native-endian `u32` words, header first:

| Field | Contents |
| --- | --- |
| magic | `0x3142_524f`, the endianness tripwire |
| format version | bumped when the layout below changes |
| fingerprint bytes | byte length of the tokenizer fingerprint |
| term bytes | byte length of the joined term text |
| term count | number of entries in the dictionary |
| document count | number of notes with stored terms |
| entry count | total distinct (document, term) pairs |

Then the fingerprint and the terms (joined by `\n`, padded to a 4-byte boundary), one 20-byte key per document,
two words per document (BM25 length, distinct-term count), then all term ids, then all counts.

The magic carries the endianness: a store written on a host of the other byte order fails the check and is
rebuilt rather than misread. Words are read with `from_ne_bytes` out of byte slices, so no alignment is
assumed and a buffer at an odd offset decodes identically.

## Why each document is keyed by a content hash

Each document's key is the SHA-1 of exactly the `description\nbody` text that `recall_document_terms`
tokenizes. A note whose text changed simply does not match its stored entry, so it is tokenized again. This is
what makes the store safe without any invalidation logic: there is no timestamp to trust, no mtime to race, and
no way for edited content to inherit stale terms.

SHA-1 is appropriate here because the value is a cache key and never a security boundary. A collision costs one
unnecessary re-tokenize; it cannot expose, drop, or reorder a memory.

## What the decoder refuses

Every count in the header is attacker-controlled once a file sits on disk, so all lengths are combined with
checked arithmetic and the total is compared against the real buffer length **before any slice is taken**. A
corrupt count cannot read past the end.

Beyond size, the decoder rejects:

- a magic or version mismatch, truncation, or trailing bytes (a store must describe the whole file);
- a term count that disagrees with the term text, since ids index that text;
- a duplicate document key — two entries for one note disagree about its terms and there is no way to pick;
- term ids that do not ascend, or name a term the dictionary does not have;
- a zero count, which would make a document look longer than its real term count and skew its BM25 length
  normalization.

## Scope boundary

This module encodes and decodes bytes. It does not decide when to save, where the `.git` directory is, or how
the terms are seeded into a live corpus — those are the persistence layer's job, and they are ported
separately.
