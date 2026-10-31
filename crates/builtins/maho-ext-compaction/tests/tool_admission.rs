use maho_core::compaction::compaction::estimate_tokens;
use maho_ext_compaction::tool_admission::*;
use serde_json::json;
fn tokens(text: &str) -> u64 {
    estimate_tokens(&json!({"role":"user","content":text,"timestamp":0}))
}
#[test]
fn cap_scales_at_five_percent() {
    assert_eq!(resolve_tool_result_admission_cap_tokens(200000), 10000);
}
#[test]
fn cap_has_fifty_thousand_ceiling() {
    assert_eq!(resolve_tool_result_admission_cap_tokens(1000000), 50000);
}
#[test]
fn cap_has_8192_floor() {
    assert_eq!(resolve_tool_result_admission_cap_tokens(64000), 8192);
}
#[test]
fn under_cap_text_passes_through() {
    let text = "small tool output\n".repeat(10);
    let result = admit_tool_result(&text, 200000);
    assert_eq!(result.text, text);
    assert!(!result.projected);
}
#[test]
fn over_cap_projection_is_deterministic_and_bounded() {
    let text = format!(
        "HEAD-SENTINEL-START\n{}\nTAIL-SENTINEL-END",
        "x".repeat(80000)
    );
    let first = admit_tool_result(&text, 200000);
    assert_eq!(admit_tool_result(&text, 200000), first);
    assert!(first.projected);
    assert!(first.text.starts_with("HEAD-SENTINEL-START\n"));
    assert!(first.text.ends_with("\nTAIL-SENTINEL-END"));
    assert!(tokens(&first.text) <= 10000);
    assert!(first.text.contains(TOOL_ADMISSION_MARKER_PREFIX));
}
#[test]
fn model_downswitch_reprojects_excerpt() {
    let text = format!("PROLOGUE\n{}\nEPILOGUE", "z".repeat(400000));
    let first = admit_tool_result(&text, 1000000);
    let second = admit_tool_result(&first.text, 64000);
    assert!(first.projected);
    assert!(second.projected);
    assert!(tokens(&second.text) <= 8192);
}
#[test]
fn model_visible_marker_cannot_bypass_cap() {
    let text = format!(
        "{TOOL_ADMISSION_MARKER_PREFIX} kept 10 of ~99 tokens]\n{}",
        "q".repeat(80000)
    );
    let result = admit_tool_result(&text, 200000);
    assert!(result.projected);
    assert_ne!(result.text, text);
    assert!(tokens(&result.text) <= 10000);
}
#[test]
fn zero_budget_projects_to_empty() {
    let result = admit_tool_result_within_budget("content", 0);
    assert!(result.projected);
    assert!(result.text.is_empty());
}
#[test]
fn split_surrogate_pair_keeps_upstream_lengths() {
    // senpi slices UTF-16 units; each cut half is one unit (U+FFFD here), never a six-char escape.
    let text = format!("a{}", "\u{1f600}".repeat(3000));
    let result = admit_tool_result_within_budget(&text, 1000);
    assert!(result.projected);
    assert_eq!(result.text.encode_utf16().count(), 3248);
    let (head, rest) = result.text.split_once('\n').expect("head");
    assert_eq!(head.encode_utf16().count(), 2398);
    assert!(head.ends_with('\u{fffd}'));
    assert!(rest.starts_with("[tool result projected: kept 800 of ~1501 tokens]\n\u{fffd}"));
}
#[test]
fn excerpt_budget_uses_upstream_double_ratio() {
    // floor(179 * (612 / 537)) is 203 in doubles, one below the exact quotient 204.
    let text = format!("{}{}", "A".repeat(512), ".".repeat(100));
    let result = admit_tool_result_within_budget(&text, 179);
    let (head, rest) = result.text.split_once('\n').expect("head");
    assert_eq!(head, "A".repeat(121));
    assert_eq!(
        rest,
        format!(
            "[tool result projected: kept 41 of ~537 tokens]\n{}",
            ".".repeat(40)
        )
    );
}
