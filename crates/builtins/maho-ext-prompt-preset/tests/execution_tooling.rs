use maho_ext_prompt_preset::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_section, build_execution_tooling_paragraph, EXECUTION_TOOLING_RULES};

#[test]
fn absent_eval_suppresses_both_dialects() {
    for dialect in [ExecutionToolingDialect::Claude, ExecutionToolingDialect::Kimi] {
        assert!(build_execution_tooling_section(&["read".into()], dialect).is_empty());
        assert!(build_execution_tooling_paragraph(&[], dialect).is_empty());
    }
}
#[test]
fn claude_eval_routing_uses_tagged_envelope() {
    let result = build_execution_tooling_section(&["eval".into()], ExecutionToolingDialect::Claude);
    assert!(result.starts_with("<execution_tooling>\n"));
    assert!(result.ends_with("\n</execution_tooling>"));
}
#[test]
fn kimi_eval_routing_is_untagged_and_paragraph_adds_gap() {
    let tools = ["eval".into()];
    let result = build_execution_tooling_section(&tools, ExecutionToolingDialect::Kimi);
    assert!(!result.is_empty());
    assert!(!result.starts_with("<execution_tooling>"));
    assert_eq!(build_execution_tooling_paragraph(&tools, ExecutionToolingDialect::Kimi), format!("{result}\n\n"));
}
#[test]
fn machine_rule_ids_remain_unique_and_eval_scoped() {
    let ids: std::collections::BTreeSet<_> = EXECUTION_TOOLING_RULES.iter().map(|rule| rule.id).collect();
    assert_eq!(ids.len(), 4);
    assert!(EXECUTION_TOOLING_RULES.iter().all(|rule| rule.concern == "code-cell-routing"));
}
