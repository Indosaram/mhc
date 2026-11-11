//! Project-scoped recall: a child project's notes join a session under a qualified path.
//!
//! Spec: `docs/cleanroom/specs/memory-project-corpus.md`. The namespace matters because recall keys its ledger
//! and surfaced set by path. Two identities that both have `reference/deploy.md` must stay two distinct notes,
//! and a qualified path must remain a plain relative path so every existing path check still applies to it.

use crate::bm25::RecallDocument;

pub const PROJECT_MEMORY_PATH_PREFIX: &str = "@project/";

pub fn qualify_project_documents(
    identity_id: &str,
    documents: &[RecallDocument],
) -> Vec<RecallDocument> {
    documents
        .iter()
        .map(|document| RecallDocument {
            path: format!(
                "{PROJECT_MEMORY_PATH_PREFIX}{identity_id}/{}",
                document.path
            ),
            description: document.description.clone(),
            body: document.body.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMemoryPath {
    pub identity_id: String,
    pub path: String,
}

/// Split a qualified path back into owner and note path, at the **first** slash after the identity.
///
/// A note path may itself be nested, so splitting at the last slash or on whitespace would attribute
/// `notes/2026/deploy.md` to an identity that does not exist. A path outside the namespace, or one with an
/// empty identity or empty note path, is not a project path at all.
pub fn parse_project_memory_path(path: &str) -> Option<ProjectMemoryPath> {
    let rest = path.strip_prefix(PROJECT_MEMORY_PATH_PREFIX)?;
    let slash = rest.find('/')?;
    if slash == 0 || slash == rest.len() - 1 {
        return None;
    }
    Some(ProjectMemoryPath {
        identity_id: rest[..slash].to_owned(),
        path: rest[slash + 1..].to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(path: &str, description: &str, body: &str) -> RecallDocument {
        RecallDocument {
            path: path.to_owned(),
            description: description.to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn qualification_prefixes_every_path_with_the_identity() {
        let qualified = qualify_project_documents(
            "child-1234",
            &[
                document("reference/invoices.md", "Invoices", "Nightly."),
                document("notes/deploy.md", "Deploy", "Nightly."),
            ],
        );
        assert_eq!(
            qualified[0].path,
            "@project/child-1234/reference/invoices.md"
        );
        assert_eq!(qualified[1].path, "@project/child-1234/notes/deploy.md");
    }

    #[test]
    fn qualification_preserves_description_and_body() {
        let qualified = qualify_project_documents(
            "child-1234",
            &[document("a.md", "description here", "body here")],
        );
        assert_eq!(qualified[0].description, "description here");
        assert_eq!(qualified[0].body, "body here");
    }

    #[test]
    fn qualification_keeps_the_input_length() {
        let documents = [
            document("a.md", "a", "a"),
            document("b.md", "b", "b"),
            document("c.md", "c", "c"),
        ];
        assert_eq!(
            qualify_project_documents("child", &documents).len(),
            documents.len()
        );
    }

    #[test]
    fn two_identities_with_the_same_note_path_stay_distinct() {
        let first = qualify_project_documents("a", &[document("reference/x.md", "d", "b")]);
        let second = qualify_project_documents("b", &[document("reference/x.md", "d", "b")]);
        assert_ne!(first[0].path, second[0].path);
    }

    #[test]
    fn a_qualified_path_round_trips() {
        let qualified = qualify_project_documents("child-9", &[document("notes/a.md", "d", "b")]);
        let parsed = parse_project_memory_path(&qualified[0].path).expect("parses");
        assert_eq!(parsed.identity_id, "child-9");
        assert_eq!(parsed.path, "notes/a.md");
    }

    #[test]
    fn a_nested_note_path_splits_at_the_first_slash() {
        let parsed =
            parse_project_memory_path("@project/child-1/notes/2026/deploy.md").expect("parses");
        assert_eq!(parsed.identity_id, "child-1");
        assert_eq!(parsed.path, "notes/2026/deploy.md");
    }

    #[test]
    fn a_path_outside_the_namespace_is_not_a_project_path() {
        assert!(parse_project_memory_path("notes/deploy.md").is_none());
        assert!(parse_project_memory_path("@projectx/child/a.md").is_none());
    }

    #[test]
    fn a_namespace_without_an_identity_or_a_note_is_not_a_project_path() {
        assert!(parse_project_memory_path("@project/").is_none());
        assert!(parse_project_memory_path("@project/child").is_none());
        assert!(parse_project_memory_path("@project//notes/a.md").is_none());
        assert!(parse_project_memory_path("@project/child/").is_none());
    }

    #[test]
    fn a_qualified_path_stays_a_plain_relative_path() {
        let qualified = qualify_project_documents("child", &[document("notes/a.md", "d", "b")]);
        let path = &qualified[0].path;
        assert!(!path.starts_with('/'), "{path}");
        assert!(!path.split('/').any(|segment| segment == ".."), "{path}");
    }
}
