use maho_core::compaction::{compaction::estimate_tokens, settings::default_compaction_settings};
use maho_ext_compaction::{orchestration::*, tool_admission::*};
use serde_json::{Value, json};
fn geometry() -> CompactionGeometry {
    CompactionGeometry {
        threshold_tokens: 80000.,
        lead_tokens: 10000.,
        reserve_tokens: 10000.,
    }
}
fn result(content: Value) -> Value {
    json!({"role":"toolResult","toolCallId":"call","toolName":"read","content":content,"isError":false,"timestamp":1})
}
#[test]
fn grace_defers_when_in_flight_and_inside_cap() {
    assert!(should_defer_grace_band(
        82000.,
        geometry(),
        100000.,
        true,
        Some(true)
    ));
}
#[test]
fn grace_blocks_when_past_cap_or_disabled() {
    assert!(!should_defer_grace_band(
        91000.,
        geometry(),
        100000.,
        true,
        Some(true)
    ));
    assert!(!should_defer_grace_band(
        82000.,
        geometry(),
        100000.,
        true,
        Some(false)
    ));
}
#[test]
fn reminder_is_leased_once_without_changing_restoration() {
    let input = [
        json!({"role":"user","content":[{"type":"text","text":"real prompt"}],"timestamp":1}),
        json!({"role":"custom","customType":"compaction-restoration","content":"restore checkpoint","display":false,"timestamp":2}),
    ];
    let output = inject_token_budget_reminder(&input, Some("budget reminder"));
    assert_eq!(output[0]["content"].as_array().expect("content").len(), 2);
    assert_eq!(output[1], input[1]);
    assert_eq!(
        inject_token_budget_reminder(&output, Some("budget reminder")),
        output
    );
    assert_eq!(
        inject_token_budget_reminder(&input[1..], Some("budget reminder")),
        input[1..]
    );
}
#[test]
fn configured_lead_is_clamped_consistently() {
    let mut settings = default_compaction_settings();
    for (lead, expected) in [(None, 17500.), (Some(1.), 8192.), (Some(1000000.), 32768.)] {
        settings.ideal.speculative_lead_tokens = lead;
        let g = resolve_compaction_geometry(200000., &settings, None);
        assert_eq!(g.threshold_tokens, 140000.);
        assert_eq!(g.lead_tokens, expected);
    }
}
#[test]
fn markers_do_not_bypass_admission() {
    let text = format!(
        "head\n{TOOL_ADMISSION_MARKER_PREFIX} kept 10 of ~99 tokens]\n{}\ntail",
        "x".repeat(100000)
    );
    let output = admit_context_tool_results(&[result(json!(text))], 100000, true);
    assert_ne!(output[0]["content"], json!(text));
}
fn multipart() -> Value {
    result(
        json!([{"type":"text","text":"a".repeat(100000)},{"type":"text","text":"b".repeat(100000)}]),
    )
}
#[test]
fn multipart_text_shares_one_cap() {
    let output = admit_context_tool_results(&[multipart()], 200000, true);
    let retained = output[0]["content"]
        .as_array()
        .expect("content")
        .iter()
        .map(|b| estimate_tokens(&json!({"role":"user","content":b["text"]})))
        .sum::<u64>();
    assert!(retained <= resolve_tool_result_admission_cap_tokens(200000));
}
#[test]
fn admitted_multipart_is_idempotent() {
    let output = admit_context_tool_results(&[multipart()], 200000, true);
    assert_eq!(admit_context_tool_results(&output, 200000, true), output);
}
#[test]
fn under_cap_multipart_is_unchanged() {
    let input = [result(
        json!([{"type":"text","text":"first"},{"type":"image","mimeType":"image/png","data":"IMAGE"},{"type":"text","text":"second"}]),
    )];
    assert_eq!(admit_context_tool_results(&input, 200000, true), input);
}
#[test]
fn oversized_text_projection_preserves_surrounding_blocks() {
    let input = [result(
        json!([{"type":"text","text":"before"},{"type":"image","mimeType":"image/png","data":"FIRST"},{"type":"text","text":"x".repeat(100000)},{"type":"image","mimeType":"image/jpeg","data":"SECOND"},{"type":"text","text":"after"}]),
    )];
    let output = admit_context_tool_results(&input, 100000, true);
    for i in [0, 1, 3, 4] {
        assert_eq!(output[0]["content"][i], input[0]["content"][i]);
    }
    assert_ne!(output[0]["content"][2], input[0]["content"][2]);
}
#[test]
fn single_result_marker_is_not_admission_state() {
    let marked = format!(
        "head\n{TOOL_ADMISSION_MARKER_PREFIX} kept 10 of ~99 tokens]\n{}\ntail",
        "x".repeat(100000)
    );
    let (text, admitted) = admit_context_tool_result(&marked, 100000);
    assert!(admitted);
    assert_ne!(text, marked);
}
#[test]
fn single_result_split_surrogate_keeps_upstream_lengths() {
    let (text, admitted) =
        admit_context_tool_result(&format!("a{}", "\u{1f600}".repeat(20000)), 100000);
    assert!(admitted);
    assert_eq!(text.encode_utf16().count(), 26265);
    let (head, rest) = text.split_once('\n').expect("head");
    assert_eq!(head.encode_utf16().count(), 19659);
    assert!(rest.starts_with("[tool result projected: kept 6554 of ~10001 tokens]\n"));
}
#[test]
fn reminder_alone_becomes_a_hidden_before_prompt_message() {
    let message = resolve_before_agent_start_message(
        None,
        Some("[context budget] x"),
        Some(true),
        Some("initial"),
    )
    .expect("message");
    assert_eq!(
        message,
        json!({"customType":"compaction-token-budget","display":false,"content":"[context budget] x","placement":"before-prompt","details":{"tokenBudgetReminderEpoch":"initial"}})
    );
}
#[test]
fn reminder_joins_an_existing_message_and_keeps_its_details() {
    let base = json!({"customType":"restore","display":true,"content":"restore","details":{"a":1}});
    let message = resolve_before_agent_start_message(Some(&base), Some("budget"), None, Some("c1"))
        .expect("message");
    assert_eq!(message["content"], "restore\n\nbudget");
    assert_eq!(message["placement"], "before-prompt");
    assert_eq!(
        message["details"],
        json!({"a":1,"tokenBudgetReminderEpoch":"c1"})
    );
    assert_eq!(message["display"], true);
}
#[test]
fn disabled_or_missing_reminder_returns_the_message_unchanged() {
    let base = json!({"customType":"restore","content":"restore"});
    assert_eq!(
        resolve_before_agent_start_message(Some(&base), Some("budget"), Some(false), Some("c1")),
        Some(base.clone())
    );
    assert_eq!(
        resolve_before_agent_start_message(Some(&base), None, Some(true), Some("c1")),
        Some(base)
    );
    assert_eq!(
        resolve_before_agent_start_message(None, Some("budget"), Some(false), Some("c1")),
        None
    );
}
