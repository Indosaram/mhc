use maho_ai::types::{Model, ModelThinkingLevel};
use std::collections::HashSet;
use crate::model_resolver::{find_exact_model_reference_match, parse_model_pattern};
use super::expansion::{FallbackAuthTiers, MAX_PROVIDERS_PER_FAMILY, parse_bare_selector, rank_family_models};
pub use super::settings::FallbackChains;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackSelector {
    pub raw: String, pub provider: String, pub id: String, pub thinking_level: Option<ModelThinkingLevel>,
}

pub fn is_chain_tombstone(entries: Option<&[String]>) -> bool { entries.is_some_and(<[String]>::is_empty) }

pub fn parse_fallback_selector(raw: &str, models: &[Model]) -> Option<FallbackSelector> {
    let raw = raw.trim();
    if !raw.contains('/') || raw.contains('*') { return None; }
    if let Some(model) = find_exact_model_reference_match(raw, models) {
        return Some(FallbackSelector { raw: raw.into(), provider: model.provider.clone(), id: model.id.clone(), thinking_level: None });
    }
    let (reference, level) = raw.rsplit_once(':').and_then(|(prefix, suffix)| ModelThinkingLevel::parse(&suffix.to_lowercase()).map(|l| (prefix, Some(l)))).unwrap_or((raw, None));
    let (provider, pattern) = reference.split_once('/')?;
    if provider.trim().is_empty() || pattern.trim().is_empty() { return None; }
    let candidates: Vec<_> = models.iter().filter(|m| m.provider.eq_ignore_ascii_case(provider.trim())).cloned().collect();
    let pattern = level.map_or_else(|| pattern.trim().to_owned(), |l| format!("{}:{}", pattern.trim(), l.as_str()));
    let parsed = parse_model_pattern(&pattern, &candidates, false);
    if parsed.warning.is_some() || (level.is_some() && parsed.thinking_level.is_none()) { return None; }
    let model = parsed.model?;
    Some(FallbackSelector { raw: raw.into(), provider: model.provider, id: model.id, thinking_level: parsed.thinking_level })
}

pub fn format_selector(model: &Model, thinking: Option<ModelThinkingLevel>) -> String {
    let base = format!("{}/{}", model.provider, model.id);
    thinking.map_or_else(|| base.clone(), |level| format!("{base}:{}", level.as_str()))
}
pub fn base_selector(selector: &FallbackSelector) -> String { format!("{}/{}", selector.provider, selector.id) }
fn format_parsed(selector: &FallbackSelector) -> String {
    let base = base_selector(selector);
    selector.thinking_level.map_or_else(|| base.clone(), |l| format!("{base}:{}", l.as_str()))
}
fn normalized_base(selector: &str) -> String {
    let normalized = selector.trim().to_lowercase();
    normalized.rsplit_once(':').filter(|(_, suffix)| ModelThinkingLevel::parse(suffix).is_some()).map_or_else(|| normalized.clone(), |(prefix, _)| prefix.to_owned())
}

pub fn canonicalize_fallback_chains(chains: &FallbackChains, models: &[Model], tiers: &FallbackAuthTiers<'_>) -> FallbackChains {
    let mut canonical = FallbackChains::new();
    let mut explicit_keys = HashSet::new();
    let mut tombstones = HashSet::new();
    let expand = |entries: &[String], key: &str| -> Vec<String> {
        entries.iter().flat_map(|entry| {
            if let Some(bare) = parse_bare_selector(entry) {
                rank_family_models(models, &bare.family, tiers, Some(MAX_PROVIDERS_PER_FAMILY)).into_iter()
                    .map(|m| format_selector(m, bare.thinking_level)).filter(|s| normalized_base(s) != normalized_base(key)).collect()
            } else { parse_fallback_selector(entry, models).map(|p| vec![format_parsed(&p)]).unwrap_or_default() }
        }).collect()
    };
    for (key, entries) in chains {
        let Some(bare) = parse_bare_selector(key) else { continue; };
        for model in rank_family_models(models, &bare.family, tiers, None) {
            if entries.is_empty() { tombstones.insert(format_selector(model, None).to_lowercase()); continue; }
            let key = format_selector(model, bare.thinking_level);
            let entries = expand(entries, &key);
            if !entries.is_empty() { canonical.insert(key, entries); }
        }
    }
    for (key, entries) in chains {
        if parse_bare_selector(key).is_some() { continue; }
        let Some(parsed) = parse_fallback_selector(key, models) else { continue; };
        let key = format_parsed(&parsed);
        if entries.is_empty() { tombstones.insert(normalized_base(&key)); continue; }
        explicit_keys.insert(key.to_lowercase());
        let entries = expand(entries, &key);
        if !entries.is_empty() { canonical.insert(key, entries); }
    }
    canonical.retain(|key, _| explicit_keys.contains(&key.to_lowercase()) || !tombstones.contains(&normalized_base(key)));
    canonical
}

pub fn resolve_chain_key(model: &Model, thinking: Option<ModelThinkingLevel>, chains: &FallbackChains) -> Option<String> {
    let base = format_selector(model, None);
    let exact = format_selector(model, thinking);
    if chains.contains_key(&exact) { Some(exact) } else if chains.contains_key(&base) { Some(base) } else { None }
}

pub fn candidates_after<'a>(entries: &'a [String], current: &str) -> &'a [String] {
    let exact = current.trim().to_lowercase();
    let position = entries.iter().position(|entry| entry.to_lowercase() == exact)
        .or_else(|| entries.iter().position(|entry| normalized_base(entry) == normalized_base(current)));
    position.map_or(entries, |i| &entries[i + 1..])
}
