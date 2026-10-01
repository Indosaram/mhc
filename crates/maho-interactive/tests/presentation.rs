use maho_interactive::{theme::{Theme, ColorMode}, session_info_format::format_session_info};
use serde_json::json;
fn theme() -> Theme { Theme::builtin("dark", ColorMode::Truecolor).expect("valid test fixture") }
fn stats() -> serde_json::Value { json!({"sessionFile":"/tmp/session.jsonl","sessionId":"session-abc-123","userMessages":1,"assistantMessages":1,"toolCalls":0,"toolResults":0,"totalMessages":2,"tokens":{"input":100,"output":200,"cacheRead":0,"cacheWrite":0,"total":300},"cost":0}) }
fn plain(stats: &serde_json::Value) -> String { maho_tui::utils::strip_terminal_sequences(&format_session_info(stats, None, &theme())) }
#[test]
fn session_cost_uses_two_decimals() { let mut s = stats(); s["cost"] = json!(0.1234); assert!(plain(&s).contains("$0.12")); }
#[test]
fn session_context_includes_grouped_tokens() { let mut s = stats(); s["contextUsage"] = json!({"tokens":21897,"contextWindow":200000,"percent":4}); assert!(plain(&s).contains("21,897 / 200,000 (4%)")); }
#[test]
fn session_context_is_absent_without_usage() { assert!(!plain(&stats()).contains("Context Window")); }
#[test]
fn session_zero_cost_is_absent() { assert!(!plain(&stats()).contains("$0.00")); }
#[test]
fn session_large_tokens_are_grouped() { let mut s = stats(); s["tokens"]["input"] = json!(1234567); assert!(plain(&s).contains("1,234,567")); }
#[test]
fn session_unknown_context_has_placeholder() { let mut s = stats(); s["contextUsage"] = json!({"tokens":null,"contextWindow":200000,"percent":null}); assert!(plain(&s).contains("— / 200,000 (—)")); }
#[test]
fn version_matches_generated_reference() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("golden/version-label.json")).expect("valid test fixture and successful operation");
    for c in cases { assert_eq!(maho_interactive::version_label::format_display_version(c["args"][0].as_str().expect("valid test fixture and successful operation")), c["result"].as_str().expect("valid test fixture and successful operation")); }
}
#[test]
fn retry_countdown_stops_after_dispose() {
    use maho_interactive::components::status_indicator::StatusIndicator;
    use maho_tui::tui::Component;
    let mut s = StatusIndicator::retry(1, 3, 3000, false, "Esc", theme(), 0);
    s.tick(1000);
    assert!(maho_tui::utils::strip_terminal_sequences(&s.render(120).join("\n")).contains("in 2s"));
    s.dispose();
    assert!(!s.tick(4000));
}
#[test]
fn narrow_compaction_keeps_cancel_hint() {
    use maho_interactive::components::status_indicator::{StatusIndicator, CompactionStatusReason};
    use maho_tui::tui::Component;
    let mut s = StatusIndicator::compaction(CompactionStatusReason::Overflow, "Esc", theme(), 0);
    let lines = s.render(40);
    assert_eq!(lines.len(), 1);
    assert!(maho_tui::utils::strip_terminal_sequences(&lines[0]).contains("to cancel"));
}
