//! Haystacks and BM25 index computed once per corpus `RecallCorpusCache` holds.
//!
//! The registry keeps a strong reference to each registered corpus, so its documents slice cannot
//! be freed and its address reused by another revision; slice identity alone is therefore a sound key.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::bm25::RecallBm25Index;
use super::provider::{RecallCorpus, RecallDocument};

#[derive(Default)]
pub(crate) struct CorpusDerived {
    pub(crate) haystacks: OnceLock<Vec<String>>,
    /// Rebuilt when the dictionary generation moved past the cached index.
    pub(crate) bm25: Mutex<Option<Arc<RecallBm25Index>>>,
    /// Told after a build of this corpus' index tokenized at least one document.
    pub(crate) encoded_listener: Mutex<Option<EncodedListener>>,
}

pub(crate) type EncodedListener = Arc<dyn Fn() + Send + Sync>;

struct RegisteredCorpus {
    corpus: Arc<RecallCorpus>,
    derived: Arc<CorpusDerived>,
}

static REGISTRY: Mutex<Vec<RegisteredCorpus>> = Mutex::new(Vec::new());

fn registry() -> std::sync::MutexGuard<'static, Vec<RegisteredCorpus>> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn register(corpus: &Arc<RecallCorpus>) {
    if corpus.documents.is_empty() {
        return;
    }
    let mut entries = registry();
    if entries
        .iter()
        .any(|entry| Arc::ptr_eq(&entry.corpus, corpus))
    {
        return;
    }
    entries.push(RegisteredCorpus {
        corpus: Arc::clone(corpus),
        derived: Arc::default(),
    });
}

pub(crate) fn unregister(corpus: &Arc<RecallCorpus>) {
    registry().retain(|entry| !Arc::ptr_eq(&entry.corpus, corpus));
}

pub(crate) fn derived_for(documents: &[RecallDocument]) -> Option<Arc<CorpusDerived>> {
    if documents.is_empty() {
        return None;
    }
    registry()
        .iter()
        .find(|entry| {
            entry.corpus.documents.as_ptr() == documents.as_ptr()
                && entry.corpus.documents.len() == documents.len()
        })
        .map(|entry| Arc::clone(&entry.derived))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use super::{derived_for, register, unregister};
    use crate::recall::provider::{RecallCorpus, RecallDocument};
    use crate::recall::select::{SelectRecallOptions, select_recall_candidates};

    fn corpus() -> Arc<RecallCorpus> {
        let document = |path: &str, body: &str| RecallDocument {
            path: path.to_string(),
            description: format!("about {path}"),
            body: body.to_string(),
        };
        Arc::new(RecallCorpus {
            revision: Some("rev".to_string()),
            documents: vec![
                document("a.md", "alpha beta gamma"),
                document("b.md", "beta only here"),
                document("c.md", "nothing relevant"),
            ],
        })
    }

    fn select(documents: &[RecallDocument]) -> Vec<crate::recall::select::RecallCandidate> {
        select_recall_candidates(
            documents,
            &["beta".to_string(), "alpha beta".to_string()],
            &SelectRecallOptions {
                max_items: 5,
                surfaced: BTreeSet::new(),
                exclude_paths: None,
                strategy: None,
                expansions: None,
            },
        )
    }

    #[test]
    fn registered_corpus_resolves_by_slice_identity_and_ranks_identically() {
        let corpus = corpus();
        let expected = select(&corpus.documents);
        assert!(derived_for(&corpus.documents).is_none());

        register(&corpus);
        let derived = derived_for(&corpus.documents).expect("registered corpus resolves");
        assert!(derived_for(&corpus.documents[..2]).is_none());
        assert!(derived_for(&corpus.documents.clone()).is_none());
        assert_eq!(select(&corpus.documents), expected);
        assert!(derived.haystacks.get().is_some());
        assert_eq!(select(&corpus.documents), expected);

        unregister(&corpus);
        assert!(derived_for(&corpus.documents).is_none());
        assert_eq!(select(&corpus.documents), expected);
    }
}
