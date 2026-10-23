use std::collections::BTreeMap;

use super::{RecallTermStore, decode_recall_term_store, encode_recall_term_store};
use crate::recall::bm25::StoredDocumentTerms;

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
