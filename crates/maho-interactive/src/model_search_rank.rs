//! Favorites-aware relevance ranking, ported from pinned model-search-rank.ts.
use crate::model_search::ModelSearchItem;
use maho_tui::fuzzy::fuzzy_match;
use std::cmp::Ordering;

#[derive(Clone, Copy)]
struct TokenMatch { tier: usize, field_weight: usize, cost: f64 }

fn match_field(token: &str, field: &str) -> Option<(usize, f64)> {
    if field.is_empty() { return None; }
    if field == token { return Some((0, 0.0)); }
    let mut best = None;
    for (index, _) in field.match_indices(token) {
        let left = index == 0 || !field.as_bytes()[index - 1].is_ascii_alphanumeric();
        let end = index + token.len();
        let right = end == field.len() || !field.as_bytes()[end].is_ascii_alphanumeric();
        let tier = if left && right { 1 } else if left || right { 2 } else { 3 };
        if best.is_none_or(|(previous, _)| tier < previous) {
            best = Some((tier, field[..index].encode_utf16().count() as f64));
        }
    }
    best.or_else(|| {
        let fuzzy = fuzzy_match(token, field);
        fuzzy.matches.then_some((4, fuzzy.score))
    })
}

fn match_token(token: &str, fields: &[String; 4]) -> Option<TokenMatch> {
    let mut best: Option<TokenMatch> = None;
    for (index, field) in fields.iter().enumerate() {
        let Some((tier, cost)) = match_field(token, field) else { continue; };
        let field_weight = if tier == 0 { [1, 3, 2, 0][index] } else { index };
        let candidate = TokenMatch { tier, field_weight, cost };
        if best.is_none_or(|previous| {
            (tier, field_weight).cmp(&(previous.tier, previous.field_weight))
                .then_with(|| cost.total_cmp(&previous.cost)) == Ordering::Less
        }) { best = Some(candidate); }
    }
    best
}

fn score_plan(tokens: &[&str], fields: &[String; 4]) -> Option<[f64; 7]> {
    let mut score = [0.0; 7];
    for token in tokens {
        let matched = match_token(token, fields)?;
        if matched.tier > 0 { score[4 - matched.tier] += 1.0; }
        score[4] += matched.field_weight as f64;
        score[if matched.tier == 4 { 5 } else { 6 }] += matched.cost;
    }
    Some(score)
}

fn compare_keys(a: &[f64], b: &[f64]) -> Ordering {
    a.iter().zip(b).map(|(a, b)| a.total_cmp(b)).find(|order| !order.is_eq())
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}

pub fn rank_model_search_items<'a, T>(
    items: &'a [T], query: &str, get_model: impl Fn(&T) -> ModelSearchItem,
    favorites_first: bool, is_favorite: impl Fn(&T) -> bool,
) -> Vec<&'a T> {
    let query = query.trim().to_lowercase();
    let normalized = query.split('/').map(str::trim).collect::<Vec<_>>().join("/");
    if normalized.is_empty() { return items.iter().collect(); }
    let compound: Vec<_> = normalized.split_whitespace().collect();
    let legacy: Vec<_> = normalized.split(|c: char| c.is_whitespace() || c == '/')
        .filter(|token| !token.is_empty()).collect();
    let mut ranked = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let model = get_model(item);
        let id = model.id.to_lowercase();
        let provider = model.provider.to_lowercase();
        let fields = [id, model.name.unwrap_or_default().to_lowercase(), provider.clone(),
            format!("{provider}/{}", model.id.to_lowercase())];
        let score = [score_plan(&compound, &fields), score_plan(&legacy, &fields)]
            .into_iter().flatten().min_by(|a, b| compare_keys(a, b));
        let Some(score) = score else { continue; };
        let mut key = vec![f64::from(normalized != fields[3]),
            f64::from(favorites_first && !is_favorite(item))];
        key.extend(score);
        key.push(fields[0].encode_utf16().count() as f64);
        key.push(index as f64);
        ranked.push((item, key));
    }
    ranked.sort_by(|(_, a), (_, b)| compare_keys(a, b));
    ranked.into_iter().map(|(item, _)| item).collect()
}
