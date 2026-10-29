#[test]
fn normalizes_feedback() { assert_eq!(maho_omo_comment_checker::utils::normalize_feedback_text(" \r\nalpha\rbeta\r\n "),"alpha\nbeta"); }
#[test]
fn string_extraction_rejects_nonstrings() { assert!(maho_omo_comment_checker::utils::get_string(&serde_json::json!(42)).is_none()); }

#[test]
fn recognizes_bun_missing_module_values() {
    use maho_omo_comment_checker::resolver::is_missing_module_error;
    assert!(is_missing_module_error(&serde_json::json!({"code":"MODULE_NOT_FOUND"})));
    assert!(is_missing_module_error(&serde_json::json!({"name":"ResolveMessage"})));
    assert!(!is_missing_module_error(&serde_json::json!({"code":"EACCES"})));
}
