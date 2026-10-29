use model_core::ModelSuggestionInfo;
use model_core::parse_model_suggestion;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn suggestion(provider_id: &str, model_id: &str, suggestion: &str) -> Option<ModelSuggestionInfo> {
    Some(ModelSuggestionInfo {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        suggestion: suggestion.to_string(),
    })
}

/// A JS `new Error(message)`: `name` is "Error" and `message` is readable.
fn js_error(message: &str) -> Value {
    json!({ "name": "Error", "message": message })
}

#[test]
fn extracts_suggestions_from_structured_anthropic_provider_model_not_found_error() {
    let error = json!({
        "name": "ProviderModelNotFoundError",
        "data": {
            "providerID": "anthropic",
            "modelID": "claude-sonet-4",
            "suggestions": ["claude-sonnet-4", "claude-sonnet-4-6"],
        },
    });

    assert_eq!(
        parse_model_suggestion(&error),
        suggestion("anthropic", "claude-sonet-4", "claude-sonnet-4")
    );
}

#[test]
fn extracts_suggestions_from_nested_openai_errors() {
    let error = json!({
        "data": {
            "name": "ProviderModelNotFoundError",
            "data": {
                "providerID": "openai",
                "modelID": "gpt-5",
                "suggestions": ["gpt-5.4"],
            },
        },
    });

    assert_eq!(
        parse_model_suggestion(&error),
        suggestion("openai", "gpt-5", "gpt-5.4")
    );
}

#[test]
fn extracts_suggestions_from_bedrock_style_model_not_found_messages() {
    let error = js_error(
        "Model not found: aws-bedrock-anthropic/claude-sonet-4. Did you mean: claude-sonnet-4, claude-sonnet-4-6?",
    );

    assert_eq!(
        parse_model_suggestion(&error),
        suggestion("aws-bedrock-anthropic", "claude-sonet-4", "claude-sonnet-4")
    );
}

#[test]
fn extracts_suggestions_from_plain_string_message_payloads() {
    let error = json!("Model not found: openai/gtp-5. Did you mean: gpt-5?");

    assert_eq!(
        parse_model_suggestion(&error),
        suggestion("openai", "gtp-5", "gpt-5")
    );
}

#[test]
fn returns_none_for_unrelated_errors() {
    assert_eq!(
        parse_model_suggestion(&js_error("Connection timeout")),
        None
    );
    assert_eq!(parse_model_suggestion(&Value::Null), None);
}

/// serde_json values cannot be cyclic; the TS case exercises the same path (an object with no
/// `message` whose serialization carries no suggestion) and must return null.
#[test]
fn returns_none_for_object_payloads_without_messages() {
    let error = json!({ "self": {} });

    assert_eq!(parse_model_suggestion(&error), None);
}
