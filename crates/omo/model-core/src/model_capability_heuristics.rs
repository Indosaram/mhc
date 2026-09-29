use std::sync::LazyLock;

use regex::Regex;

use crate::model_normalization::normalize_model_id;
use crate::model_string_parser::parse_variant_from_model_id;
use crate::reasoning_level::SplitReasoningSuffixOptions;

/// A heuristic model family: matched by `pattern` or any `includes` substring.
/// Families are compared by name; the registry holds one definition per family.
#[derive(Debug, Clone, Copy)]
pub struct HeuristicModelFamilyDefinition {
    pub family: &'static str,
    pub includes: Option<&'static [&'static str]>,
    pub pattern: Option<fn(&str) -> bool>,
    pub variants: Option<&'static [&'static str]>,
    pub reasoning_efforts: Option<&'static [&'static str]>,
    /// `(requested, family value)` pairs.
    pub reasoning_effort_aliases: Option<&'static [(&'static str, &'static str)]>,
    pub supports_temperature: Option<bool>,
    pub supports_thinking: Option<bool>,
}

impl HeuristicModelFamilyDefinition {
    const fn named(family: &'static str) -> Self {
        Self {
            family,
            includes: None,
            pattern: None,
            variants: None,
            reasoning_efforts: None,
            reasoning_effort_aliases: None,
            supports_temperature: None,
            supports_thinking: None,
        }
    }

    #[must_use]
    pub fn reasoning_effort_alias(&self, requested: &str) -> Option<&'static str> {
        self.reasoning_effort_aliases?
            .iter()
            .find(|(from, _)| *from == requested)
            .map(|(_, to)| *to)
    }
}

#[expect(clippy::expect_used, reason = "static regex literals are valid")]
fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

fn claude_opus_pattern(model_id: &str) -> bool {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| regex(r"claude(?:-[0-9]+(?:-[0-9]+)*)?-opus"));
    PATTERN.is_match(model_id)
}

fn openai_reasoning_pattern(model_id: &str) -> bool {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| regex(r"(?:^|/)o[0-9](?:$|-)"));
    PATTERN.is_match(model_id)
}

/// `-thinking`/`-think` suffixed kimi/k2 ids, or kimi-for-coding k2p* ids (k2p5, k2-p6, k2.p6).
fn kimi_thinking_pattern(model_id: &str) -> bool {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        regex(r"(?:kimi.*-(?:thinking|think)|k2(?:.*-(?:thinking|think)|[-.]?p[0-9]))")
    });
    PATTERN.is_match(model_id)
}

/// `(?:kimi|k2(?![-.]?p\d))`: "kimi" anywhere, or a "k2" not followed by an optional separator,
/// "p" and a digit (those are kimi-for-coding thinking models).
fn kimi_pattern(model_id: &str) -> bool {
    static K2P: LazyLock<Regex> = LazyLock::new(|| regex(r"^[-.]?p[0-9]"));
    model_id.contains("kimi")
        || model_id
            .match_indices("k2")
            .any(|(start, _)| !K2P.is_match(&model_id[start + 2..]))
}

const LOW_MEDIUM_HIGH: &[&str] = &["low", "medium", "high"];
const LOW_TO_MAX: &[&str] = &["low", "medium", "high", "max"];
const OPENAI_REASONING_EFFORTS: &[&str] = &["none", "minimal", "low", "medium", "high"];
const HIGH_MAX: &[&str] = &["high", "max"];
const HIGH_MAX_ALIASES: &[(&str, &str)] = &[("low", "high"), ("medium", "high"), ("xhigh", "max")];

