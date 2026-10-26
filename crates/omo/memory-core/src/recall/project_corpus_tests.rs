use super::{PROJECT_MEMORY_PATH_PREFIX, parse_project_memory_path, qualify_project_documents};
use crate::recall::provider::RecallDocument;

fn document(path: &str, description: &str, body: &str) -> RecallDocument {
    RecallDocument {
        path: path.to_string(),
        description: description.to_string(),
        body: body.to_string(),
    }
}

#[test]
fn given_project_documents_when_qualified_then_each_path_carries_the_identity_namespace() {
    let documents = vec![
        document(
            "reference/invoices.md",
            "Invoices",
            "Nightly invoice export.",
        ),
        document("reference/deploy.md", "Deploy", "Nightly deployment."),
    ];

    let qualified = qualify_project_documents("child-1234abcd", &documents);

    assert_eq!(
        qualified,
        vec![
            document(
                "@project/child-1234abcd/reference/invoices.md",
                "Invoices",
                "Nightly invoice export."
            ),
            document(
                "@project/child-1234abcd/reference/deploy.md",
                "Deploy",
                "Nightly deployment."
            ),
        ]
    );
    assert_eq!(
        qualified[0].path,
        format!("{PROJECT_MEMORY_PATH_PREFIX}child-1234abcd/reference/invoices.md")
    );
}

#[test]
fn given_a_qualified_path_when_parsed_then_the_identity_and_the_note_path_split_at_the_first_slash()
{
    let parsed = parse_project_memory_path("@project/child-1234abcd/reference/deploy.md")
        .expect("a qualified project path parses");
    assert_eq!(parsed.identity_id, "child-1234abcd");
    assert_eq!(parsed.path, "reference/deploy.md");

    let nested = parse_project_memory_path("@project/child-1234abcd/notes/2026/deploy.md")
        .expect("a nested project path parses");
    assert_eq!(nested.identity_id, "child-1234abcd");
    assert_eq!(nested.path, "notes/2026/deploy.md");
}

#[test]
fn given_a_path_outside_the_namespace_when_parsed_then_it_is_not_a_project_path() {
    assert!(parse_project_memory_path("reference/deploy.md").is_none());
    assert!(parse_project_memory_path("notes/@project/child-1234abcd/deploy.md").is_none());
    assert!(parse_project_memory_path("").is_none());
}

#[test]
fn given_a_qualified_path_missing_its_identity_or_its_note_path_when_parsed_then_it_is_rejected() {
    assert!(parse_project_memory_path("@project//reference/deploy.md").is_none());
    assert!(parse_project_memory_path("@project/child-1234abcd/").is_none());
    assert!(parse_project_memory_path("@project/child-1234abcd").is_none());
    assert!(parse_project_memory_path("@project/").is_none());
}

#[test]
fn given_a_qualified_path_when_reparsed_then_the_qualified_form_round_trips() {
    let document = document("reference/deploy.md", "Deploy", "Nightly deployment.");
    let qualified = qualify_project_documents("child-1234abcd", std::slice::from_ref(&document));

    let parsed = parse_project_memory_path(&qualified[0].path).expect("a qualified path parses");
    assert_eq!(parsed.identity_id, "child-1234abcd");
    assert_eq!(parsed.path, document.path);
}
