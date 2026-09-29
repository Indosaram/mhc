use model_core::ModelFormat;
use model_core::ModelFormatInput;
use model_core::normalize_model_format;
use pretty_assertions::assert_eq;

fn from_string(model: &str) -> Option<ModelFormat> {
    normalize_model_format(Some(&ModelFormatInput::String(model.to_string())))
}

fn format(provider_id: &str, model_id: &str) -> ModelFormat {
    ModelFormat {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
    }
}

#[test]
fn splits_provider_model_format_correctly() {
    assert_eq!(
        from_string("opencode/glm-5-free"),
        Some(format("opencode", "glm-5-free"))
    );
}

#[test]
fn handles_provider_with_multiple_slashes() {
    assert_eq!(
        from_string("anthropic/claude-opus-4-7/max"),
        Some(format("anthropic", "claude-opus-4-7/max"))
    );
}

#[test]
fn returns_none_for_malformed_string_without_separator() {
    assert_eq!(from_string("invalid"), None);
}

#[test]
fn returns_none_for_empty_string() {
    assert_eq!(from_string(""), None);
}

#[test]
fn passes_object_format_through_unchanged() {
    let input = format("opencode", "glm-5-free");
    assert_eq!(
        normalize_model_format(Some(&ModelFormatInput::Pair(input.clone()))),
        Some(input)
    );
}

#[test]
fn returns_none_for_null() {
    assert_eq!(normalize_model_format(None), None);
}

#[test]
fn returns_none_for_undefined() {
    assert_eq!(normalize_model_format(None), None);
}
