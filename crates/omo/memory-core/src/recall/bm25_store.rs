//! Binary codec of the persisted recall term store (pin `recall/bm25-store.ts`). The store is a
//! derived cache: a decode that finds anything wrong returns `None` and recall tokenizes again.
//!
//! Layout, native-endian u32 words unless noted (a store from a host of the other endianness fails
//! the magic check):
//!   header      MAGIC, FORMAT_VERSION, fingerprint bytes, term bytes, term count, document count, entry count
//!   fingerprint UTF-8, the tokenizer fingerprint the terms were made with
//!   terms       UTF-8, the terms joined by "\n", padded to 4 bytes
//!   keys        20 bytes per document, the SHA-1 of the text its terms were made from
//!   documents   per document: BM25 length, number of distinct terms
//!   ids, counts every document's term ids (ascending per document), then every document's counts
//!
//! Words are read with `from_ne_bytes` from byte slices, so no alignment is ever assumed.

use std::collections::BTreeMap;

use super::bm25::{DocumentKey, StoredDocumentTerms};

const MAGIC: u32 = 0x3142_524f;
const FORMAT_VERSION: u32 = 1;
const HEADER_WORDS: usize = 7;
const KEY_BYTES: usize = 20;

/// The persisted terms of a repository's notes under one tokenizer fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallTermStore {
    pub fingerprint: String,
    pub terms: Vec<String>,
    /// SHA-1 of a document's `description\nbody` -> its terms against `terms`.
    pub documents: BTreeMap<DocumentKey, StoredDocumentTerms>,
}

fn pad4(bytes: usize) -> usize {
    (bytes + 3) & !3
}

fn push_word(out: &mut Vec<u8>, word: usize) {
    out.extend_from_slice(&u32::try_from(word).unwrap_or(u32::MAX).to_ne_bytes());
}

pub fn encode_recall_term_store(store: &RecallTermStore) -> Vec<u8> {
    let fingerprint = store.fingerprint.as_bytes();
    let terms = store.terms.join("\n");
    let terms = terms.as_bytes();
    let entries: usize = store
        .documents
        .values()
        .map(|document| document.ids.len())
        .sum();
    let mut out = Vec::new();
    for word in [
        MAGIC as usize,
        FORMAT_VERSION as usize,
        fingerprint.len(),
        terms.len(),
        store.terms.len(),
        store.documents.len(),
        entries,
    ] {
        push_word(&mut out, word);
    }
    out.extend_from_slice(fingerprint);
    out.extend_from_slice(terms);
    out.resize(HEADER_WORDS * 4 + pad4(fingerprint.len() + terms.len()), 0);
    for key in store.documents.keys() {
        out.extend_from_slice(key);
    }
    for document in store.documents.values() {
        push_word(&mut out, document.length);
        push_word(&mut out, document.ids.len());
    }
    for document in store.documents.values() {
        for id in &document.ids {
            out.extend_from_slice(&id.to_ne_bytes());
        }
    }
    for document in store.documents.values() {
        for count in &document.counts {
            out.extend_from_slice(&count.to_ne_bytes());
        }
    }
    out
}

fn word_at(bytes: &[u8], index: usize) -> Option<u32> {
    let start = index.checked_mul(4)?;
    let slice = bytes.get(start..start.checked_add(4)?)?;
    let array: [u8; 4] = slice.try_into().ok()?;
    Some(u32::from_ne_bytes(array))
}

/// `None` for anything but a complete, consistent store of this format.
pub fn decode_recall_term_store(bytes: &[u8]) -> Option<RecallTermStore> {
    if bytes.len() < HEADER_WORDS * 4 {
        return None;
    }
    let header: Vec<usize> = (0..HEADER_WORDS)
        .map(|index| word_at(bytes, index).map(|word| word as usize))
        .collect::<Option<_>>()?;
    let [
        magic,
        version,
        fingerprint_bytes,
        term_bytes,
        term_count,
        document_count,
        entries,
    ] = header[..]
    else {
        return None;
    };
    if magic != MAGIC as usize || version != FORMAT_VERSION as usize {
        return None;
    }
    let keys_offset = HEADER_WORDS
        .checked_mul(4)?
        .checked_add(pad4(fingerprint_bytes.checked_add(term_bytes)?))?;
    let words_offset = keys_offset.checked_add(document_count.checked_mul(KEY_BYTES)?)?;
    let word_count = document_count
        .checked_mul(2)?
        .checked_add(entries.checked_mul(2)?)?;
    if bytes.len() != words_offset.checked_add(word_count.checked_mul(4)?)? {
        return None;
    }

    let text_start = HEADER_WORDS * 4;
    let fingerprint = std::str::from_utf8(bytes.get(text_start..text_start + fingerprint_bytes)?)
        .ok()?
        .to_string();
    let terms_text = std::str::from_utf8(
        bytes.get(text_start + fingerprint_bytes..text_start + fingerprint_bytes + term_bytes)?,
    )
    .ok()?;
    let terms: Vec<String> = if term_count == 0 && terms_text.is_empty() {
        Vec::new()
    } else {
        terms_text.split('\n').map(str::to_string).collect()
    };
    if terms.len() != term_count {
        return None;
    }

    let word = |index: usize| word_at(&bytes[words_offset..], index);
    let mut documents = BTreeMap::new();
    let mut entry = document_count * 2;
    for position in 0..document_count {
        let length = word(position * 2)? as usize;
        let distinct = word(position * 2 + 1)? as usize;
        if entry.checked_add(distinct)? > document_count * 2 + entries {
            return None;
        }
        let ids: Vec<u32> = (entry..entry + distinct)
            .map(&word)
            .collect::<Option<_>>()?;
        let counts: Vec<u32> = (entry + entries..entry + entries + distinct)
            .map(&word)
            .collect::<Option<_>>()?;
        if !is_valid_document(&ids, &counts, term_count) {
            return None;
        }
        let key_start = keys_offset + position * KEY_BYTES;
        let key: DocumentKey = bytes
            .get(key_start..key_start + KEY_BYTES)?
            .try_into()
            .ok()?;
        if documents
            .insert(
                key,
                StoredDocumentTerms {
                    ids,
                    counts,
                    length,
                },
            )
            .is_some()
        {
            return None;
        }
        entry += distinct;
    }
    if entry != document_count * 2 + entries {
        return None;
    }
    Some(RecallTermStore {
        fingerprint,
        terms,
        documents,
    })
}

fn is_valid_document(ids: &[u32], counts: &[u32], term_count: usize) -> bool {
    let mut previous: Option<u32> = None;
    for (position, id) in ids.iter().enumerate() {
        if previous.is_some_and(|previous| *id <= previous)
            || *id as usize >= term_count
            || counts.get(position).copied().unwrap_or(0) == 0
        {
            return false;
        }
        previous = Some(*id);
    }
    true
}

#[cfg(test)]
#[path = "bm25_store_tests.rs"]
mod tests;
