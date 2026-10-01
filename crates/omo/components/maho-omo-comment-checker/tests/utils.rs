#[test]
fn normalizes_feedback() { assert_eq!(maho_omo_comment_checker::utils::normalize_feedback_text(" \r\nalpha\rbeta\r\n "),"alpha\nbeta"); }
#[test]
fn string_extraction_rejects_nonstrings() { assert!(maho_omo_comment_checker::utils::get_string(&serde_json::json!(42)).is_none()); }
