use delegate_core::DetectedError;
use delegate_core::build_retry_guidance;
use delegate_core::detect_delegate_task_error;
use pretty_assertions::assert_eq;

#[test]
fn unknown_category_output_retry_guidance_preserves_available_options() {
    let output = r#"[ERROR] Unknown category: "bad". Available: visual-engineering, ultrabrain"#;
    let error = detect_delegate_task_error(output);

    assert_eq!(
        error,
        Some(DetectedError {
            error_type: "unknown_category".to_string(),
            original_output: output.to_string(),
        })
    );
    let guidance = error.as_ref().map(build_retry_guidance).unwrap_or_default();
    let available_options: Option<Vec<&str>> = output
        .split_once("Available: ")
        .map(|(_, list)| list.split(", ").collect());

    assert!(!guidance.is_empty());
    assert!(guidance.contains("run_in_background=true"));
    let available_options = available_options.unwrap_or_default();
    assert!(!available_options.is_empty());
    assert!(
        available_options
            .iter()
            .all(|option| guidance.contains(option))
    );
}

#[test]
fn background_retry_pattern_recommends_standard_spawn() {
    let pattern = delegate_core::DELEGATE_TASK_ERROR_PATTERNS
        .iter()
        .find(|pattern| pattern.error_type == "missing_run_in_background");

    let Some(pattern) = pattern else {
        panic!("background retry pattern is missing");
    };
    assert!(pattern.fix_hint.contains("run_in_background=true (the standard spawn)"));
    assert!(pattern.fix_hint.contains("run_in_background=false for a short child"));
}
