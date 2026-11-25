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

#[test]
fn plain_gemini_pro_gets_preview_but_antigravity_and_preview_ids_are_preserved() {
    for (provider, model, expected) in [
        (
            "google",
            "antigravity-gemini-3.1-pro",
            "antigravity-gemini-3.1-pro",
        ),
        (
            "google",
            "google/antigravity-gemini-3.1-pro",
            "google/antigravity-gemini-3.1-pro",
        ),
        (
            "github-copilot",
            "antigravity-gemini-3.1-pro",
            "antigravity-gemini-3.1-pro",
        ),
        (
            "vercel",
            "google/antigravity-gemini-3.1-pro",
            "google/antigravity-gemini-3.1-pro",
        ),
        ("google", "gemini-3.1-pro", "gemini-3.1-pro-preview"),
        ("github-copilot", "gemini-3.1-pro", "gemini-3.1-pro-preview"),
        (
            "vercel",
            "google/gemini-3.1-pro",
            "google/gemini-3.1-pro-preview",
        ),
        ("google", "gemini-3.1-pro-preview", "gemini-3.1-pro-preview"),
    ] {
        assert_eq!(
            transform_model_for_provider(provider, model),
            expected,
            "{provider}/{model}"
        );
        assert_eq!(
            transform_model_for_provider_display(provider, model),
            expected,
            "{provider}/{model}"
        );
    }
}
