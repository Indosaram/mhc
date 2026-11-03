//! Port of `prompt-preset/matchers.ts` (senpi 903b8336): the model-id signals each preset matches.
//!
//! Every regex is the upstream literal; `normalize_model_id` is upstream
//! `modelId.toLowerCase().replace(/\s+/g, "-")`.

use crate::presets::ModelMetadata;
use regex::Regex;
use std::sync::LazyLock;

/// `ModelWithPromptPresetMetadata`: `{ id, provider, name?, promptPreset? }`.
pub type ModelWithPromptPresetMetadata<'a> = ModelMetadata<'a>;

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("static regex"));

pub fn normalize_model_id(model_id: &str) -> String {
    WHITESPACE
        .replace_all(&model_id.to_lowercase(), "-")
        .into_owned()
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

macro_rules! signal {
    ($has:ident, $is:ident, $pattern:literal) => {
        pub fn $has(value: &str) -> bool {
            static PATTERN: LazyLock<Regex> = LazyLock::new(|| re($pattern));
            PATTERN.is_match(&normalize_model_id(value))
        }
        pub fn $is(model: ModelWithPromptPresetMetadata<'_>) -> bool {
            $has(model.id) || model.name.is_some_and($has)
        }
    };
}

// The GPT-6 family (Astra, 6.1 Sol, Sol, Luna) shares one prompting guide, so every tier renders the
// gpt-6-astra preset. The dotless form is accepted only with a single digit right after the 6.
signal!(
    has_gpt6_family_signal,
    is_gpt6_family_model,
    r"(?:^|[/@:._-])gpt[._-]?6(?:[._-]\d+|\d)?[._-](?:astra|sol|luna)(?:$|[/@:._-])"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gpt5Version {
    Gpt52,
    Gpt53Codex,
    Gpt54,
    Gpt55,
    Gpt56,
}
impl Gpt5Version {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gpt52 => "gpt-5.2",
            Self::Gpt53Codex => "gpt-5.3-codex",
            Self::Gpt54 => "gpt-5.4",
            Self::Gpt55 => "gpt-5.5",
            Self::Gpt56 => "gpt-5.6",
        }
    }
}

pub fn extract_gpt5_version(model_id: &str) -> Option<Gpt5Version> {
    let normalized = normalize_model_id(model_id);
    [
        ("gpt-5.6", Gpt5Version::Gpt56),
        ("gpt-5.5", Gpt5Version::Gpt55),
        ("gpt-5.4", Gpt5Version::Gpt54),
        ("gpt-5.3", Gpt5Version::Gpt53Codex),
        ("gpt-5.2", Gpt5Version::Gpt52),
    ]
    .into_iter()
    .find_map(|(marker, version)| normalized.contains(marker).then_some(version))
}

signal!(
    has_kimi_k26_signal,
    is_kimi_k26_model,
    r"(?:^|[/@._-])kimi-k2(?:[._-]|p)6(?:$|[/@._:-])"
);

// Kimi Code addresses its models by rolling product ids rather than version tags.
pub const KIMI_CODE_K27_MODEL_ID: &str = "kimi-for-coding-highspeed";
pub const KIMI_CODE_K28_MODEL_ID: &str = "kimi-for-coding";

pub fn has_kimi_k27_signal(value: &str) -> bool {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?:^|[/@._-])kimi-k2(?:[._-]|p)7(?:$|[/@._:-])"));
    let normalized = normalize_model_id(value);
    normalized == KIMI_CODE_K27_MODEL_ID || PATTERN.is_match(&normalized)
}
pub fn is_kimi_k27_model(model: ModelWithPromptPresetMetadata<'_>) -> bool {
    has_kimi_k27_signal(model.id) || model.name.is_some_and(has_kimi_k27_signal)
}

pub fn has_kimi_k28_signal(value: &str) -> bool {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?:^|[/@._-])kimi-k2(?:[._-]|p)8(?:$|[/@._:-])"));
    let normalized = normalize_model_id(value);
    normalized == KIMI_CODE_K28_MODEL_ID || PATTERN.is_match(&normalized)
}
pub fn is_kimi_k28_model(model: ModelWithPromptPresetMetadata<'_>) -> bool {
    has_kimi_k28_signal(model.id) || model.name.is_some_and(has_kimi_k28_signal)
}

pub fn has_kimi_k3_signal(value: &str) -> bool {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|[/@._-])kimi-k3(?:$|[/@._:-])"));
    let normalized = normalize_model_id(value);
    normalized == "k3" || PATTERN.is_match(&normalized)
}
pub fn is_kimi_k3_model(model: ModelWithPromptPresetMetadata<'_>) -> bool {
    has_kimi_k3_signal(model.id) || model.name.is_some_and(has_kimi_k3_signal)
}

