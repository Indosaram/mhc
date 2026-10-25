use std::sync::LazyLock;

use regex::Regex;

use crate::model_string_parser::parse_variant_from_model_id;
use crate::reasoning_level::{SplitReasoningSuffixOptions, is_reasoning_level_or_auto};

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
pub fn is_gpt6_astra_model(model: Option<&str>) -> bool {
    let Some(model) = model.filter(|model| !model.is_empty()) else {
        return false;
    };
    let parsed = parse_variant_from_model_id(
        &model.to_lowercase(),
        SplitReasoningSuffixOptions {
            allow_max_suffix: Some(true),
        },
    );
    if parsed.variant.as_deref().is_some_and(|variant| !is_reasoning_level_or_auto(variant)) {
        return false;
    }
    matches!(extract_model_name(&parsed.model_id), "gpt-6-astra" | "gpt-6-astra-fast")
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

/// Kimi Code addresses its models by rolling product ids that carry no version
/// signal. Moonshot upgraded `kimi-for-coding` to K2.8 Preview in place on
/// 2026-09-11 and left `kimi-for-coding-highspeed` on K2.7 Code HighSpeed.
/// <https://www.kimi.com/code/docs/en/kimi-code/models.html> (checked 2026-09-18)
pub const KIMI_CODE_K27_MODEL_ID: &str = "kimi-for-coding-highspeed";
pub const KIMI_CODE_K28_MODEL_ID: &str = "kimi-for-coding";

#[must_use]
pub fn is_kimi_k27_model(model: &str) -> bool {
    static KIMI_K27: LazyLock<Regex> = LazyLock::new(|| regex(r"kimi-k2[.\-]?7"));
    static K2P7: LazyLock<Regex> = LazyLock::new(|| regex(r"k2[-.]?p7"));
    let model_name = extract_model_name(model).to_lowercase();
    if model_name == KIMI_CODE_K27_MODEL_ID {
        return true;
    }
    KIMI_K27.is_match(&model_name) || K2P7.is_match(&model_name)
}

#[must_use]
pub fn is_kimi_k28_model(model: &str) -> bool {
    static KIMI_K28: LazyLock<Regex> = LazyLock::new(|| regex(r"kimi-k2[.\-]?8"));
    static K2P8: LazyLock<Regex> = LazyLock::new(|| regex(r"k2[-.]?p8"));
    let model_name = extract_model_name(model).to_lowercase();
    if model_name == KIMI_CODE_K28_MODEL_ID {
        return true;
    }
    KIMI_K28.is_match(&model_name) || K2P8.is_match(&model_name)
}

/// K2.7 Code and K2.8 Preview share one prompt. K2.8 is an efficiency and
/// context upgrade inside the same K2 coding family, not a new prompting
/// contract, so every prompt-routing site treats the two as one family.
#[must_use]
pub fn is_kimi_k2_code_model(model: &str) -> bool {
    is_kimi_k27_model(model) || is_kimi_k28_model(model)
}

/// Kimi K3.1: `k3.1-pro` / `k3.1-pro-highspeed` on Kimi Code, `kimi-k3.1-pro` /
/// `kimi-k3.1-pro-highspeed` on the Kimi API and gateways, `kimi-k3-1-pro` on Venice, and the
/// dotless `k3p1`.
/// <https://www.kimi.com/code/docs/en/kimi-code/models.html> and
/// <https://platform.kimi.com/docs/guide/kimi-k3-1-pro-quickstart> (checked 2026-10-24).
/// K3.1 stays inside the K3 family ([`is_kimi_k3_model`] is true for it); prompt routing checks
/// K3.1 first.
#[must_use]
pub fn is_kimi_k31_model(model: &str) -> bool {
    static KIMI_K31: LazyLock<Regex> =
        LazyLock::new(|| regex(r"(?:^|[/._-])(?:kimi-)?k3(?:[._-]|p)1(?:$|[/._:-])"));
    static WHITESPACE_RUN: LazyLock<Regex> = LazyLock::new(|| regex(r"\s+"));
    let lowered = extract_model_name(model).to_lowercase();
    let model_name = WHITESPACE_RUN.replace_all(&lowered, "-");
    KIMI_K31.is_match(&model_name)
}

#[must_use]
pub fn is_kimi_k3_model(model: &str) -> bool {
    static K3_SUFFIX: LazyLock<Regex> = LazyLock::new(|| regex(r"k3[-.]?p?[0-9]*$"));
    if is_kimi_k31_model(model) {
        return true;
    }
    let model_name = extract_model_name(model).to_lowercase();
    model_name.contains("kimi-k3") || K3_SUFFIX.is_match(&model_name)
}

#[must_use]
pub fn is_swe2_model(model: &str) -> bool {
    static SWE2: LazyLock<Regex> = LazyLock::new(|| regex(r"^swe-2(?:[-.]|$)"));
    SWE2.is_match(&extract_model_name(model).to_lowercase())
}

/// The SWE-2 lanes Devin's Cascade serves. It answers every other SWE-2 uid - the bare `swe-2`, and
/// the `swe-2-low` / `swe-2-high-lite` strings that appear only inside the Devin CLI binary - with
/// `permission_denied`, so a config naming one fails every request.
pub const DEVIN_SWE2_SERVED_LANES: [&str; 3] = ["swe-2-medium", "swe-2-high", "swe-2-max"];

/// An explicit `devin/` selector naming a SWE-2 id outside [`DEVIN_SWE2_SERVED_LANES`].
#[must_use]
pub fn is_unserved_devin_swe2_selector(selector: &str) -> bool {
    let Some(separator) = selector.find('/') else {
        return false;
    };
    if separator == 0 || !selector[..separator].eq_ignore_ascii_case("devin") {
        return false;
    }
    // A reasoning suffix (`:high`, ` (high)`) rides on the selector, never on the uid Cascade sees.
    let selector_model = selector[separator + 1..].trim().to_lowercase();
    let model_id = selector_model
        .split(|c: char| c == ':' || c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("");
    if !is_swe2_model(model_id) {
        return false;
    }
    !DEVIN_SWE2_SERVED_LANES.contains(&model_id)
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

#[cfg(test)]
mod tests {
    use super::is_gpt6_astra_model;

    #[test]
    fn recognizes_supported_astra_selectors() {
        // Given the upstream supported bare, prefixed and reasoning-suffixed selectors.
        for model in [
            "gpt-6-astra", "gpt-6-astra-fast", "openai/gpt-6-astra",
            "chatgpt-subscription/gpt-6-astra-fast", "github-copilot/gpt-6-astra",
            "opencode/gpt-6-astra", "vercel/openai/gpt-6-astra-fast",
            "openrouter/openai/gpt-6-astra", " GPT-6-Astra ", "gpt-6-astra:high",
            "gpt-6-astra-fast:max", "chatgpt-subscription/gpt-6-astra (high)",
            "openai/gpt-6-astra-fast xhigh", "vercel/openai/gpt-6-astra: auto",
        ] {
            // When the selector is detected, then only the Astra prompt gate opens.
            assert!(is_gpt6_astra_model(Some(model)), "{model}");
        }
    }

    #[test]
    fn rejects_other_models_and_unknown_astra_variants() {
        // Given selectors outside the upstream Astra gate.
        for model in [
            None, Some(""), Some("unknown"), Some("openai/gpt-6-sol"),
            Some("openai/gpt-6-sol-fast:high"), Some("chatgpt-subscription/gpt-6.1-sol"),
            Some("gpt-6.1-sol-fast (high)"), Some("gpt-6-luna"), Some("gpt-6-luna-fast"),
            Some("gpt-5.6-sol"), Some("anthropic/claude-fable-5"),
            Some("gpt-6-astra/claude-opus-5-5"), Some("custom-gpt-6-astra"),
            Some("gpt-6-astra-preview"), Some("gpt-6-astra-fastest"),
            Some("gpt-6-astra:unrecognized"), Some("gpt-6-astra (unrecognized)"),
            Some("gpt-6-astra unrelated"),
        ] {
            // When the selector is detected, then the gate stays closed.
            assert!(!is_gpt6_astra_model(model), "{model:?}");
        }
    }
}
