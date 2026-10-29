//! `tools/output/transcript/read-bounded.test.ts`

use pretty_assertions::assert_eq;

use crate::tools::output::transcript::read_bounded::{
    MAX_TRANSCRIPT_SOURCE_BYTES, read_bounded_file_text,
};

#[test]
fn given_a_transcript_larger_than_the_source_budget_when_read_then_memory_is_capped_while_head_and_tail_survive()
 {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("large.jsonl");
    let content = format!(
        "{}MIDDLE{}",
        "a".repeat(MAX_TRANSCRIPT_SOURCE_BYTES),
        "z".repeat(MAX_TRANSCRIPT_SOURCE_BYTES)
    );
    std::fs::write(&path, content).expect("write");

    let result = read_bounded_file_text(&path, None)
        .expect("read")
        .expect("file exists");

    assert_eq!(result.truncated, true);
    assert!(result.text.len() <= MAX_TRANSCRIPT_SOURCE_BYTES + 1);
    assert!(result.text.starts_with('a'));
    assert!(result.text.ends_with('z'));
    assert!(!result.text.contains("MIDDLE"));
}
