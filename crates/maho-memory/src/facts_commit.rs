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
}