// Exactly the SWE-2 lanes Devin's Cascade serves; every other swe-2 uid is refused upstream (#2306).
signal!(
    has_swe2_signal,
    is_swe2_model,
    r"(?:^|[/@:._-])swe-2-(?:medium|high|max)(?:$|[/@:._])"
);

signal!(
    has_deepseek_v4_flash_0731_signal,
    is_deepseek_v4_flash_0731_model,
    r"(?:^|[/@:._-])deepseek[._-]v4[._-]flash[._-]0731(?:$|[/@:._-])"
);
signal!(
    has_deepseek_v4_flash_signal,
    is_deepseek_v4_flash_model,
    r"(?:^|[/@:._-])deepseek[._-]v4[._-]flash(?:$|[/@:._-])"
);

pub fn has_deepseek_v41_flash_signal(value: &str) -> bool {
    static VERSIONED: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?:^|[/@:._-])deepseek[._-]v4(?:[._-]1|p1)[._-]flash(?:$|[/@:._-])"));
    static OFFICIAL: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?:^|[/@:._-])deepseek[._-]flash(?:$|[/@:._-])"));
    let normalized = normalize_model_id(value);
    VERSIONED.is_match(&normalized) || OFFICIAL.is_match(&normalized)
}

pub const DEEPSEEK_OFFICIAL_PROVIDER: &str = "deepseek";

/// On the official API, deepseek-v4-flash is served by V4.1 Flash since 2026-09-10.
pub fn is_retired_official_deepseek_v4_flash_alias(
    model: ModelWithPromptPresetMetadata<'_>,
) -> bool {
    model.provider == DEEPSEEK_OFFICIAL_PROVIDER && has_deepseek_v4_flash_signal(model.id)
}

pub fn is_deepseek_v41_flash_model(model: ModelWithPromptPresetMetadata<'_>) -> bool {
    has_deepseek_v41_flash_signal(model.id)
        || model.name.is_some_and(has_deepseek_v41_flash_signal)
        || is_retired_official_deepseek_v4_flash_alias(model)
}

signal!(
    has_deepseek_v4_pro_signal,
    is_deepseek_v4_pro_model,
    r"(?:^|[/@:._-])deepseek[._-]v4[._-]pro(?:$|[/@:._-])"
);
signal!(
    has_glm52_signal,
    is_glm52_model,
    r"(?:^|[/@._-])glm(?:[._-]|p)5(?:[._-]|p)2(?:$|[/@._:-])"
);
signal!(
    has_glm53_signal,
    is_glm53_model,
    r"(?:^|[/@._-])glm(?:[._-]|p)5(?:[._-]|p)3(?:$|[/@._:-])"
);
signal!(
    has_grok45_signal,
    is_grok45_model,
    r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?5(?:$|[/@._:-])"
);
signal!(
    has_grok46_signal,
    is_grok46_model,
    r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?6(?:$|[/@._:-])"
);
signal!(
    has_grok47_signal,
    is_grok47_model,
    r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?7(?:$|[/@._:-])"
);

// Claude Mythos shares each Fable release's prompting guide, so Mythos ids route to the matching Fable preset.
pub const CLAUDE_FABLE_51_MARKERS: &[&str] =
    &["fable-5-1", "fable-5.1", "mythos-5-1", "mythos-5.1"];
pub const CLAUDE_FABLE_5_MARKERS: &[&str] = &["fable-5", "mythos-5"];
pub const CLAUDE_OPUS_55_MARKERS: &[&str] = &["opus-5-5", "opus-5.5"];
pub const CLAUDE_SONNET_55_MARKERS: &[&str] = &["sonnet-5-5", "sonnet-5.5"];
pub const CLAUDE_HAIKU_55_MARKERS: &[&str] = &["haiku-5-5", "haiku-5.5"];

fn has_marker(model_id: &str, markers: &[&str]) -> bool {
    let normalized = normalize_model_id(model_id);
    markers.iter().any(|marker| normalized.contains(marker))
}

pub fn is_claude_fable51_model(model_id: &str) -> bool {
    has_marker(model_id, CLAUDE_FABLE_51_MARKERS)
}
pub fn is_claude_fable5_model(model_id: &str) -> bool {
    has_marker(model_id, CLAUDE_FABLE_5_MARKERS)
}
pub fn is_claude_opus55_model(model_id: &str) -> bool {
    has_marker(model_id, CLAUDE_OPUS_55_MARKERS)
}
pub fn is_claude_opus5_model(model_id: &str) -> bool {
    normalize_model_id(model_id).contains("opus-5")
}
/// Sonnet 5 keeps the default dynamic prompt; only the 5.5 release has a tuned core.
pub fn is_claude_sonnet55_model(model_id: &str) -> bool {
    has_marker(model_id, CLAUDE_SONNET_55_MARKERS)
}
/// Haiku 4.5 and older keep the default dynamic prompt; only the 5.5 release has a tuned core.
pub fn is_claude_haiku55_model(model_id: &str) -> bool {
    has_marker(model_id, CLAUDE_HAIKU_55_MARKERS)
}

