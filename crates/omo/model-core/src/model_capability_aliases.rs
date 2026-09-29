use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

/// A one-to-one alias from a legacy model id to its canonical snapshot id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactAliasRule {
    pub alias_model_id: &'static str,
    pub rule_id: &'static str,
    pub canonical_model_id: &'static str,
    pub rationale: &'static str,
}

/// A predicate-driven alias family (e.g. tier suffixes) mapped to canonical ids.
#[derive(Debug, Clone, Copy)]
pub struct PatternAliasRule {
    pub rule_id: &'static str,
    pub description: &'static str,
    pub provider_ids: Option<&'static [&'static str]>,
    pub allowed_subprovider_hosts: Option<&'static [&'static str]>,
    pub matches: fn(&str) -> bool,
    pub canonicalize: fn(&str) -> String,
}

/// How a requested id reached its canonical form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AliasSource {
    Canonical,
    ExactAlias,
    PatternAlias,
}

impl AliasSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::ExactAlias => "exact-alias",
            Self::PatternAlias => "pattern-alias",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelIdAliasResolution {
    pub requested_model_id: String,
    pub canonical_model_id: String,
    pub source: AliasSource,
    pub rule_id: Option<String>,
}

const GEMINI_3_PRO_RATIONALE: &str =
    "Legacy Gemini 3 tier suffixes still need to land on the canonical preview model.";

static EXACT_ALIAS_RULES: [ExactAliasRule; 2] = [
    ExactAliasRule {
        alias_model_id: "gemini-3-pro-high",
        rule_id: "gemini-3-pro-tier-alias",
        canonical_model_id: "gemini-3-pro-preview",
        rationale: GEMINI_3_PRO_RATIONALE,
    },
    ExactAliasRule {
        alias_model_id: "gemini-3-pro-low",
        rule_id: "gemini-3-pro-tier-alias",
        canonical_model_id: "gemini-3-pro-preview",
        rationale: GEMINI_3_PRO_RATIONALE,
    },
];

#[expect(clippy::expect_used, reason = "static regex literals are valid")]
fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

fn matches_openai_fast(model_id: &str) -> bool {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| regex(r"^gpt-5\.6-(?:sol|terra|luna)-fast$"));
    PATTERN.is_match(model_id)
}

fn canonicalize_openai_fast(model_id: &str) -> String {
    model_id
        .strip_suffix("-fast")
        .unwrap_or(model_id)
        .to_string()
}

fn matches_claude_thinking_legacy(model_id: &str) -> bool {
    model_id == "claude-opus-4-7-thinking"
}

fn canonicalize_claude_thinking_legacy(_model_id: &str) -> String {
    "claude-opus-4-7".to_string()
}

fn matches_gemini_31_pro_tier(model_id: &str) -> bool {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| regex(r"^gemini-3\.1-pro-(?:high|low)$"));
    PATTERN.is_match(model_id)
}

fn canonicalize_gemini_31_pro_tier(_model_id: &str) -> String {
    "gemini-3.1-pro".to_string()
}

static PATTERN_ALIAS_RULES: [PatternAliasRule; 3] = [
    PatternAliasRule {
        rule_id: "openai-gpt-5.6-fast-service-tier-alias",
        description: "Normalizes OpenCode's OpenAI GPT-5.6 fast service-tier IDs to canonical snapshot IDs.",
        provider_ids: Some(&["openai"]),
        allowed_subprovider_hosts: Some(&["vercel"]),
        matches: matches_openai_fast,
        canonicalize: canonicalize_openai_fast,
    },
    PatternAliasRule {
        rule_id: "claude-thinking-legacy-alias",
        description: "Normalizes the legacy claude-opus-4-7-thinking id to the canonical snapshot ID.",
        provider_ids: None,
        allowed_subprovider_hosts: None,
        matches: matches_claude_thinking_legacy,
        canonicalize: canonicalize_claude_thinking_legacy,
    },
    PatternAliasRule {
        rule_id: "gemini-3.1-pro-tier-alias",
        description: "Normalizes Gemini 3.1 Pro tier suffixes to the canonical snapshot ID.",
        provider_ids: None,
        allowed_subprovider_hosts: None,
        matches: matches_gemini_31_pro_tier,
        canonicalize: canonicalize_gemini_31_pro_tier,
    },
];

fn normalize_lookup_model_id(model_id: &str) -> String {
    model_id.trim().to_lowercase()
}

fn strip_provider_prefix_for_alias_lookup(normalized_model_id: &str) -> &str {
    match normalized_model_id.find('/') {
        Some(slash_index) if slash_index > 0 && slash_index != normalized_model_id.len() - 1 => {
            &normalized_model_id[slash_index + 1..]
        }
        _ => normalized_model_id,
    }
}

fn rule_applies_to_provider(
    rule: &PatternAliasRule,
    provider_id: Option<&str>,
    embedded_provider_id: Option<&str>,
) -> bool {
    let Some(provider_ids) = rule.provider_ids else {
        return true;
    };
    let Some(provider_id) = provider_id else {
        return false;
    };
    let matches_provider_id = provider_ids.contains(&provider_id);
    let matches_embedded_provider_id = rule
        .allowed_subprovider_hosts
        .is_some_and(|hosts| hosts.contains(&provider_id))
        && embedded_provider_id.is_some_and(|embedded| provider_ids.contains(&embedded));
    matches_provider_id || matches_embedded_provider_id
}

/// Canonicalizes exact and pattern aliases; provider-scoped rules only apply to their providers.
#[must_use]
pub fn resolve_model_id_alias(model_id: &str, provider_id: Option<&str>) -> ModelIdAliasResolution {
    let requested_model_id = normalize_lookup_model_id(model_id);
    let alias_lookup_model_id =
        strip_provider_prefix_for_alias_lookup(&requested_model_id).to_string();
    let normalized_provider_id = provider_id
        .filter(|id| !id.is_empty())
        .map(normalize_lookup_model_id);
    let embedded_provider_id = requested_model_id
        .find('/')
        .filter(|end| *end > 0)
        .map(|end| requested_model_id[..end].to_string());

    if let Some(rule) = EXACT_ALIAS_RULES
        .iter()
        .find(|rule| rule.alias_model_id == alias_lookup_model_id)
    {
        return ModelIdAliasResolution {
            requested_model_id,
            canonical_model_id: rule.canonical_model_id.to_string(),
            source: AliasSource::ExactAlias,
            rule_id: Some(rule.rule_id.to_string()),
        };
    }

    for rule in &PATTERN_ALIAS_RULES {
        if !rule_applies_to_provider(
            rule,
            normalized_provider_id.as_deref(),
            embedded_provider_id.as_deref(),
        ) {
            continue;
        }
        if !(rule.matches)(&alias_lookup_model_id) {
            continue;
        }
        return ModelIdAliasResolution {
            canonical_model_id: (rule.canonicalize)(&alias_lookup_model_id),
            requested_model_id,
            source: AliasSource::PatternAlias,
            rule_id: Some(rule.rule_id.to_string()),
        };
    }

    ModelIdAliasResolution {
        requested_model_id,
        canonical_model_id: alias_lookup_model_id,
        source: AliasSource::Canonical,
        rule_id: None,
    }
}

#[must_use]
pub fn get_exact_model_id_alias_rules() -> &'static [ExactAliasRule] {
    &EXACT_ALIAS_RULES
}

#[must_use]
pub fn get_pattern_model_id_alias_rules() -> &'static [PatternAliasRule] {
    &PATTERN_ALIAS_RULES
}
