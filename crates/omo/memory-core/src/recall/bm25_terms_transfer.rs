//! Moves document terms between the live BM25 index and a persisted store (pin
//! `recall/bm25-terms-transfer.ts`). It reaches the index through [`RecallTermAccess`].

use super::bm25::{RecallTermAccess, RecallTermsSnapshot, StoredDocumentTerms};
use super::provider::RecallDocument;

/// Adopts stored terms for documents that have none yet, so building their index tokenizes nothing.
/// Ids index `terms`; in a process whose dictionary is still empty they become the dictionary's ids
/// as they are, otherwise each document is moved onto the dictionary's ids and re-sorted.
pub fn seed_recall_document_terms<'a>(
    access: &RecallTermAccess,
    terms: &[String],
    entries: impl IntoIterator<Item = (&'a RecallDocument, StoredDocumentTerms)>,
) {
    let mapping: Vec<u32> = terms.iter().map(|term| access.intern(term)).collect();
    let identity = mapping
        .iter()
        .enumerate()
        .all(|(position, id)| *id as usize == position);
    for (document, stored) in entries {
        let stored = if identity {
            stored
        } else {
            remap(&stored, &mapping)
        };
        access.adopt(document, stored);
    }
}

fn remap(stored: &StoredDocumentTerms, mapping: &[u32]) -> StoredDocumentTerms {
    let moved = |position: usize| -> u32 {
        stored
            .ids
            .get(position)
            .and_then(|id| mapping.get(*id as usize))
            .copied()
            .unwrap_or(0)
    };
    let mut order: Vec<usize> = (0..stored.ids.len()).collect();
    order.sort_by_key(|position| moved(*position));
    StoredDocumentTerms {
        ids: order.iter().map(|position| moved(*position)).collect(),
        counts: order
            .iter()
            .map(|position| stored.counts.get(*position).copied().unwrap_or(0))
            .collect(),
        length: stored.length,
    }
}

/// The current terms of `documents` against a term list of only the terms they use, ids kept in
/// dictionary order so each document's ids stay ascending. `None` while any of them has no current
/// terms, as after a dictionary replacement before the next build.
pub fn snapshot_recall_document_terms(
    access: &RecallTermAccess,
    documents: &[RecallDocument],
) -> Option<RecallTermsSnapshot> {
    let mut encoded = Vec::with_capacity(documents.len());
    for document in documents {
        encoded.push(access.current(document)?);
    }
    let size = access.vocabulary_size();
    let mut compact = vec![0u32; size];
    let mut used = vec![false; size];
    for terms in &encoded {
        for id in &terms.ids {
            *used.get_mut(*id as usize)? = true;
        }
    }
    let mut terms = Vec::new();
    for (id, flag) in used.iter().enumerate() {
        if !flag {
            continue;
        }
        compact[id] = u32::try_from(terms.len()).ok()?;
        terms.push(access.term(u32::try_from(id).ok()?).unwrap_or_default());
    }
    let documents = encoded
        .iter()
        .map(|document| StoredDocumentTerms {
            ids: document
                .ids
                .iter()
                .map(|id| compact.get(*id as usize).copied().unwrap_or(0))
                .collect(),
            counts: document.counts.clone(),
            length: document.length,
        })
        .collect();
    Some(RecallTermsSnapshot { terms, documents })
}

#[cfg(test)]
#[path = "bm25_terms_transfer_tests.rs"]
mod tests;
