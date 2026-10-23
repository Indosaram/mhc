use super::{seed_recall_document_terms, snapshot_recall_document_terms};
use crate::recall::bm25::{RECALL_TERM_ACCESS, StoredDocumentTerms, recall_document_terms};
use crate::recall::provider::RecallDocument;

fn doc(body: &str) -> RecallDocument {
    RecallDocument {
        path: "n.md".to_string(),
        description: "transfer-unit".to_string(),
        body: body.to_string(),
    }
}

#[test]
fn a_seeded_document_snapshots_with_compact_ascending_ids() {
    let document = doc("zzseedalpha zzseedbeta zzseedalpha");
    // Stored ids deliberately descend against the term list so the remap must re-sort.
    let stored = StoredDocumentTerms {
        ids: vec![0, 1],
        counts: vec![1, 2],
        length: 3,
    };
    let terms = vec!["zzseedbeta".to_string(), "zzseedalpha".to_string()];
    seed_recall_document_terms(&RECALL_TERM_ACCESS, &terms, [(&document, stored)]);

    let snapshot =
        snapshot_recall_document_terms(&RECALL_TERM_ACCESS, std::slice::from_ref(&document))
            .expect("document has current terms");
    let only = &snapshot.documents[0];
    assert!(only.ids.windows(2).all(|pair| pair[0] < pair[1]));
    let by_term: Vec<(String, u32)> = only
        .ids
        .iter()
        .zip(&only.counts)
        .map(|(id, count)| (snapshot.terms[*id as usize].clone(), *count))
        .collect();
    assert!(by_term.contains(&("zzseedbeta".to_string(), 1)));
    assert!(by_term.contains(&("zzseedalpha".to_string(), 2)));
    assert_eq!(snapshot.terms.len(), 2);
}

#[test]
fn a_document_without_current_terms_has_no_snapshot() {
    let document = doc("zzunseenonly body text");
    assert!(snapshot_recall_document_terms(&RECALL_TERM_ACCESS, std::slice::from_ref(&document)).is_none());
    assert!(!recall_document_terms(&document).terms.is_empty());
}
