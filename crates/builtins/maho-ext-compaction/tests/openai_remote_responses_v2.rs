use std::collections::BTreeMap;
use maho_ext_compaction::openai_remote_responses_v2::*;
use serde_json::json;

#[test]
fn beta_header_normalizes_case_and_deduplicates_tokens() {
    let headers = BTreeMap::from([("X-Codex-Beta-Features".into(), " alpha,alpha, remote_compaction_v2 ".into()), ("authorization".into(), "Bearer credential".into())]);
    let next = with_remote_compaction_v2_header(&headers);
    assert_eq!(next["x-codex-beta-features"], "alpha,remote_compaction_v2");
    assert_eq!(next["authorization"], headers["authorization"]);
    assert!(!next.contains_key("X-Codex-Beta-Features"));
}

#[test]
fn payload_rewrite_keeps_options_and_appends_native_trigger() {
    let input = [json!({"role":"user","content":"request"})];
    let payload = rewrite_v2_payload(&json!({"model":"m","input":[]}), &input).unwrap();
    assert_eq!(payload["model"], "m");
    assert_eq!(payload["input"][0], input[0]);
    assert!(has_v2_trigger(&payload));
}

#[test]
fn malformed_payload_and_removed_trigger_are_rejected() {
    assert!(rewrite_v2_payload(&json!(null), &[]).is_none());
    assert!(!has_v2_trigger(&json!({"input":[{"type":"message"}]})));
}
