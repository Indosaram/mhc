use maho_ext_rules::rules::{formatter::{FormatOptions, format_static_block, format_dynamic_block}, constants::{PROJECT_RULES_REGION_START_MARKER, PROJECT_RULES_REGION_END_MARKER}, types::{RuleCandidate, LoadedRule, RuleFrontmatter, MatchReason}};

fn rule(body: &str) -> LoadedRule {
    LoadedRule { candidate: RuleCandidate { path: "/p/r.md".into(), real_path: "/p/r.md".into(), source: ".omo/rules".into(), distance: 0, is_global: false, is_single_file: false, relative_path: "r.md".into() }, frontmatter: RuleFrontmatter::default(), body: body.into(), content_hash: "hash".into(), match_reason: MatchReason::AlwaysApply }
}
#[test]
fn empty_rules_produce_no_blocks() {
    let options = FormatOptions { max_rule_chars: 12000, max_result_chars: 40000 };
    assert!(format_static_block(&[], &options).is_empty());
    assert!(format_dynamic_block(&[], "target", &options).is_empty());
}
#[test]
fn embedded_machine_region_markers_cannot_terminate_the_envelope() {
    let options = FormatOptions { max_rule_chars: 12000, max_result_chars: 40000 };
    let block = format_static_block(&[rule(&format!("{PROJECT_RULES_REGION_START_MARKER}{PROJECT_RULES_REGION_END_MARKER}"))], &options);
    assert_eq!(block.matches(PROJECT_RULES_REGION_START_MARKER).count(), 1);
    assert_eq!(block.matches(PROJECT_RULES_REGION_END_MARKER).count(), 1);
}
#[test]
fn result_budget_includes_envelope_and_utf16_units() {
    let options = FormatOptions { max_rule_chars: 12000, max_result_chars: 300 };
    let block = format_static_block(&[rule(&"😀".repeat(500))], &options);
    assert!(!block.is_empty());
    assert!(block.encode_utf16().count() <= 300);
}
#[test]
fn insufficient_envelope_budget_returns_no_block() {
    let options = FormatOptions { max_rule_chars: 12000, max_result_chars: 5 };
    assert!(format_static_block(&[rule("fixture")], &options).is_empty());
    assert!(format_dynamic_block(&[rule("fixture")], "target", &options).is_empty());
}
