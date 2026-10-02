pub use super::favorite_model_ids::{FavoriteModelIds, clear_favorite_models, favorite_models, get_sorted_favorite_model_ids, is_favorite_model, move_favorite_model, toggle_favorite_model};
use maho_core::model_resolver::PatternResolution;
use maho_ai::types::Model;

pub fn get_model_full_id(model: &Model) -> String { format!("{}/{}", model.provider, model.id) }

pub struct FavoritePatternsForPersist<'a> {
    pub stored_patterns: &'a [String],
    pub pattern_resolutions: &'a [PatternResolution],
    pub selected_ids: &'a FavoriteModelIds,
    pub candidate_ids: &'a [String],
}

pub fn merge_favorite_patterns_for_persist(options: FavoritePatternsForPersist<'_>) -> Option<Vec<String>> {
    let selected = options.selected_ids.as_deref().unwrap_or(options.candidate_ids);
    let mut accounted = std::collections::HashSet::new();
    let mut merged = Vec::new();
    let mut append = |pattern: String| { if !merged.contains(&pattern) { merged.push(pattern); } };
    for pattern in options.stored_patterns {
        let Some(resolution) = options.pattern_resolutions.iter().find(|resolution| &resolution.pattern == pattern) else { continue; };
        if resolution.unresolved { append(resolution.pattern.clone()); continue; }
        if resolution.owned_ids.is_empty() { continue; }
        let visible: Vec<_> = resolution.owned_ids.iter().filter(|id| options.candidate_ids.contains(id)).collect();
        let selected_visible: Vec<_> = visible.iter().copied().filter(|id| selected.contains(id)).collect();
        let positions: Vec<_> = selected_visible.iter().filter_map(|id| selected.iter().position(|candidate| candidate == *id)).collect();
        let ordered = positions.windows(2).all(|pair| pair[1] == pair[0] + 1);
        if selected_visible.len() == visible.len() && ordered {
            append(resolution.pattern.clone());
            accounted.extend(selected_visible.into_iter().cloned());
            continue;
        }
        if !resolution.is_glob { continue; }
        let exploded = selected.iter().filter(|id| selected_visible.contains(id)).chain(resolution.owned_ids.iter().filter(|id| !options.candidate_ids.contains(id)));
        for id in exploded {
            let mut exact = id.clone();
            if let Some(tier) = &resolution.service_tier { exact.push(':'); exact.push_str(tier); }
            if let Some(level) = resolution.thinking_level { exact.push(':'); exact.push_str(level.as_str()); }
            append(exact);
            if options.candidate_ids.contains(id) { accounted.insert(id.clone()); }
        }
    }
    for id in selected { if !accounted.contains(id) { append(id.clone()); } }
    (!merged.is_empty()).then_some(merged)
}
