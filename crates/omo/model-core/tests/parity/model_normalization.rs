use model_core::normalize_model;
use model_core::normalize_model_id;
use pretty_assertions::assert_eq;

#[test]
fn normalize_model_returns_none_for_undefined() {
    assert_eq!(normalize_model(None), None);
}

#[test]
fn normalize_model_returns_none_for_empty_string() {
    assert_eq!(normalize_model(Some("")), None);
}

#[test]
fn normalize_model_returns_none_for_whitespace_only_string() {
    assert_eq!(normalize_model(Some("   ")), None);
}

#[test]
fn normalize_model_returns_same_string_for_valid_model() {
    assert_eq!(
        normalize_model(Some("claude-3-opus")).as_deref(),
        Some("claude-3-opus")
    );
}

#[test]
fn normalize_model_trims_leading_and_trailing_spaces() {
    assert_eq!(
        normalize_model(Some("  claude-3-opus  ")).as_deref(),
        Some("claude-3-opus")
    );
}

#[test]
fn normalize_model_returns_none_for_only_spaces() {
    assert_eq!(normalize_model(Some("     ")), None);
}

#[test]
fn normalize_model_id_replaces_dots_in_version_numbers() {
    assert_eq!(normalize_model_id("claude-3.5-sonnet"), "claude-3-5-sonnet");
}

#[test]
fn normalize_model_id_leaves_models_without_dots_unchanged() {
    assert_eq!(normalize_model_id("claude-opus"), "claude-opus");
}

#[test]
fn normalize_model_id_replaces_multiple_dot_numbers() {
    assert_eq!(normalize_model_id("model.1.2"), "model-1-2");
}
