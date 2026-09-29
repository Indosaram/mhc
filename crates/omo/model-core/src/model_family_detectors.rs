use std::sync::LazyLock;

use regex::Regex;

fn extract_model_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn dashed_model_name(model: &str) -> String {
    extract_model_name(model).to_lowercase().replace('.', "-")
}

#[expect(clippy::expect_used, reason = "static regex literals are valid")]
fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

#[must_use]
pub fn is_gpt_model(model: &str) -> bool {
    extract_model_name(model).to_lowercase().contains("gpt")
}

#[must_use]
pub fn is_claude_opus46_model(model: &str) -> bool {
    dashed_model_name(model).contains("claude-opus-4-6")
}

#[must_use]
pub fn is_claude_opus47_model(model: &str) -> bool {
    dashed_model_name(model).contains("claude-opus-4-7")
}

#[must_use]
pub fn is_claude_opus48_model(model: &str) -> bool {
    dashed_model_name(model).contains("claude-opus-4-8")
}

#[must_use]
pub fn is_claude_opus5_model(model: &str) -> bool {
    dashed_model_name(model).contains("claude-opus-5")
}

#[must_use]
pub fn is_claude_fable5_model(model: &str) -> bool {
    dashed_model_name(model).contains("claude-fable-5")
}

/// Claude Fable shares the Opus 4.7+ request surface (adaptive thinking only, explicit
/// enabled-thinking budgets rejected), so it counts as "4.7 or later".
#[must_use]
pub fn is_claude_opus47_or_later_model(model: &str) -> bool {
    static CLAUDE_OPUS_VERSION: LazyLock<Regex> =
        LazyLock::new(|| regex(r"claude-opus-([0-9]+)(?:-([0-9]+))?"));
    let model_name = dashed_model_name(model);
    if model_name.contains("claude-fable") {
        return true;
    }
    let Some(captures) = CLAUDE_OPUS_VERSION.captures(&model_name) else {
        return false;
    };
    let Some(major) = captures.get(1).and_then(|m| m.as_str().parse::<f64>().ok()) else {
        return false;
    };
    let minor = match captures.get(2) {
        None => 0.0,
        Some(m) => match m.as_str().parse::<f64>() {
            Ok(minor) => minor,
            Err(_) => return false,
        },
    };
    major > 4.0 || (major == 4.0 && minor >= 7.0)
}

/// Claude Fable / Mythos family (claude-fable-5, claude-mythos-5, claude-mythos-preview):
/// adaptive-only models that reject `thinking.type: "enabled"`.
#[must_use]
pub fn is_claude_fable_or_mythos_model(model: &str) -> bool {
    static FABLE_OR_MYTHOS: LazyLock<Regex> =
        LazyLock::new(|| regex(r"claude-(?:fable|mythos)-(?:[0-9]+|preview)"));
    FABLE_OR_MYTHOS.is_match(&dashed_model_name(model))
}

#[must_use]
pub fn is_kimi_k2_model(model: &str) -> bool {
    static K2P: LazyLock<Regex> = LazyLock::new(|| regex(r"k2[-.]?p[567]"));
    let model_name = extract_model_name(model).to_lowercase();
    model_name.contains("kimi") || K2P.is_match(&model_name)
}

#[must_use]
pub fn is_kimi_k27_model(model: &str) -> bool {
    static KIMI_K27: LazyLock<Regex> = LazyLock::new(|| regex(r"kimi-k2[.\-]?7"));
    static K2P7: LazyLock<Regex> = LazyLock::new(|| regex(r"k2[-.]?p7"));
    let model_name = extract_model_name(model).to_lowercase();
    KIMI_K27.is_match(&model_name) || K2P7.is_match(&model_name)
}

#[must_use]
pub fn is_kimi_k3_model(model: &str) -> bool {
    static K3_SUFFIX: LazyLock<Regex> = LazyLock::new(|| regex(r"k3[-.]?p?[0-9]*$"));
    let model_name = extract_model_name(model).to_lowercase();
    model_name.contains("kimi-k3") || K3_SUFFIX.is_match(&model_name)
}

#[must_use]
pub fn is_mini_max_model(model: &str) -> bool {
    extract_model_name(model).to_lowercase().contains("minimax")
}

#[must_use]
pub fn is_glm_model(model: &str) -> bool {
    extract_model_name(model).to_lowercase().contains("glm")
}

/// `needle(?![0-9])`: the xAI catalog ships grok-4.20, so a bare substring would swallow it.
fn contains_without_trailing_digit(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, _)| {
        !haystack[start + needle.len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_digit())
    })
}

#[must_use]
pub fn is_grok45_model(model: &str) -> bool {
    contains_without_trailing_digit(&dashed_model_name(model), "grok-4-5")
}

#[must_use]
pub fn is_grok46_model(model: &str) -> bool {
    contains_without_trailing_digit(&dashed_model_name(model), "grok-4-6")
}

#[must_use]
pub fn is_gemini_model(model: &str) -> bool {
    const GEMINI_PROVIDERS: [&str; 2] = ["google/", "google-vertex/"];
    if GEMINI_PROVIDERS
        .iter()
        .any(|prefix| model.starts_with(prefix))
    {
        return true;
    }
    let model_name = extract_model_name(model).to_lowercase();
    if model.starts_with("github-copilot/") && model_name.starts_with("gemini") {
        return true;
    }
    model_name.starts_with("gemini-")
}
