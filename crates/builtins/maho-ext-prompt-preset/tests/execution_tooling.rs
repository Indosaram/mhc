use maho_core::dynamic_prompt::SharedSections;
use maho_ext_prompt_preset::execution_tooling::{
    EXECUTION_TOOLING_RULES, ExecutionToolingDialect, build_execution_tooling_paragraph,
    build_execution_tooling_section,
};

#[test]
fn absent_eval_suppresses_both_dialects() {
    for dialect in [
        ExecutionToolingDialect::Claude,
        ExecutionToolingDialect::Kimi,
    ] {
        assert!(
            build_execution_tooling_section(&["read".into()], dialect, SharedSections::Diet)
                .is_empty()
        );
        assert!(build_execution_tooling_paragraph(&[], dialect, SharedSections::Full).is_empty());
    }
}
#[test]
fn claude_eval_routing_uses_tagged_envelope() {
    let result = build_execution_tooling_section(
        &["eval".into()],
        ExecutionToolingDialect::Claude,
        SharedSections::Diet,
    );
    assert!(result.starts_with("<execution_tooling>\n"));
    assert!(result.ends_with("\n</execution_tooling>"));
}
#[test]
fn kimi_eval_routing_is_untagged_and_paragraph_adds_gap() {
    let tools = ["eval".into()];
    let result = build_execution_tooling_section(
        &tools,
        ExecutionToolingDialect::Kimi,
        SharedSections::Diet,
    );
    assert!(!result.is_empty());
    assert!(!result.starts_with("<execution_tooling>"));
    assert_eq!(
        build_execution_tooling_paragraph(
            &tools,
            ExecutionToolingDialect::Kimi,
            SharedSections::Diet
        ),
        format!("{result}\n\n")
    );
}
#[test]
fn machine_rule_ids_remain_unique_and_eval_scoped() {
    let ids: std::collections::BTreeSet<_> =
        EXECUTION_TOOLING_RULES.iter().map(|rule| rule.id).collect();
    assert_eq!(ids.len(), 4);
    assert!(
        EXECUTION_TOOLING_RULES
            .iter()
            .all(|rule| rule.concern == "code-cell-routing")
    );
}
#[test]
fn full_shared_sections_render_the_pre_diet_directives() {
    let tools = ["eval".into()];
    let diet = build_execution_tooling_section(
        &tools,
        ExecutionToolingDialect::Claude,
        SharedSections::Diet,
    );
    let full = build_execution_tooling_section(
        &tools,
        ExecutionToolingDialect::Claude,
        SharedSections::Full,
    );
    for rule in EXECUTION_TOOLING_RULES {
        assert_eq!(
            diet.matches(rule.directive.claude).count(),
            1,
            "{}",
            rule.id
        );
        assert_eq!(
            full.matches(rule.full_directive.claude).count(),
            1,
            "{}",
            rule.id
        );
        assert!(!full.contains(rule.directive.claude), "{}", rule.id);
    }
}
