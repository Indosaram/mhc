//! Binary codec of the persisted recall term store (behaviour reference: `recall/bm25_store.rs`).
//!
//! The store is a derived cache and never a source of truth: a decode that finds anything wrong
//! returns `None` and recall tokenizes the corpus again. Nothing here can make a memory unreadable.
//!
//! Layout, native-endian `u32` words unless noted:
//!
//! ```text
//! header      magic, format version, fingerprint bytes, term bytes,
//!             term count, document count, entry count
//! fingerprint UTF-8, the tokenizer fingerprint the terms were made with
//! terms       UTF-8, the terms joined by "\n", padded to 4 bytes
//! keys        20 bytes per document, the SHA-1 of the text its terms came from
//! documents   per document: BM25 length, then the number of distinct terms
//! ids, counts every document's term ids (ascending per document),
//!             then every document's counts
//! ```
//!
//! Words are read with `from_ne_bytes` from byte slices, so no alignment is ever assumed. A store
//! written on a host of the other endianness fails the magic check and is rebuilt.

use std::collections::BTreeMap;

use crate::bm25::{DocumentKey, StoredDocumentTerms};

const MAGIC: u32 = 0x3142_524f;
const FORMAT_VERSION: u32 = 1;
const HEADER_WORDS: usize = 7;
const KEY_BYTES: usize = 20;

/// The persisted terms of a repository's notes under one tokenizer fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallTermStore {
    /// The fingerprint the terms were made with; a store of another tokenizer is ignored.
    pub fingerprint: String,
    /// The term dictionary every document's ids index into.
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

/// The store as bytes. Every length is written as a 32-bit word, so a store too large to address
/// saturates rather than wrapping into a different, plausible-looking file.
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
///
/// Every count in the header is attacker-controlled once the file is on disk, so the lengths are
/// combined with checked arithmetic and the total is compared against the real buffer length before
/// any slice is taken: a corrupt count cannot make this read past the end.
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

/// A document's ids must ascend (so a lookup can bisect), name a real term, and never carry a
/// zero count: a term counted zero times would still make the document look longer than it is and
/// skew its BM25 length normalization.
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
mod tests {
    use super::*;

    fn store() -> RecallTermStore {
        let mut documents = BTreeMap::new();
        documents.insert(
            [1u8; 20],
            StoredDocumentTerms {
                ids: vec![0, 2],
                counts: vec![3, 1],
                length: 4,
            },
        );
        documents.insert(
            [2u8; 20],
            StoredDocumentTerms {
                ids: vec![1],
                counts: vec![5],
                length: 5,
            },
        );
        RecallTermStore {
            fingerprint: "fp-é".to_string(),
            terms: vec!["alpha".to_string(), "베타".to_string(), "gamma".to_string()],
            documents,
        }
    }

    #[test]
    fn a_store_round_trips() {
        let original = store();
        assert_eq!(
            decode_recall_term_store(&encode_recall_term_store(&original)),
            Some(original)
        );
    }

    #[test]
    fn an_empty_store_round_trips() {
        let empty = RecallTermStore {
            fingerprint: String::new(),
            terms: Vec::new(),
            documents: BTreeMap::new(),
        };
        assert_eq!(
            decode_recall_term_store(&encode_recall_term_store(&empty)),
            Some(empty)
        );
    }

    #[test]
    fn a_misaligned_buffer_decodes_the_same() {
        let bytes = encode_recall_term_store(&store());
        let mut shifted = vec![0u8];
        shifted.extend_from_slice(&bytes);
        assert_eq!(decode_recall_term_store(&shifted[1..]), Some(store()));
    }

    #[test]
    fn bad_magic_version_or_truncation_decodes_to_none() {
        let bytes = encode_recall_term_store(&store());
        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 0xff;
        assert_eq!(decode_recall_term_store(&bad_magic), None);
        let mut bad_version = bytes.clone();
        bad_version[4] ^= 0xff;
        assert_eq!(decode_recall_term_store(&bad_version), None);
        assert_eq!(decode_recall_term_store(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode_recall_term_store(&bytes[..10]), None);
        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(decode_recall_term_store(&trailing), None);
    }

    #[test]
    fn an_out_of_range_term_id_is_rejected() {
        let mut original = store();
        if let Some(document) = original.documents.get_mut(&[2u8; 20]) {
            document.ids = vec![9];
        }
        assert_eq!(
            decode_recall_term_store(&encode_recall_term_store(&original)),
            None
        );
    }

    #[test]
    fn a_duplicate_document_key_is_rejected() {
        // Two entries under one key would make the store disagree with itself about one note's
        // terms; there is no way to pick, so the whole store is refused and rebuilt.
        let original = store();
        let mut bytes = encode_recall_term_store(&original);
        let document_count = word_at(&bytes, 5).expect("document count") as usize;
        assert_eq!(document_count, 2);
        let first_key = HEADER_WORDS * 4
            + pad4(original.fingerprint.len() + { original.terms.join("\n").len() });
        let second_key = first_key + KEY_BYTES;
        let (first, second) = bytes.split_at_mut(second_key);
        second[..KEY_BYTES].copy_from_slice(&first[first_key..first_key + KEY_BYTES]);
        assert_eq!(decode_recall_term_store(&bytes), None);
    }

    #[test]
    fn a_non_ascending_id_list_is_rejected() {
        let mut original = store();
        if let Some(document) = original.documents.get_mut(&[1u8; 20]) {
            document.ids = vec![2, 0];
            document.counts = vec![1, 1];
        }
        assert_eq!(
            decode_recall_term_store(&encode_recall_term_store(&original)),
            None
        );
    }

    #[test]
    fn a_zero_count_is_rejected() {
        let mut original = store();
        if let Some(document) = original.documents.get_mut(&[1u8; 20]) {
            document.counts = vec![3, 0];
        }
        assert_eq!(
            decode_recall_term_store(&encode_recall_term_store(&original)),
            None
        );
    }

    #[test]
    fn a_term_count_that_disagrees_with_the_text_is_rejected() {
        let original = store();
        let mut bytes = encode_recall_term_store(&original);
        // Bump the term count without adding a term.
        let count = word_at(&bytes, 4).expect("term count") as usize;
        bytes[16..20].copy_from_slice(&((count + 1) as u32).to_ne_bytes());
        assert_eq!(decode_recall_term_store(&bytes), None);
    }

    #[test]
    fn a_fingerprint_with_multibyte_characters_survives_the_round_trip() {
        // The padding is computed on byte length, not character count; a store keyed by the wrong
        // number would shift every offset after it and read as corrupt.
        let mut original = store();
        original.fingerprint = "fp-東京-라면-é".to_string();
        assert_eq!(
            decode_recall_term_store(&encode_recall_term_store(&original)),
            Some(original)
        );
    }
}
