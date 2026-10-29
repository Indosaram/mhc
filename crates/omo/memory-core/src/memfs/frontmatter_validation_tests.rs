use super::*;

#[test]
fn a_well_formed_header_passes_both_checks() {
    let content = "---\ndescription: A memory block\nread_only: true\n---\nbody\n";
    assert_eq!(describe_frontmatter_violation(content), None);
    assert_eq!(describe_frontmatter_grammar_violation(content), None);
}

#[test]
fn missing_frontmatter_is_reported() {
    assert_eq!(
        describe_frontmatter_violation("no frontmatter here"),
        Some("missing frontmatter (must start with --- and close with ---)".to_string())
    );
}

#[test]
fn grammar_rejects_indented_continuations_comments_and_non_pairs() {
    assert_eq!(
        describe_header_grammar_violation("description: A\n  continued"),
        Some("indented continuation lines are not allowed (single-line scalars only): continued".to_string())
    );
    assert_eq!(
        describe_header_grammar_violation("description: A\n# a comment"),
        Some("comment lines are not allowed in frontmatter: # a comment".to_string())
    );
    assert_eq!(
        describe_header_grammar_violation("not a key value line"),
        Some("not a 'key: value' line: not a key value line".to_string())
    );
    assert_eq!(
        describe_header_grammar_violation("key:value\ndescription: A"),
        Some("not a 'key: value' line: key:value".to_string())
    );
}

#[test]
fn grammar_rejects_duplicate_keys_and_a_missing_description() {
    assert_eq!(
        describe_header_grammar_violation("description: A\ndescription: B"),
        Some("duplicate frontmatter key 'description'".to_string())
    );
    assert_eq!(
        describe_header_grammar_violation("read_only: true"),
        Some("missing required field 'description'".to_string())
    );
    assert_eq!(
        describe_header_grammar_violation("description:"),
        Some("'description' must not be empty".to_string())
    );
}

#[test]
fn values_must_be_safe_or_canonical_scalars() {
    assert_eq!(
        describe_header_grammar_violation("description: a: b"),
        Some(
            "'description' is not a safe YAML plain scalar (quote it, or remove ': ' and ' #'): a: b"
                .to_string()
        )
    );
    assert_eq!(
        describe_header_grammar_violation("description: A\nkind: a: b"),
        Some("'kind' is not a safe YAML plain scalar (quote it, or remove ': ' and ' #'): a: b".to_string())
    );
    assert_eq!(
        describe_header_grammar_violation("description: A\naliases: [1]"),
        Some("'aliases' must be a JSON array of strings: [1]".to_string())
    );
    assert_eq!(
        describe_header_grammar_violation("description: A\nnote: \"unterminated"),
        Some("'note' is not a valid quoted scalar (use JSON double quotes): \"unterminated".to_string())
    );
    assert_eq!(
        describe_header_grammar_violation("description: A\nnote: >"),
        Some("'note' must be a non-empty single line".to_string())
    );
    assert_eq!(describe_header_grammar_violation("description: A\nread_only: true"), None);
}

#[test]
fn description_contract_names_scaffolding_before_length_or_lines() {
    let scaffolding = describe_description_violation("a</description>\n<parameter name=\"file_text\">b");
    assert!(
        scaffolding
            .as_deref()
            .is_some_and(|message| message.contains("tool-call scaffolding"))
    );
    assert_eq!(
        describe_description_violation("line one\nline two"),
        Some("'description' must be a single line".to_string())
    );
    assert_eq!(
        describe_description_violation("   "),
        Some("'description' must not be empty".to_string())
    );
    let long = "x".repeat(MAX_DESCRIPTION_LENGTH + 1);
    assert_eq!(
        describe_description_violation(&long),
        Some(format!(
            "'description' exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
            MAX_DESCRIPTION_LENGTH + 1
        ))
    );
    assert_eq!(describe_description_violation(&"x".repeat(MAX_DESCRIPTION_LENGTH)), None);
}

#[test]
fn scaffolding_detection_matches_the_pinned_pattern() {
    assert_eq!(
        find_tool_call_scaffolding("</description>"),
        Some("</description>".to_string())
    );
    assert_eq!(
        find_tool_call_scaffolding("<parameter name=\"file_text\">"),
        Some("<parameter name=\"file_text\">".to_string())
    );
    assert_eq!(
        find_tool_call_scaffolding("</PARAMETER>"),
        Some("</PARAMETER>".to_string())
    );
    assert_eq!(find_tool_call_scaffolding("a plain description"), None);
    assert_eq!(find_tool_call_scaffolding("descriptions are fine"), None);
}

#[test]
fn a_header_violation_prefers_grammar_over_the_description_contract() {
    assert_eq!(
        describe_header_violation("description: A\n  leaked"),
        Some("indented continuation lines are not allowed (single-line scalars only): leaked".to_string())
    );
    assert_eq!(
        describe_header_violation("description: A"),
        None
    );
}
