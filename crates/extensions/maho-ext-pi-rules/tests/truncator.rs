use maho_ext_pi_rules::rules::truncator::{truncate_budget, truncate_rule, BudgetRule};

const PATH: &str = ".omo/rules/typescript.md";
fn notice(path: &str) -> String { format!("\n\n[Rule truncated. Read full rule: {path}]") }
fn rule(body: &str, path: &str) -> BudgetRule { BudgetRule { body: body.into(), relative_path: path.into() } }

#[test]
fn shorter_body_is_unchanged() {
    let body = "Use strict TypeScript.";
    let result = truncate_rule(body, 100, PATH);
    assert_eq!(result.body, body); assert!(!result.truncated); assert_eq!(result.original_length, body.len());
}
#[test]
fn equal_body_is_unchanged() {
    let body = "1234567890";
    let result = truncate_rule(body, body.len(), PATH);
    assert_eq!(result.body, body); assert!(!result.truncated);
}
#[test]
fn longer_body_fits_budget() {
    let body = "a".repeat(200);
    let result = truncate_rule(&body, 100, PATH);
    assert!(result.truncated); assert!(result.body.len() <= 100); assert!(result.body.ends_with(&notice(PATH)));
}
#[test]
fn small_prefix_is_preserved() {
    let suffix = notice(PATH); let body = "b".repeat(suffix.len() + 6);
    let result = truncate_rule(&body, suffix.len() + 5, PATH);
    assert_eq!(result.body, format!("bbbbb{suffix}"));
}
#[test]
fn custom_path_is_substituted() {
    let body = "c".repeat(200); let path = ".claude/rules/python.md";
    let result = truncate_rule(&body, 100, path);
    assert!(result.body.ends_with(&notice(path))); assert!(!result.body.contains("{path}"));
}
#[test]
fn original_length_survives_truncation() {
    let body = "d".repeat(500);
    let result = truncate_rule(&body, 120, PATH);
    assert_eq!(result.original_length, 500);
}
#[test]
fn notice_can_exceed_rule_budget() {
    let body = "e".repeat(200); let suffix = notice(PATH);
    let result = truncate_rule(&body, suffix.len() - 1, PATH);
    assert_eq!(result.body, suffix); assert!(result.truncated); assert_eq!(result.original_length, 200);
}
#[test]
fn empty_body_stays_empty() {
    let result = truncate_rule("", 10, PATH);
    assert_eq!(result.body, ""); assert!(!result.truncated); assert_eq!(result.original_length, 0);
}
#[test]
fn surrogate_pair_is_not_split() {
    let body = "😀".repeat(80); let suffix = notice(PATH);
    let result = truncate_rule(&body, suffix.len() + 9, PATH);
    assert_eq!(result.body, format!("{}{suffix}", "😀".repeat(4))); assert_eq!(result.original_length, 160);
}
#[test]
fn budget_keeps_all_fitting_rules() {
    let rules = [rule("first", "first.md"), rule("second", "second.md")];
    let result = truncate_budget(&rules, 20);
    assert_eq!(result.len(), 2); assert_eq!(result[0].body, "first"); assert_eq!(result[1].body, "second"); assert!(result.iter().all(|r| !r.truncated));
}
#[test]
fn budget_truncates_first_rule() {
    let rules = [rule(&"a".repeat(100), PATH)]; let suffix = notice(PATH);
    let result = truncate_budget(&rules, suffix.len() + 10);
    assert_eq!(result.len(), 1); assert_eq!(result[0].body, format!("{}{suffix}", "a".repeat(10))); assert!(result[0].truncated);
}
#[test]
fn exhausted_budget_excludes_later_rules() {
    let rules = [rule("exact", "first.md"), rule("second", "second.md")];
    let result = truncate_budget(&rules, 5);
    assert_eq!(result.len(), 1); assert_eq!(result[0].body, "exact"); assert!(!result[0].truncated);
}
#[test]
fn empty_rules_produce_empty_result() {
    let result = truncate_budget(&[], 100);
    assert!(result.is_empty());
}
#[test]
fn exact_budget_does_not_truncate() {
    let rules = [rule("12345", PATH)];
    let result = truncate_budget(&rules, 5);
    assert_eq!(result.len(), 1); assert_eq!(result[0].body, "12345"); assert!(!result[0].truncated);
}
