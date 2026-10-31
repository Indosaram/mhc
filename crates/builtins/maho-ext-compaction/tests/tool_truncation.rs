use maho_ext_api::{ToolContent, ToolResult};
use maho_ext_compaction::tool_truncation::*;
fn text(result: &ToolResult) -> &str {
    match &result.content[0] {
        ToolContent::Text { text, .. } => text,
        ToolContent::Image { .. } => panic!("expected text"),
    }
}
#[test]
fn oversized_output_is_truncated_with_marker() {
    let result = ToolResult::text("X".repeat(50 * 1024));
    let output = truncate_oversized_tool_results(&[result]);
    assert!(text(&output[0]).contains("<truncated:51200 bytes original"));
    assert!(text(&output[0]).len() < 50 * 1024);
}
#[test]
fn recent_small_outputs_are_preserved() {
    let inputs = [
        ToolResult::text("X".repeat(40000)),
        ToolResult::text("recent-output-A"),
        ToolResult::text("recent-output-B"),
    ];
    let output = truncate_oversized_tool_results(&inputs);
    assert_eq!(output[1..], inputs[1..]);
}
#[test]
fn truncation_is_idempotent() {
    let first = truncate_oversized_tool_results(&[ToolResult::text("X".repeat(60000))]);
    assert_eq!(truncate_oversized_tool_results(&first), first);
}
#[test]
fn generous_budget_preserves_actionable_marker() {
    let result = ToolResult::text("Beginning <truncated:81920 bytes original; guidance>");
    assert_eq!(
        pre_prune_tool_outputs_to_budget(std::slice::from_ref(&result), 1000000),
        vec![result]
    );
}
#[test]
fn legacy_marker_prevents_double_truncation() {
    let result = ToolResult::text(format!(
        "{}\n<truncated:123 bytes original>\n{}",
        "A".repeat(60000),
        "B".repeat(60000)
    ));
    assert_eq!(
        truncate_oversized_tool_results(std::slice::from_ref(&result)),
        vec![result]
    );
}
#[test]
fn second_pass_reduces_legacy_marker_to_marker_only() {
    let result = ToolResult::text(format!(
        "{}\n<truncated:123 bytes original>\n{}",
        "A".repeat(60000),
        "B".repeat(60000)
    ));
    assert_eq!(
        text(&pre_prune_tool_outputs_to_budget(&[result], 1)[0]),
        "<truncated:123 bytes original>"
    );
}
#[test]
fn second_pass_preserves_entire_actionable_marker() {
    let output = pre_prune_tool_outputs_to_budget(&[ToolResult::text("X".repeat(60000))], 1);
    assert!(text(&output[0]).starts_with("<truncated:60000 bytes original"));
    assert!(text(&output[0]).ends_with('>'));
    assert!(!text(&output[0]).contains('\n'));
}
#[test]
fn actionable_marker_prevents_double_truncation() {
    let result = ToolResult::text(format!(
        "{}\n<truncated:123 bytes original; guidance>\n{}",
        "A".repeat(60000),
        "B".repeat(60000)
    ));
    assert_eq!(
        truncate_oversized_tool_results(std::slice::from_ref(&result)),
        vec![result]
    );
}
#[test]
fn threshold_uses_utf8_bytes() {
    let result = ToolResult::text("\u{ac00}".repeat(2000));
    let output = truncate_oversized_tool_results(&[result]);
    assert!(text(&output[0]).contains("<truncated:6000 bytes original"));
}
#[test]
fn images_and_metadata_are_preserved() {
    let result = ToolResult {
        content: vec![
            ToolContent::text("X".repeat(60000)),
            ToolContent::Image {
                data: "bytes".into(),
                mime_type: "image/png".into(),
            },
        ],
        details: Some(serde_json::json!({"origin":"tool"})),
        structured_content: None,
        is_error: Some(true),
    };
    let output = pre_prune_tool_outputs_to_budget(std::slice::from_ref(&result), 1);
    assert_eq!(output[0].content[1], result.content[1]);
    assert_eq!(output[0].details, result.details);
    assert_eq!(output[0].is_error, result.is_error);
}
#[test]
fn split_surrogate_pair_keeps_upstream_head_and_tail_lengths() {
    // senpi cuts at 800 and length-400 UTF-16 units even inside a surrogate pair.
    let input = format!(
        "{}\u{1f600}{}\u{1f600}{}",
        "a".repeat(799),
        "b".repeat(3000),
        "c".repeat(399)
    );
    let output = truncate_oversized_tool_results(&[ToolResult::text(input)]);
    let out = text(&output[0]);
    assert_eq!(out.encode_utf16().count(), 1378);
    let (head, rest) = out.split_once('\n').expect("head");
    assert_eq!(head, format!("{}\u{fffd}", "a".repeat(799)));
    assert!(rest.starts_with("<truncated:4206 bytes original;"));
    assert_eq!(
        rest.rsplit_once('\n').expect("tail").1,
        format!("\u{fffd}{}", "c".repeat(399))
    );
}
