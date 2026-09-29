//! Private adapter for the delegate-core / model-core APIs slice A needs.
//!
//! Sibling gap: the Rust `delegate-core` crate does not yet export `resolveModelForDelegateTask`
//! or `DelegateFallbackEntry`, and `model-core` exports none of `fuzzyMatchModel`,
//! `normalizeModel`, `parseModelString`, `parseVariantFromModelID` or `transformModelForProvider`.
//! This module mirrors those TypeScript functions exactly and should be replaced by the sibling
//! exports once they land (tracked in `parity.md` under "Sibling gaps").

use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;

/// `DelegateFallbackEntry`: one rung of a builtin fallback chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateFallbackEntry {
    pub providers: Vec<String>,
    pub model: String,
    pub variant: Option<String>,
}

impl DelegateFallbackEntry {
    pub(crate) fn new(providers: &[&str], model: &str, variant: Option<&str>) -> Self {
        Self {
            providers: providers
                .iter()
                .map(|provider| (*provider).to_string())
                .collect(),
            model: model.to_string(),
            variant: variant.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DelegateModelResolutionInput<'a> {
    pub(crate) user_model: Option<&'a str>,
    pub(crate) user_fallback_models: Option<&'a [String]>,
    pub(crate) category_default_model: Option<&'a str>,
    pub(crate) fallback_chain: Option<&'a [DelegateFallbackEntry]>,
    pub(crate) available_models: BTreeSet<String>,
    pub(crate) system_default_model: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DelegateModelResolution {
    pub(crate) model: String,
    pub(crate) variant: Option<String>,
    pub(crate) fallback_entry: Option<DelegateFallbackEntry>,
    pub(crate) matched_fallback: bool,
}

impl DelegateModelResolution {
    fn plain(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            variant: None,
            fallback_entry: None,
            matched_fallback: false,
        }
    }
}

const REASONING_LEVELS_OR_AUTO: [&str; 8] = [
    "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto",
];

pub(crate) fn normalize_model(model: Option<&str>) -> Option<String> {
    model
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_string)
}

fn split_reasoning_suffix(model: &str, allow_max_suffix: Option<bool>) -> (String, Option<String>) {
    let trimmed = model.trim();
    let Some(separator) = trimmed.rfind(':') else {
        return (trimmed.to_string(), None);
    };
    let base = trimmed[..separator].trim();
    let token = trimmed[separator + 1..].trim().to_lowercase();
    if base.is_empty() || !REASONING_LEVELS_OR_AUTO.contains(&token.as_str()) {
        return (trimmed.to_string(), None);
    }
    if token == "max" && !allow_max_suffix.unwrap_or_else(|| base.contains('/')) {
        return (trimmed.to_string(), None);
    }
    (base.to_string(), Some(token))
}

static PARENTHESIZED_VARIANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.*)\(([^()]+)\)\s*$").unwrap_or_else(|error| panic!("{error}"))
});
static SPACE_VARIANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(.*\S)\s+([a-z][a-z0-9_-]*)$").unwrap_or_else(|error| panic!("{error}"))
});

pub(crate) fn parse_variant_from_model_id(
    raw: &str,
    allow_max_suffix: Option<bool>,
) -> (String, Option<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (String::new(), None);
    }
    if let Some(captures) = PARENTHESIZED_VARIANT.captures(trimmed) {
        let model_id = captures
            .get(1)
            .map_or("", |m| m.as_str())
            .trim()
            .to_string();
        let variant = captures
            .get(2)
            .map(|m| m.as_str().trim().to_string())
            .filter(|variant| !variant.is_empty());
        return (model_id, variant);
    }
    let (base, level) = split_reasoning_suffix(trimmed, allow_max_suffix);
    if level.is_some() {
        return (base, level);
    }
    if let Some(captures) = SPACE_VARIANT.captures(trimmed) {
        let model_id = captures
            .get(1)
            .map_or("", |m| m.as_str())
            .trim()
            .to_string();
        let variant = captures
            .get(2)
            .map_or("", |m| m.as_str())
            .trim()
            .to_lowercase();
        if !variant.is_empty() {
            return (model_id, Some(variant));
        }
    }
    (trimmed.to_string(), None)
}

