use maho_ext_openai_image_gen::gate::*;
use serde_json::json;

#[test]
fn endpoint_and_api_gate_preserve_explicit_override() {
    let mut model = NativeImageGenModel { id: "model", provider: "openai", api: "openai-responses", base_url: "https://api.openai.com/v1", compat: None };
    assert!(supports_native_image_generation(Some(&model)));
    model.base_url = "https://gateway.example/v1";
    assert!(!supports_native_image_generation(Some(&model)));
    let compat = json!({"supportsImageGeneration":true});
    model.compat = Some(&compat);
    assert!(supports_native_image_generation(Some(&model)));
    model.api = "azure-openai-responses";
    assert!(!supports_native_image_generation(Some(&model)));
}

#[test]
fn codex_responses_requires_https_chatgpt_endpoint_unless_explicitly_disabled() {
    let mut model = NativeImageGenModel {
        id: "codex-model",
        provider: "openai-codex",
        api: "openai-codex-responses",
        base_url: "https://chatgpt.com/backend-api/codex",
        compat: None,
    };
    assert!(supports_native_image_generation(Some(&model)));
    model.base_url = "http://chatgpt.com/backend-api/codex";
    assert!(!supports_native_image_generation(Some(&model)));
    model.base_url = "https://proxy.example/backend-api/codex";
    assert!(!supports_native_image_generation(Some(&model)));

    let disabled = json!({"supportsImageGeneration": false});
    model.base_url = "https://chatgpt.com/backend-api/codex";
    model.compat = Some(&disabled);
    assert!(!supports_native_image_generation(Some(&model)));

    let enabled = json!({"supportsImageGeneration": true});
    model.base_url = "https://proxy.example/backend-api/codex";
    model.compat = Some(&enabled);
    assert!(!supports_native_image_generation(Some(&model)));
}

#[test]
fn environment_defaults_on_and_only_recognized_false_disables() {
    assert!(is_enabled(None));
    assert!(is_enabled(Some("unknown")));
    for value in ["0", " false ", "NO", "off"] { assert!(!is_enabled(Some(value))); }
    assert!(!supports_native_image_generation(None));
    assert_eq!(native_image_gen_model_key(None), "");
}
