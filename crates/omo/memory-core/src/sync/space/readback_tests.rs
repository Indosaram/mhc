use super::tests::{files, state};
use super::*;

fn block(header: &str) -> String {
    format!(
        "\n<!-- memory-sync 2026-10-15: the other machine had different frontmatter for n.md; kept here so no fact is lost:\n{header}-->\n"
    )
}

#[test]
fn glued_separated_and_leading_blocks_preserve_neighbor_boundaries() {
    // Given
    let marker = block("---\nk: v\n---\n");
    let cases = [
        (format!("line1\n{}line2\n", &marker[1..]), "line1\nline2\n"),
        (format!("line1\n{marker}line2\n"), "line1\nline2\n"),
        (format!("{}tail\n", &marker[1..]), "tail\n"),
    ];
    for (input, expected) in cases {
        // When
        let result = strip_t2_blocks(&input);
        // Then
        assert_eq!(result.body, expected);
        assert_eq!(result.entries.len(), 1);
        assert!(result.malformed.is_empty());
    }
}

#[test]
fn null_markers_do_not_remove_keys_and_lookalike_comments_are_untouched() {
    // Given
    let marker = block("---\nobsolete: null\n---\n");
    let ordinary = "a\n<!-- memory-sync 2026-10-15: something else entirely -->\nb\n";
    // When
    let stripped = strip_t2_blocks(&format!("x\n{marker}"));
    // Then
    assert_eq!(stripped.body, "x\n");
    assert!(stripped.entries.is_empty());
    let untouched = strip_t2_blocks(ordinary);
    assert_eq!(untouched.body, ordinary);
    assert!(untouched.entries.is_empty());
    assert!(untouched.malformed.is_empty());
}

#[test]
fn metadata_allowances_are_bounded_and_multiline_values_are_not_vouched_linewise() {
    // Given
    let with_key = "---\nk: v\n---\nx\n";
    let marker = block("---\nk: \"v\"\n---\n");
    let fused = block("---\nk: \"v\"\n---");
    // When / Then
    for source in [format!("x\n{marker}"), format!("x\n{fused}")] {
        assert_eq!(
            read_back::first_missing_fact(with_key, &source, &std::collections::BTreeMap::new()),
            None
        );
    }
    for (rendered, source, reason) in [
        (
            "x\n",
            "x\n----->\n",
            "missing line \"----->\" (1 dropped, 0 allowed as metadata)",
        ),
        (
            "x\n",
            "x\n\n",
            "missing line \"\" (1 dropped, 0 allowed as metadata)",
        ),
        (
            "x\n",
            "---\nk: v\n---\nx\n---\n",
            "missing line \"---\" (3 dropped, 2 allowed as metadata)",
        ),
        (
            "---\ntags: [a]\n---\nx\n",
            "---\ntags:\n  - a\n---\nx\n",
            "missing line \"tags:\"",
        ),
    ] {
        assert_eq!(
            read_back::first_missing_fact(rendered, source, &std::collections::BTreeMap::new())
                .as_deref(),
            Some(reason)
        );
    }
    assert_eq!(
        read_back::first_missing_fact(
            "---\ntags:\n  - a\n  - b\n---\nx\n",
            "---\ntags: [a, b]\n---\nx\n",
            &std::collections::BTreeMap::new()
        ),
        None
    );
}

#[test]
fn crlf_side_and_line_deletion_are_normalized_without_duplicate_shared_lines() {
    // Given
    let doc = super::tests::seed("x\ny\n");
    let sources = files("x\r\ny\r\nz\r\n");
    // When
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    // Then
    assert_eq!(plan.rendered, "x\ny\nz\n");
    assert_eq!(plan.removal_candidates, [sources[0].path.clone()]);
    let doc = super::tests::seed("a\nb\nc\n");
    let mut baseline = state(&doc);
    baseline.last_materialised = baseline.current_text.clone();
    for text in ["a\nc\n", "a\r\nc\r\n"] {
        let plan = plan_adoption(
            &[Snapshot {
                path: "n.md".into(),
                text: text.into(),
            }],
            &baseline,
        )
        .expect("plan");
        assert!(matches!(plan.action, AdoptionAction::None));
    }
}
#[test]
fn joined_register_lines_count_only_inside_the_result_header() {
    // Given: a body line that looks like the joined register key line
    let rendered = "---\nk: \"a / b\"\n---\nk: \"a / b\"\n";
    let source = "k: \"a / b\"\n";
    let registers = std::collections::BTreeMap::from([(
        "k".to_string(),
        vec!["a".to_string(), "b".to_string()],
    )]);
    // When / Then: the body line is held text, so the source line is held
    assert_eq!(
        read_back::first_missing_fact(rendered, source, &registers),
        None
    );
}