pub(crate) fn parse_model_string(model: &str) -> Option<(String, String, Option<String>)> {
    let trimmed = model.trim();
    let separator = trimmed.find('/')?;
    let provider = trimmed[..separator].trim();
    let raw_model_id = trimmed[separator + 1..].trim();
    if provider.is_empty() || raw_model_id.is_empty() {
        return None;
    }
    let (model_id, variant) = parse_variant_from_model_id(raw_model_id, Some(true));
    if model_id.is_empty() {
        return None;
    }
    Some((provider.to_string(), model_id, variant))
}

static CLAUDE_VERSION_DOT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"claude-(\w+)-(\d+)-(\d+)").unwrap_or_else(|error| panic!("{error}"))
});
static GEMINI_31_PRO_PREVIEW: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"gemini-3\.1-pro(?!-)").unwrap_or_else(|error| panic!("{error}"))
});
static GEMINI_3_FLASH_PREVIEW: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"(?<!antigravity-)gemini-3-flash(?!-)")
        .unwrap_or_else(|error| panic!("{error}"))
});

fn claude_version_dot(model: &str) -> String {
    CLAUDE_VERSION_DOT
        .replace_all(model, "claude-$1-$2.$3")
        .into_owned()
}

fn gemini_31_preview(model: &str) -> String {
    GEMINI_31_PRO_PREVIEW
        .replace_all(model, "gemini-3.1-pro-preview")
        .into_owned()
}

fn gemini_3_flash_preview(model: &str) -> String {
    GEMINI_3_FLASH_PREVIEW
        .replace_all(model, "gemini-3-flash-preview")
        .into_owned()
}

fn infer_sub_provider(model: &str) -> Option<&'static str> {
    [
        ("claude-", "anthropic"),
        ("gpt-", "openai"),
        ("gemini-", "google"),
        ("grok-", "xai"),
        ("minimax-", "minimax"),
        ("kimi-", "moonshotai"),
        ("k3", "moonshotai"),
        ("glm-", "zai"),
    ]
    .into_iter()
    .find(|(prefix, _)| model.starts_with(prefix))
    .map(|(_, provider)| provider)
}

pub(crate) fn transform_model_for_provider(provider: &str, model: &str) -> String {
    match provider {
        "vercel" => {
            if let Some((sub_provider, sub_model)) = model.split_once('/') {
                return format!(
                    "{sub_provider}/{}",
                    gemini_31_preview(&claude_version_dot(sub_model))
                );
            }
            match infer_sub_provider(model) {
                Some(sub_provider) => {
                    format!(
                        "{sub_provider}/{}",
                        gemini_31_preview(&claude_version_dot(model))
                    )
                }
                None => model.to_string(),
            }
        }
        "github-copilot" => gemini_3_flash_preview(&gemini_31_preview(&claude_version_dot(model))),
        "google" => gemini_3_flash_preview(&gemini_31_preview(model)),
        "kimi-coding" | "kimi-for-coding" => match model {
            "kimi-k3" => "k3".to_string(),
            "kimi-k3-256k" => "k3-256k".to_string(),
            _ => model.to_string(),
        },
        _ => model.to_string(),
    }
}

static CLAUDE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"claude-(opus|sonnet|haiku)-(\d+)[.-](\d+)")
        .unwrap_or_else(|error| panic!("{error}"))
});
static KIMI_K2: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"kimi-k2[.-](\d+)").unwrap_or_else(|error| panic!("{error}")));
static GLM_GPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(glm|gpt)-(\d+)[.-](\d+)").unwrap_or_else(|error| panic!("{error}"))
});

fn normalize_model_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let step = CLAUDE_NAME.replace_all(&lower, "claude-$1-$2.$3");
    let step = KIMI_K2.replace_all(&step, "kimi-k2.$1");
    GLM_GPT.replace_all(&step, "$1-$2.$3").into_owned()
}

fn shortest(candidates: Vec<&String>) -> Option<String> {
    candidates
        .into_iter()
        .reduce(|shortest, current| {
            if current.encode_utf16().count() < shortest.encode_utf16().count() {
                current
            } else {
                shortest
            }
        })
        .cloned()
}

