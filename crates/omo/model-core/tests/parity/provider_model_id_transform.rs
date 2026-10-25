use model_core::transform_model_for_provider;
use model_core::transform_model_for_provider_display;
use pretty_assertions::assert_eq;

#[test]
fn transforms_kimi_models_for_kimi_coding_providers() {
    let provider = "kimi-coding";

    assert_eq!(transform_model_for_provider(provider, "kimi-k3"), "k3");
    assert_eq!(
        transform_model_for_provider(provider, "kimi-k3-256k"),
        "k3-256k"
    );
    assert_eq!(
        transform_model_for_provider(provider, "kimi-k3.1-pro"),
        "k3.1-pro"
    );
    assert_eq!(
        transform_model_for_provider(provider, "kimi-k3.1-pro-highspeed"),
        "k3.1-pro-highspeed"
    );
}

#[test]
fn transforms_kimi_k31_models_for_kimi_for_coding_provider() {
    let provider = "kimi-for-coding";

    assert_eq!(
        transform_model_for_provider(provider, "kimi-k3.1-pro"),
        "k3.1-pro"
    );
    assert_eq!(
        transform_model_for_provider(provider, "kimi-k3.1-pro-highspeed"),
        "k3.1-pro-highspeed"
    );
    assert_eq!(
        transform_model_for_provider_display(provider, "kimi-k3.1-pro"),
        "k3.1-pro"
    );
}

#[test]
fn passes_through_unrelated_models_unchanged() {
    assert_eq!(
        transform_model_for_provider("kimi-for-coding", "gpt-5.6-luna-fast"),
        "gpt-5.6-luna-fast"
    );
}
