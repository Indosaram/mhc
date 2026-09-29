use std::sync::LazyLock;

use regex::Regex;

fn infer_sub_provider(model: &str) -> Option<&'static str> {
    const PREFIXES: [(&str, &str); 8] = [
        ("claude-", "anthropic"),
        ("gpt-", "openai"),
        ("gemini-", "google"),
        ("grok-", "xai"),
        ("minimax-", "minimax"),
        ("kimi-", "moonshotai"),
        ("k3", "moonshotai"),
        ("glm-", "zai"),
    ];
    PREFIXES
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, provider)| *provider)
}

#[expect(clippy::expect_used, reason = "static regex literal is valid")]
fn claude_version_dot(model: &str) -> String {
    static CLAUDE_VERSION_DOT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"claude-([A-Za-z0-9_]+)-([0-9]+)-([0-9]+)").expect("valid regex")
    });
    CLAUDE_VERSION_DOT
        .replace_all(model, "claude-$1-$2.$3")
        .into_owned()
}

/// Global replace of `needle` when not followed by `-` and, when given, not preceded by
/// `forbidden_prefix` (the JS lookaround forms `needle(?!-)` / `(?<!prefix)needle(?!-)`).
fn replace_unsuffixed(
    model: &str,
    needle: &str,
    forbidden_prefix: Option<&str>,
    replacement: &str,
) -> String {
    let mut result = String::with_capacity(model.len());
    let mut cursor = 0;
    let mut search_from = 0;
    while let Some(offset) = model[search_from..].find(needle) {
        let start = search_from + offset;
        let end = start + needle.len();
        let followed_by_dash = model[end..].starts_with('-');
        let preceded_by_forbidden =
            forbidden_prefix.is_some_and(|prefix| model[..start].ends_with(prefix));
        if followed_by_dash || preceded_by_forbidden {
            search_from = start + 1;
            continue;
        }
        result.push_str(&model[cursor..start]);
        result.push_str(replacement);
        cursor = end;
        search_from = end;
    }
    result.push_str(&model[cursor..]);
    result
}

fn gemini_31_pro_preview(model: &str) -> String {
    replace_unsuffixed(model, "gemini-3.1-pro", None, "gemini-3.1-pro-preview")
}

fn gemini_3_flash_preview(model: &str) -> String {
    replace_unsuffixed(
        model,
        "gemini-3-flash",
        Some("antigravity-"),
        "gemini-3-flash-preview",
    )
}

fn apply_gateway_transforms(model: &str) -> String {
    gemini_31_pro_preview(&claude_version_dot(model))
}

fn transform_model_for_provider_using_anthropic_behavior(provider: &str, model: &str) -> String {
    match provider {
        "vercel" => {
            if let Some((sub_provider, sub_model)) = model.split_once('/') {
                return format!("{sub_provider}/{}", apply_gateway_transforms(sub_model));
            }
            match infer_sub_provider(model) {
                Some(sub_provider) => format!("{sub_provider}/{}", apply_gateway_transforms(model)),
                None => model.to_string(),
            }
        }
        "github-copilot" => {
            gemini_3_flash_preview(&gemini_31_pro_preview(&claude_version_dot(model)))
        }
        "google" => gemini_3_flash_preview(&gemini_31_pro_preview(model)),
        "kimi-coding" | "kimi-for-coding" => match model {
            "kimi-k3" => "k3".to_string(),
            "kimi-k3-256k" => "k3-256k".to_string(),
            _ => model.to_string(),
        },
        _ => model.to_string(),
    }
}

/// Provider-specific model id rewrite used when building `provider/model` strings.
#[must_use]
pub fn transform_model_for_provider(provider: &str, model: &str) -> String {
    transform_model_for_provider_using_anthropic_behavior(provider, model)
}

/// Same transform, used for display strings.
#[must_use]
pub fn transform_model_for_provider_display(provider: &str, model: &str) -> String {
    transform_model_for_provider_using_anthropic_behavior(provider, model)
}
