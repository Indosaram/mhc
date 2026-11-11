//! The commit message a facts mutation writes, and the trailers a later audit reads back.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-commit-message.md`. The trailers are the only durable record of
//! *why* a fact exists and *what* the writer had to change to store it, so their spelling is a contract with
//! every reader that greps history — not a format choice.

/// The commit that lands one facts batch.
///
/// `stripped` and `evicted` are rendered as one trailer line each rather than a combined list, because a
/// reader looks up a single path or id and must be able to match it without parsing a comma-separated field.
pub fn commit_message(
    batch_id: &str,
    count: usize,
    stripped: &[String],
    evicted: &[String],
) -> String {
    let fact_word = if count == 1 { "fact" } else { "facts" };
    let mut message = format!(
        "chore(facts): extract {count} {fact_word}\n\nGenerated-By: facts-extractor\nOmo-Writer: facts-extractor\nOmo-Facts-Batch: {batch_id}"
    );
    for path in stripped {
        message.push_str(&format!("\nOmo-Provenance-Stripped: {path}"));
    }
    for id in evicted {
        message.push_str(&format!("\nOmo-Agreement-Evicted: {id}"));
    }
    message
}

/// How a recovery attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactsRecoveryResult {
    Committed {
        sha: String,
        affected_paths: Vec<String>,
    },
    /// Someone else changed the repository mid-apply. The plan's state was rolled back.
    ParentDirty { detail: Option<String> },
}

impl FactsRecoveryResult {
    pub fn outcome(&self) -> &str {
        match self {
            Self::Committed { .. } => "committed",
            Self::ParentDirty { .. } => "parent_dirty",
        }
    }

    pub fn sha(&self) -> Option<&str> {
        match self {
            Self::Committed { sha, .. } => Some(sha),
            Self::ParentDirty { .. } => None,
        }
    }

    /// Empty when nothing was committed: a dirty parent wrote no paths, so a caller must not report progress
    /// from this on an attempt that changed nothing.
    pub fn affected_paths(&self) -> &[String] {
        match self {
            Self::Committed { affected_paths, .. } => affected_paths,
            Self::ParentDirty { .. } => &[],
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Committed { .. } => None,
            Self::ParentDirty { detail } => detail.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_fact_is_singular() {
        let message = commit_message("b1", 1, &[], &[]);
        assert!(
            message.starts_with("chore(facts): extract 1 fact\n"),
            "{message}"
        );
        assert!(!message.contains("facts\n"), "{message}");
    }

    #[test]
    fn several_facts_are_plural() {
        assert!(commit_message("b1", 2, &[], &[]).starts_with("chore(facts): extract 2 facts\n"));
        assert!(commit_message("b1", 0, &[], &[]).starts_with("chore(facts): extract 0 facts\n"));
    }

    #[test]
    fn the_identity_trailers_are_always_present() {
        let message = commit_message("batch-9", 3, &[], &[]);
        assert!(
            message.contains("\n\nGenerated-By: facts-extractor\n"),
            "{message}"
        );
        assert!(
            message.contains("\nOmo-Writer: facts-extractor\n"),
            "{message}"
        );
        assert!(message.contains("\nOmo-Facts-Batch: batch-9"), "{message}");
    }

    #[test]
    fn every_stripped_path_gets_its_own_trailer() {
        let stripped = vec![
            "notes/facts/2026-11.md".to_owned(),
            "people/a/card.md".to_owned(),
        ];
        let message = commit_message("b1", 1, &stripped, &[]);
        assert!(
            message.contains("\nOmo-Provenance-Stripped: notes/facts/2026-11.md"),
            "{message}"
        );
        assert!(
            message.contains("\nOmo-Provenance-Stripped: people/a/card.md"),
            "{message}"
        );
        assert_eq!(message.matches("Omo-Provenance-Stripped:").count(), 2);
    }

    #[test]
    fn every_evicted_agreement_gets_its_own_trailer() {
        let evicted = vec!["a1".to_owned(), "a2".to_owned()];
        let message = commit_message("b1", 1, &[], &evicted);
        assert!(message.contains("\nOmo-Agreement-Evicted: a1"), "{message}");
        assert!(message.contains("\nOmo-Agreement-Evicted: a2"), "{message}");
    }

    #[test]
    fn a_message_without_extras_has_no_empty_trailers() {
        let message = commit_message("b1", 1, &[], &[]);
        assert!(!message.contains("Omo-Provenance-Stripped:"), "{message}");
        assert!(!message.contains("Omo-Agreement-Evicted:"), "{message}");
        assert!(!message.ends_with('\n'), "{message}");
    }

    #[test]
    fn trailers_are_appended_after_the_identity_block() {
        let message = commit_message("b1", 1, &["p".to_owned()], &["a".to_owned()]);
        let batch = message.find("Omo-Facts-Batch: b1").expect("batch");
        let stripped = message
            .find("Omo-Provenance-Stripped: p")
            .expect("stripped");
        let evicted = message.find("Omo-Agreement-Evicted: a").expect("evicted");
        assert!(batch < stripped, "{message}");
        assert!(stripped < evicted, "{message}");
    }

    fn committed() -> FactsRecoveryResult {
        FactsRecoveryResult::Committed {
            sha: "abc123".to_owned(),
            affected_paths: vec!["notes/facts/2026-11.md".to_owned()],
        }
    }

    fn parent_dirty(detail: Option<&str>) -> FactsRecoveryResult {
        FactsRecoveryResult::ParentDirty {
            detail: detail.map(str::to_owned),
        }
    }

    #[test]
    fn a_committed_result_exposes_its_sha_and_paths() {
        let result = committed();
        assert_eq!(result.outcome(), "committed");
        assert_eq!(result.sha(), Some("abc123"));
        assert_eq!(result.affected_paths(), ["notes/facts/2026-11.md"]);
        assert_eq!(result.detail(), None);
    }

    #[test]
    fn a_dirty_parent_reports_no_sha_and_no_paths() {
        for result in [
            parent_dirty(None),
            parent_dirty(Some("Foreign entry retained at a")),
        ] {
            assert_eq!(result.outcome(), "parent_dirty");
            assert_eq!(result.sha(), None);
            assert!(result.affected_paths().is_empty());
        }
    }

    #[test]
    fn a_dirty_parent_keeps_its_detail() {
        assert_eq!(
            parent_dirty(Some("Foreign entry retained at notes/a.md")).detail(),
            Some("Foreign entry retained at notes/a.md")
        );
        assert_eq!(parent_dirty(None).detail(), None);
    }
}