// Bare gemini-4, other generations of the family and unknown siblings stay out.
signal!(
    has_gemini4_argon_signal,
    is_gemini4_argon_model,
    r"(?:^|[/@:._-])gemini[._-]?4[._-]argon(?:$|[/@:._-])"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeOpusVersion {
    ClaudeOpus48,
    ClaudeOpus47,
    ClaudeOpus46,
    ClaudeOpus45,
}
impl ClaudeOpusVersion {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeOpus48 => "claude-opus-4-8",
            Self::ClaudeOpus47 => "claude-opus-4-7",
            Self::ClaudeOpus46 => "claude-opus-4-6",
            Self::ClaudeOpus45 => "claude-opus-4-5",
        }
    }
}

pub fn extract_claude_opus_version(model_id: &str) -> Option<ClaudeOpusVersion> {
    let normalized = normalize_model_id(model_id);
    if normalized.contains("opus-4-8") {
        return Some(ClaudeOpusVersion::ClaudeOpus48);
    }
    if normalized.contains("opus-4-7") {
        return Some(ClaudeOpusVersion::ClaudeOpus47);
    }
    if normalized.contains("opus-4-6") {
        return Some(ClaudeOpusVersion::ClaudeOpus46);
    }
    if normalized.contains("opus-4-5") || normalized.contains("opus-4.5") {
        return Some(ClaudeOpusVersion::ClaudeOpus45);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model<'a>(id: &'a str, provider: &'a str) -> ModelWithPromptPresetMetadata<'a> {
        ModelMetadata {
            id,
            provider,
            name: None,
            prompt_preset: None,
        }
    }

    #[test]
    fn gpt6_family_accepts_tier_ids_and_rejects_bare_versions() {
        for id in [
            "gpt-6-sol",
            "gpt-6.1-sol-fast",
            "openai/gpt-6-luna",
            "openai-gpt-61-sol",
            "GPT-6.1 Sol",
        ] {
            assert!(has_gpt6_family_signal(id), "{id}");
        }
        for id in ["gpt-6", "gpt-6.1", "gpt-61", "gpt-6-mini", "sol"] {
            assert!(!has_gpt6_family_signal(id), "{id}");
        }
    }

    #[test]
    fn gpt5_versions_prefer_the_newest_marker() {
        assert_eq!(
            extract_gpt5_version("openai/gpt-5.6-pro"),
            Some(Gpt5Version::Gpt56)
        );
        assert_eq!(
            extract_gpt5_version("gpt-5.3-codex"),
            Some(Gpt5Version::Gpt53Codex)
        );
        assert_eq!(extract_gpt5_version("gpt-5"), None);
    }

    #[test]
    fn official_deepseek_v4_flash_is_v41_only_on_the_official_provider() {
        assert!(is_deepseek_v41_flash_model(model(
            "deepseek-v4-flash",
            "deepseek"
        )));
        assert!(!is_deepseek_v41_flash_model(model(
            "deepseek-v4-flash",
            "openrouter"
        )));
        assert!(is_deepseek_v41_flash_model(model(
            "deepseek-flash",
            "opencode-go"
        )));
    }

    #[test]
    fn kimi_code_product_ids_route_by_version() {
        assert!(has_kimi_k28_signal("kimi-for-coding"));
        assert!(has_kimi_k27_signal("kimi-for-coding-highspeed"));
        assert!(!has_kimi_k28_signal("kimi-for-coding-highspeed"));
        assert!(has_kimi_k3_signal("K3"));
    }

    #[test]
    fn claude_opus_versions_and_markers() {
        assert_eq!(
            extract_claude_opus_version("claude-opus-4.5"),
            Some(ClaudeOpusVersion::ClaudeOpus45)
        );
        assert_eq!(
            extract_claude_opus_version("claude-opus-4-8"),
            Some(ClaudeOpusVersion::ClaudeOpus48)
        );
        assert!(is_claude_fable51_model("claude-mythos-5.1"));
        assert!(is_claude_sonnet55_model("Claude Sonnet 5.5"));
        assert!(!is_claude_haiku55_model("claude-haiku-4-5"));
    }
}
