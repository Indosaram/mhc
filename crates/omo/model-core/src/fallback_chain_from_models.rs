use crate::fallback_model_object::FallbackModelEntry;
use crate::fallback_model_object::FallbackModelObject;
use crate::fallback_model_object::FallbackModelsConfig;
use crate::model_requirement_types::FallbackEntry;
use crate::model_resolver::normalize_fallback_models;
use crate::model_string_parser::PARENTHESIZED_VARIANT;
use crate::model_string_parser::ParsedVariant;
use crate::model_string_parser::SPACE_VARIANT;
use crate::reasoning_level::SplitReasoningSuffixOptions;
use crate::reasoning_level::split_reasoning_suffix;

pub const DEFAULT_PROVIDER_ID: &str = "opencode";

/// Unlike `parse_variant_from_model_id`, the `:level` suffix is tried first and a bare `:max`
/// stays attached (no provider prefix at this point).
fn parse_variant_from_model(raw_model: &str) -> ParsedVariant {
    let parsed = split_reasoning_suffix(raw_model, SplitReasoningSuffixOptions::default());
    if parsed.level.is_some() {
        return ParsedVariant {
            model_id: parsed.base,
            variant: parsed.level,
        };
    }

    let trimmed_model = raw_model.trim();
    if trimmed_model.is_empty() {
        return ParsedVariant {
            model_id: String::new(),
            variant: None,
        };
    }

    if let Some(captures) = PARENTHESIZED_VARIANT.captures(trimmed_model) {
        let model_id = captures
            .get(1)
            .map_or("", |m| m.as_str())
            .trim()
            .to_string();
        let variant = captures
            .get(2)
            .map(|m| m.as_str().trim().to_string())
            .filter(|variant| !variant.is_empty());
        return ParsedVariant { model_id, variant };
    }

    if let Some(captures) = SPACE_VARIANT.captures(trimmed_model) {
        let model_id = captures
            .get(1)
            .map_or("", |m| m.as_str())
            .trim()
            .to_string();
        let variant = captures
            .get(2)
            .map_or(String::new(), |m| m.as_str().trim().to_lowercase());
        if !variant.is_empty() {
            return ParsedVariant {
                model_id,
                variant: Some(variant),
            };
        }
    }

    ParsedVariant {
        model_id: trimmed_model.to_string(),
        variant: None,
    }
}

/// Parses `[provider/]model[variant syntax]` into a single-provider entry.
#[must_use]
pub fn parse_fallback_model_entry(
    model: &str,
    context_provider_id: Option<&str>,
    default_provider_id: &str,
) -> Option<FallbackEntry> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (provider_id, raw_model_id) = match trimmed.split_once('/') {
        Some((provider, rest)) => (provider.trim().to_string(), rest.trim().to_string()),
        None => {
            let context = context_provider_id
                .map(str::trim)
                .filter(|id| !id.is_empty());
            (
                context.unwrap_or(default_provider_id).to_string(),
                trimmed.to_string(),
            )
        }
    };
    if provider_id.is_empty() || raw_model_id.is_empty() {
        return None;
    }

    let parsed = parse_variant_from_model(&raw_model_id);
    if parsed.model_id.is_empty() {
        return None;
    }

    Some(FallbackEntry {
        providers: vec![provider_id],
        model: parsed.model_id,
        variant: parsed.variant,
        ..FallbackEntry::default()
    })
}

/// Object entries keep their explicit settings; an explicit variant beats an inline one.
#[must_use]
pub fn parse_fallback_model_object_entry(
    object: &FallbackModelObject,
    context_provider_id: Option<&str>,
    default_provider_id: &str,
) -> Option<FallbackEntry> {
    let base = parse_fallback_model_entry(&object.model, context_provider_id, default_provider_id)?;
    Some(FallbackEntry {
        variant: object.variant.clone().or(base.variant),
        reasoning: object.reasoning.clone(),
        reasoning_effort: object.reasoning_effort.clone(),
        temperature: object.temperature,
        top_p: object.top_p,
        max_tokens: object.max_tokens,
        thinking: object.thinking.clone(),
        ..base
    })
}

/// The entry whose `provider/model` is the longest case-insensitive prefix of `provider/modelID`.
#[must_use]
pub fn find_most_specific_fallback_entry<'a>(
    provider_id: &str,
    model_id: &str,
    chain: &'a [FallbackEntry],
) -> Option<&'a FallbackEntry> {
    let resolved = format!("{provider_id}/{model_id}").to_lowercase();
    let mut best: Option<(&FallbackEntry, usize)> = None;
    for entry in chain {
        let Some(match_len) = entry.providers.iter().find_map(|provider| {
            let candidate = format!("{provider}/{}", entry.model).to_lowercase();
            resolved.starts_with(&candidate).then_some(candidate.len())
        }) else {
            continue;
        };
        // Stable: an equal-length later match does not displace an earlier one.
        if best.is_none_or(|(_, best_len)| match_len > best_len) {
            best = Some((entry, match_len));
        }
    }
    best.map(|(entry, _)| entry)
}

/// Builds a chain from `fallback_models` config, dropping entries that fail to parse.
#[must_use]
pub fn build_fallback_chain_from_models(
    fallback_models: Option<&FallbackModelsConfig>,
    context_provider_id: Option<&str>,
    default_provider_id: &str,
) -> Option<Vec<FallbackEntry>> {
    let parsed: Vec<FallbackEntry> = normalize_fallback_models(fallback_models)?
        .iter()
        .filter_map(|entry| match entry {
            FallbackModelEntry::Model(model) => {
                parse_fallback_model_entry(model, context_provider_id, default_provider_id)
            }
            FallbackModelEntry::Object(object) => {
                parse_fallback_model_object_entry(object, context_provider_id, default_provider_id)
            }
        })
        .collect();
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}
