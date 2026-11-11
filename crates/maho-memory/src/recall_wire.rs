//! Session-scoped recall: the one place that combines the ledger and the selector.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-recall-wiring.md`. The ledger knows what a session
//! was already shown; the selector knows what is worth showing now. Neither is useful alone, and the
//! rule that joins them - never repeat, never go over budget, record exactly what was returned - is
//! the whole behavioural contract of recall.
use crate::bm25::RecallDocument;
use crate::ledger::{SessionLedger, Surfaced, content_hash};
use crate::select::{SelectOptions, select_candidates};

/// One candidate as recall reports it, with the content the caller must have to show it.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRecallHit {
    pub path: String,
    pub description: String,
    pub excerpt: String,
    pub score: f64,
    pub content: String,
}

/// What one recall pass produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionRecall {
    /// Best first.
    pub hits: Vec<SessionRecallHit>,
    pub unreadable: Vec<String>,
}

/// Run one recall pass.
///
/// Paths already recorded in the ledger are excluded before scoring, so a memory the session has
/// seen never consumes part of the budget. Only returned hits are recorded, in the same call, so a
/// caller that crashes between recall and display loses one turn of memory rather than permanently
/// suppressing a memory the user never saw.
pub fn recall_for_session(
    ledger: &mut SessionLedger,
    documents: &[RecallDocument],
    queries: &[String],
    max_items: usize,
    at: &str,
) -> SessionRecall {
    let known: Vec<(String, String)> = documents
        .iter()
        .map(|document| (document.path.clone(), content_hash(&document.body)))
        .collect();
    let already_current: std::collections::BTreeSet<String> = known
        .into_iter()
        .filter(|(path, hash)| ledger.is_current(path, hash))
        .map(|(path, _)| path)
        .collect();

    let selected = select_candidates(
        documents,
        queries,
        &SelectOptions {
            max_items,
            // Only paths whose content is unchanged are suppressed: the ledger stores a hash per
            // path, and an edited memory is information the session has not seen yet.
            surfaced: already_current,
            exclude_paths: None,
            expansions: None,
            strategy: None,
        },
    );

    let mut hits = Vec::with_capacity(selected.len());
    let mut unreadable = Vec::new();
    for candidate in selected {
        match documents.iter().find(|d| d.path == candidate.path) {
            Some(document) => hits.push(SessionRecallHit {
                path: candidate.path,
                description: candidate.description,
                excerpt: candidate.excerpt,
                score: candidate.score,
                content: document.body.clone(),
            }),
            None => unreadable.push(candidate.path),
        }
    }

    let recorded: Vec<Surfaced> = hits
        .iter()
        .map(|hit| Surfaced {
            path: hit.path.clone(),
            hash: content_hash(&hit.content),
        })
        .collect();
    ledger.mark_surfaced(&recorded, at);

    SessionRecall { hits, unreadable }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(path: &str, body: &str) -> RecallDocument {
        RecallDocument {
            path: path.to_owned(),
            description: format!("about {path}"),
            body: body.to_owned(),
        }
    }

    fn documents() -> Vec<RecallDocument> {
        vec![
            document("notes/rust.md", "Rust ownership rules and borrowing"),
            document("notes/git.md", "Git rebase workflow for shared branches"),
            document("notes/한글.md", "한국어 메모리 노트"),
        ]
    }

    fn query(term: &str) -> Vec<String> {
        vec![term.to_owned()]
    }

    fn queries(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|term| (*term).to_owned()).collect()
    }

    #[test]
    fn a_query_returns_matching_documents() {
        let mut ledger = SessionLedger::new();
        let recall = recall_for_session(&mut ledger, &documents(), &query("rust"), 5, "t");
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].path, "notes/rust.md");
    }

    #[test]
    fn returned_hits_are_recorded_as_surfaced() {
        let mut ledger = SessionLedger::new();
        let recall = recall_for_session(&mut ledger, &documents(), &query("rust"), 5, "t");
        let hit = &recall.hits[0];
        assert!(ledger.is_current(&hit.path, &content_hash(&hit.content)));
    }

    #[test]
    fn a_second_pass_does_not_repeat_a_surfaced_memory() {
        let mut ledger = SessionLedger::new();
        let first = recall_for_session(&mut ledger, &documents(), &query("rust"), 5, "t1");
        assert_eq!(first.hits.len(), 1);
        let second = recall_for_session(&mut ledger, &documents(), &query("rust"), 5, "t2");
        assert!(second.hits.is_empty());
    }

    #[test]
    fn an_edited_memory_is_surfaced_again() {
        let mut ledger = SessionLedger::new();
        recall_for_session(&mut ledger, &documents(), &query("rust"), 5, "t1");
        let edited = vec![document("notes/rust.md", "Rust ownership rules, revised")];
        let second = recall_for_session(&mut ledger, &edited, &query("rust"), 5, "t2");
        assert_eq!(second.hits.len(), 1);
    }

    #[test]
    fn the_budget_caps_the_number_of_hits() {
        let mut ledger = SessionLedger::new();
        let recall = recall_for_session(&mut ledger, &documents(), &query("notes"), 2, "t");
        assert_eq!(recall.hits.len(), 2);
    }

    #[test]
    fn a_zero_budget_returns_nothing_and_records_nothing() {
        let mut ledger = SessionLedger::new();
        let recall = recall_for_session(&mut ledger, &documents(), &query("notes"), 0, "t");
        assert!(recall.hits.is_empty());
        assert!(ledger.is_empty());
    }

    #[test]
    fn a_dropped_document_leaves_only_the_kept_ones_recorded() {
        let mut ledger = SessionLedger::new();
        let recall = recall_for_session(
            &mut ledger,
            &documents(),
            &queries(&["rust", "git"]),
            1,
            "t",
        );
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn a_korean_document_is_found_by_a_korean_query() {
        let mut ledger = SessionLedger::new();
        let recall = recall_for_session(&mut ledger, &documents(), &query("한국어"), 5, "t");
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].path, "notes/한글.md");
    }

    #[test]
    fn hits_score_higher_than_or_equal_to_zero() {
        let mut ledger = SessionLedger::new();
        let recall = recall_for_session(&mut ledger, &documents(), &query("rust"), 5, "t");
        assert!(recall.hits.iter().all(|hit| hit.score >= 0.0));
    }

    #[test]
    fn a_query_matching_nothing_returns_nothing() {
        let mut ledger = SessionLedger::new();
        let recall = recall_for_session(&mut ledger, &documents(), &query("kubernetes"), 5, "t");
        assert!(recall.hits.is_empty());
        assert!(recall.unreadable.is_empty());
    }
}
