use super::*;

fn document(description: &str, body: &str) -> RecallDocument {
    RecallDocument {
        path: "notes/x.md".to_string(),
        description: description.to_string(),
        body: body.to_string(),
    }
}

#[test]
fn the_haystack_is_the_normalized_description_then_body() {
    assert_eq!(
        normalized_haystack(&document("Deploy Notes", "Rollbacks  are   listed\nhere")),
        "deploy notes rollbacks are listed here"
    );
}

#[test]
fn the_description_and_body_are_joined_by_a_single_separator() {
    assert_eq!(
        normalized_haystack(&document("Desc", "Body")),
        "desc body"
    );
}

#[test]
fn an_empty_body_leaves_the_description_with_a_trailing_separator_collapsed() {
    assert_eq!(normalized_haystack(&document("Only", "")), "only");
}
