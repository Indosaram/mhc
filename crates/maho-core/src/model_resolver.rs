use maho_ai::types::{Model, ModelThinkingLevel, ThinkingSelection, ThinkingSelectionSource};
use maho_ai::cursor::model_capabilities::get_cursor_variant_alias;

#[derive(Debug, Clone, Default)]
pub struct ParsedModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub service_tier: Option<String>,
    pub warning: Option<String>,
}

pub fn get_model_narrowing_patterns<'a>(cli: Option<&'a [String]>, legacy: Option<&'a [String]>) -> &'a [String] {
    cli.or(legacy).unwrap_or(&[])
}

pub fn find_exact_model_reference_match<'a>(reference: &str, models: &'a [Model]) -> Option<&'a Model> {
    let reference = reference.trim();
    if reference.is_empty() { return None; }
    let canonical: Vec<_> = models.iter().filter(|m| format!("{}/{}", m.provider, m.id).eq_ignore_ascii_case(reference)).collect();
    match canonical.as_slice() { [model] => return Some(model), [] => {}, _ => return None }
    if let Some((provider, id)) = reference.split_once('/') {
        let matches: Vec<_> = models.iter().filter(|m| m.provider.eq_ignore_ascii_case(provider.trim()) && m.id.eq_ignore_ascii_case(id.trim())).collect();
        match matches.as_slice() { [model] => return Some(model), [] => {}, _ => return None }
    }
    let matches: Vec<_> = models.iter().filter(|m| m.id.eq_ignore_ascii_case(reference)).collect();
    match matches.as_slice() { [model] => Some(model), _ => None }
}

fn legacy_resolution(reference: &str, models: &[Model]) -> Option<ParsedModelResult> {
    let reference = reference.trim();
    let (provider, variant) = reference.split_once('/').map_or((None, reference), |(p, id)| (Some(p), id));
    let eligible = |model: &&Model| matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth") && provider.is_none_or(|p| model.provider.eq_ignore_ascii_case(p));
    if let Some(alias) = get_cursor_variant_alias(variant) {
        let candidates: Vec<_> = models.iter().filter(eligible).filter(|m| m.id == alias.target_id).collect();
        let [model] = candidates.as_slice() else { return None; };
        return Some(ParsedModelResult { model: Some((*model).clone()), thinking_level: alias.level,
            thinking_selection: alias.level.map(|level| ThinkingSelection { level, source: ThinkingSelectionSource::LegacyVariant, legacy_variant_id: Some(variant.into()) }), ..Default::default() });
    }
    let candidates: Vec<_> = models.iter().filter(|m| provider.is_none() || eligible(m)).cloned().collect();
    if find_exact_model_reference_match(reference, &candidates).is_some() { return None; }
    let mut derived = Vec::new();
    for model in models.iter().filter(eligible) {
        if let Some(variants) = model.compat.as_ref().and_then(|c| c.0.get("cursorReasoning")).and_then(|v| v.get("variantIds")).and_then(serde_json::Value::as_object) {
            for (level, id) in variants {
                if let (Some(level), Some(id)) = (ModelThinkingLevel::parse(level), id.as_str()) && id.eq_ignore_ascii_case(variant) {
                    derived.push(ParsedModelResult { model: Some(model.clone()), thinking_level: Some(level), thinking_selection: Some(ThinkingSelection {
                        level, source: ThinkingSelectionSource::LegacyVariant, legacy_variant_id: Some(id.into()),
                    }), ..Default::default() });
                }
            }
        }
    }
    if derived.len() == 1 { derived.pop() } else { None }
}

fn is_alias(id: &str) -> bool {
    id.ends_with("-latest") || !id.rsplit_once('-').is_some_and(|(_, suffix)| suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()))
}

fn try_match_model<'a>(pattern: &str, models: &'a [Model]) -> Option<&'a Model> {
    if let Some(model) = find_exact_model_reference_match(pattern, models) { return Some(model); }
    let pattern = pattern.to_lowercase();
    let mut matches: Vec<_> = models.iter().filter(|m| m.id.to_lowercase().contains(&pattern) || m.name.to_lowercase().contains(&pattern)).collect();
    matches.sort_by(|a, b| is_alias(&b.id).cmp(&is_alias(&a.id)).then_with(|| b.id.cmp(&a.id)));
    matches.first().copied()
}

pub fn parse_model_pattern(pattern: &str, models: &[Model], allow_invalid_thinking_level_fallback: bool) -> ParsedModelResult {
    if let Some(result) = legacy_resolution(pattern, models) { return result; }
    if let Some(model) = try_match_model(pattern, models) { return ParsedModelResult { model: Some(model.clone()), ..Default::default() }; }
    let Some((prefix, suffix)) = pattern.rsplit_once(':') else { return ParsedModelResult::default(); };
    let thinking = ModelThinkingLevel::parse(suffix);
    let tier = matches!(suffix, "auto" | "flex" | "priority");
    if thinking.is_none() && !tier && !allow_invalid_thinking_level_fallback { return ParsedModelResult::default(); }
    let mut result = parse_model_pattern(prefix, models, allow_invalid_thinking_level_fallback);
    if result.model.is_none() { return result; }
    if let Some(level) = thinking {
        result.thinking_level = if result.warning.is_some() { None } else { result.thinking_level.or(Some(level)) };
        result.thinking_selection = result.thinking_level.map(|level| ThinkingSelection { level, source: ThinkingSelectionSource::Explicit, legacy_variant_id: None });
    } else if tier {
        result.service_tier = if result.warning.is_some() { None } else { result.service_tier.or_else(|| Some(suffix.into())) };
    } else {
        result.thinking_level = None;
        result.thinking_selection = None;
        result.warning = Some(format!("Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."));
    }
    result
}

pub fn resolve_stored_model_reference(provider: &str, id: &str, models: &[Model]) -> Option<ParsedModelResult> {
    legacy_resolution(&format!("{provider}/{id}"), models).or_else(|| {
        models.iter().find(|m| m.provider == provider && m.id == id).map(|m| ParsedModelResult { model: Some(m.clone()), ..Default::default() })
    })
}