/// `fuzzyMatchModel`: candidates iterate in insertion order in TypeScript; callers here always
/// pass sorted sets, so a sorted set reproduces the TypeScript iteration order.
pub(crate) fn fuzzy_match_model(
    target: &str,
    available: &BTreeSet<String>,
    providers: Option<&[String]>,
) -> Option<String> {
    if available.is_empty() {
        return None;
    }
    let target_normalized = normalize_model_name(target);
    let candidates: Vec<&String> = match providers.filter(|providers| !providers.is_empty()) {
        Some(providers) => {
            let provider_set: HashSet<&str> = providers.iter().map(String::as_str).collect();
            available
                .iter()
                .filter(|model| provider_set.contains(model.split('/').next().unwrap_or_default()))
                .collect()
        }
        None => available.iter().collect(),
    };
    let matches: Vec<&String> = candidates
        .into_iter()
        .filter(|model| normalize_model_name(model).contains(&target_normalized))
        .collect();
    if matches.is_empty() {
        return None;
    }
    if let Some(exact) = matches
        .iter()
        .find(|model| normalize_model_name(model) == target_normalized)
    {
        return Some((*exact).clone());
    }
    let exact_model_id: Vec<&String> = matches
        .iter()
        .copied()
        .filter(|model| {
            let model_id = model.split('/').skip(1).collect::<Vec<_>>().join("/");
            normalize_model_name(&model_id) == target_normalized
        })
        .collect();
    if !exact_model_id.is_empty() {
        return shortest(exact_model_id);
    }
    shortest(matches)
}

struct ParsedFallback {
    base_model: String,
    provider_hint: Option<Vec<String>>,
    variant: Option<String>,
}

fn parse_user_fallback_model(fallback: &str) -> Option<ParsedFallback> {
    let normalized = normalize_model(Some(fallback))?;
    if let Some((provider, model_id, variant)) = parse_model_string(&normalized) {
        return Some(ParsedFallback {
            base_model: format!("{provider}/{model_id}"),
            provider_hint: Some(vec![provider]),
            variant,
        });
    }
    let (model_id, variant) = parse_variant_from_model_id(&normalized, None);
    if model_id.is_empty() {
        return None;
    }
    Some(ParsedFallback {
        base_model: model_id,
        provider_hint: None,
        variant,
    })
}

fn is_explicit_high_model(model: &str) -> bool {
    model
        .rsplit('/')
        .next()
        .is_some_and(|last| last.len() > "-high".len() && last.ends_with("-high"))
}