pub static HEURISTIC_MODEL_FAMILY_REGISTRY: [HeuristicModelFamilyDefinition; 16] = [
    HeuristicModelFamilyDefinition {
        pattern: Some(claude_opus_pattern),
        variants: Some(LOW_TO_MAX),
        supports_thinking: Some(true),
        ..HeuristicModelFamilyDefinition::named("claude-opus")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["claude"]),
        variants: Some(LOW_MEDIUM_HIGH),
        supports_thinking: Some(true),
        ..HeuristicModelFamilyDefinition::named("claude-non-opus")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["o3-deep-research", "o4-mini-deep-research"]),
        variants: Some(LOW_MEDIUM_HIGH),
        reasoning_efforts: Some(OPENAI_REASONING_EFFORTS),
        supports_temperature: Some(true),
        ..HeuristicModelFamilyDefinition::named("openai-deep-research")
    },
    HeuristicModelFamilyDefinition {
        pattern: Some(openai_reasoning_pattern),
        variants: Some(LOW_MEDIUM_HIGH),
        reasoning_efforts: Some(OPENAI_REASONING_EFFORTS),
        supports_temperature: Some(false),
        ..HeuristicModelFamilyDefinition::named("openai-reasoning")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["gpt-5"]),
        variants: Some(&["low", "medium", "high", "xhigh"]),
        reasoning_efforts: Some(&["none", "minimal", "low", "medium", "high", "xhigh", "max"]),
        ..HeuristicModelFamilyDefinition::named("gpt-5")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["gpt"]),
        variants: Some(LOW_MEDIUM_HIGH),
        ..HeuristicModelFamilyDefinition::named("gpt-legacy")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["gemini"]),
        variants: Some(LOW_MEDIUM_HIGH),
        ..HeuristicModelFamilyDefinition::named("gemini")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["qwen"]),
        ..HeuristicModelFamilyDefinition::named("qwen")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["grok"]),
        variants: Some(LOW_MEDIUM_HIGH),
        reasoning_efforts: Some(LOW_MEDIUM_HIGH),
        ..HeuristicModelFamilyDefinition::named("grok")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["kimi-thinking", "k2-thinking", "k2-think"]),
        pattern: Some(kimi_thinking_pattern),
        variants: Some(LOW_MEDIUM_HIGH),
        supports_thinking: Some(true),
        ..HeuristicModelFamilyDefinition::named("kimi-thinking")
    },
    HeuristicModelFamilyDefinition {
        pattern: Some(kimi_pattern),
        variants: Some(LOW_MEDIUM_HIGH),
        supports_thinking: Some(false),
        ..HeuristicModelFamilyDefinition::named("kimi")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["glm"]),
        variants: Some(LOW_TO_MAX),
        reasoning_efforts: Some(HIGH_MAX),
        reasoning_effort_aliases: Some(HIGH_MAX_ALIASES),
        ..HeuristicModelFamilyDefinition::named("glm")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["minimax"]),
        variants: Some(LOW_MEDIUM_HIGH),
        supports_thinking: Some(false),
        ..HeuristicModelFamilyDefinition::named("minimax")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["deepseek"]),
        variants: Some(LOW_TO_MAX),
        reasoning_efforts: Some(HIGH_MAX),
        reasoning_effort_aliases: Some(HIGH_MAX_ALIASES),
        ..HeuristicModelFamilyDefinition::named("deepseek")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["mistral", "codestral"]),
        variants: Some(LOW_MEDIUM_HIGH),
        ..HeuristicModelFamilyDefinition::named("mistral")
    },
    HeuristicModelFamilyDefinition {
        includes: Some(&["llama"]),
        variants: Some(LOW_MEDIUM_HIGH),
        ..HeuristicModelFamilyDefinition::named("llama")
    },
];

/// First registry family whose pattern or `includes` matches the normalized, variant-stripped id.
#[must_use]
pub fn detect_heuristic_model_family(
    model_id: &str,
) -> Option<&'static HeuristicModelFamilyDefinition> {
    let parsed_model = parse_variant_from_model_id(
        model_id,
        SplitReasoningSuffixOptions {
            allow_max_suffix: Some(true),
        },
    );
    let normalized_model_id = normalize_model_id(&parsed_model.model_id).to_lowercase();

    HEURISTIC_MODEL_FAMILY_REGISTRY.iter().find(|definition| {
        definition
            .pattern
            .is_some_and(|pattern| pattern(&normalized_model_id))
            || definition.includes.is_some_and(|includes| {
                includes
                    .iter()
                    .any(|value| normalized_model_id.contains(value))
            })
    })
}
