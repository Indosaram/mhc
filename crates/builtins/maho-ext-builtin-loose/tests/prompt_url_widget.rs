use maho_ext_builtin_loose::prompt_url_widget::{extract_prompt_match, format_author, desired_session_name, PromptKind};
use serde_json::json;

#[test]
fn pr_match_takes_precedence_over_issue_match() {
    let prompt = "Analyze GitHub issue(s): https://github.com/a/b/issues/1\n  YOU ARE GIVEN ONE OR MORE GITHUB PR URLS: https://github.com/a/b/pull/2 extra";
    let found = extract_prompt_match(prompt).expect("match");
    assert_eq!(found.kind, PromptKind::Pr);
    assert_eq!(found.url, "https://github.com/a/b/pull/2");
}

#[test]
fn author_formats_available_metadata() {
    let author = json!({"name":" Jane ","login":" jane "});
    assert_eq!(format_author(Some(&author)), Some("Jane (@jane)".into()));
    assert_eq!(format_author(Some(&json!({"login":"jane"}))), Some("@jane".into()));
    assert_eq!(format_author(None), None);
}

#[test]
fn session_name_preserves_custom_name_and_upgrades_fallback() {
    let found = extract_prompt_match("Analyze GitHub issue(s): https://example.com/1").expect("match");
    assert_eq!(desired_session_name(&found, Some(" Title "), Some("Issue: https://example.com/1")), Some("Issue: Title (https://example.com/1)".into()));
    assert_eq!(desired_session_name(&found, Some("Title"), Some("my session")), None);
}