/// `resolveModelForDelegateTask` with the deps every senpi-task caller passes: no connected
/// providers list and warm provider caches. `None` covers both `undefined` and `{ skipped }`.
pub(crate) fn resolve_model_for_delegate_task(
    input: &DelegateModelResolutionInput<'_>,
) -> Option<DelegateModelResolution> {
    let available = &input.available_models;
    if let Some(user_model) = normalize_model(input.user_model) {
        let parsed = parse_user_fallback_model(&user_model);
        let user_result = match &parsed {
            Some(ParsedFallback {
                base_model,
                variant: Some(variant),
                ..
            }) => DelegateModelResolution {
                variant: Some(variant.clone()),
                ..DelegateModelResolution::plain(base_model.clone())
            },
            _ => DelegateModelResolution::plain(user_model.clone()),
        };
        if let Some(fallbacks) = input
            .user_fallback_models
            .filter(|fallbacks| !fallbacks.is_empty())
            && !available.is_empty()
        {
            let provider_hint = parsed
                .as_ref()
                .and_then(|parsed| parsed.provider_hint.as_deref());
            if fuzzy_match_model(&user_result.model, available, provider_hint).is_none() {
                for fallback in fallbacks {
                    let Some(parsed_fallback) = parse_user_fallback_model(fallback) else {
                        continue;
                    };
                    if let Some(found) = fuzzy_match_model(
                        &parsed_fallback.base_model,
                        available,
                        parsed_fallback.provider_hint.as_deref(),
                    ) {
                        return Some(DelegateModelResolution {
                            model: found,
                            variant: parsed_fallback.variant,
                            fallback_entry: None,
                            matched_fallback: true,
                        });
                    }
                }
            }
        }
        return Some(user_result);
    }

    let category_default = normalize_model(input.category_default_model);
    let explicit_high = category_default
        .as_deref()
        .filter(|model| is_explicit_high_model(model))
        .map(|model| {
            (
                model.to_string(),
                model.trim_end_matches("-high").to_string(),
            )
        });
    if let Some(category_default) = &category_default {
        if available.is_empty() {
            return Some(DelegateModelResolution::plain(category_default.clone()));
        }
        let parts: Vec<&str> = category_default.split('/').collect();
        let provider_hint =
            (parts.len() >= 2 && !parts[0].is_empty()).then(|| vec![parts[0].to_string()]);
        if let Some(found) =
            fuzzy_match_model(category_default, available, provider_hint.as_deref())
        {
            if explicit_high.is_some() && found != *category_default {
                return Some(DelegateModelResolution::plain(category_default.clone()));
            }
            return Some(DelegateModelResolution::plain(found));
        }
    }

    if let Some(fallbacks) = input
        .user_fallback_models
        .filter(|fallbacks| !fallbacks.is_empty())
    {
        for fallback in fallbacks {
            let Some(parsed) = parse_user_fallback_model(fallback) else {
                continue;
            };
            if available.is_empty() {
                return Some(DelegateModelResolution {
                    model: parsed.base_model,
                    variant: parsed.variant,
                    fallback_entry: None,
                    matched_fallback: true,
                });
            }
            if let Some(found) = fuzzy_match_model(
                &parsed.base_model,
                available,
                parsed.provider_hint.as_deref(),
            ) {
                return Some(DelegateModelResolution {
                    model: found,
                    variant: parsed.variant,
                    fallback_entry: None,
                    matched_fallback: true,
                });
            }
        }
    }

    if let Some(chain) = input.fallback_chain.filter(|chain| !chain.is_empty()) {
        let rung = |entry: &DelegateFallbackEntry, model: String| {
            let explicit = explicit_high
                .as_ref()
                .filter(|(_, base)| entry.variant.as_deref() == Some("high") && model == *base);
            match explicit {
                Some((high_model, _)) => DelegateModelResolution {
                    model: high_model.clone(),
                    variant: None,
                    fallback_entry: Some(entry.clone()),
                    matched_fallback: true,
                },
                None => DelegateModelResolution {
                    model,
                    variant: entry.variant.clone(),
                    fallback_entry: Some(entry.clone()),
                    matched_fallback: true,
                },
            }
        };
        if available.is_empty() {
            let first = &chain[0];
            if let Some(provider) = first.providers.first() {
                return Some(DelegateModelResolution {
                    model: format!(
                        "{provider}/{}",
                        transform_model_for_provider(provider, &first.model)
                    ),
                    variant: first.variant.clone(),
                    fallback_entry: Some(first.clone()),
                    matched_fallback: true,
                });
            }
        } else {
            for (entry_index, entry) in chain.iter().enumerate() {
                for provider in &entry.providers {
                    let full = format!(
                        "{provider}/{}",
                        transform_model_for_provider(provider, &entry.model)
                    );
                    if let Some(found) =
                        fuzzy_match_model(&full, available, Some(std::slice::from_ref(provider)))
                    {
                        return Some(rung(entry, found));
                    }
                }
                let later_providers: HashSet<&str> = chain[entry_index + 1..]
                    .iter()
                    .filter(|candidate| candidate.model == entry.model)
                    .flat_map(|candidate| candidate.providers.iter().map(String::as_str))
                    .collect();
                let cross: BTreeSet<String> = available
                    .iter()
                    .filter(|model| {
                        let provider = model.split('/').next().unwrap_or_default();
                        !later_providers.contains(provider)
                    })
                    .cloned()
                    .collect();
                if let Some(found) = fuzzy_match_model(&entry.model, &cross, None) {
                    return Some(rung(entry, found));
                }
            }
        }
    }

    normalize_model(input.system_default_model).map(DelegateModelResolution::plain)
}
