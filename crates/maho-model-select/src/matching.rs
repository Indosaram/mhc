//! Availability matching (spec model-selection §3).

use regex::Regex;
use std::sync::OnceLock;

fn rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            (r"claude-(opus|sonnet|haiku)-(\d+)-(\d+)", "claude-$1-$2.$3"),
            (r"kimi-k2-(\d+)", "kimi-k2.$1"),
            (r"\bglm-(\d+)-(\d+)", "glm-$1.$2"),
            (r"\bgpt-(\d+)-(\d+)", "gpt-$1.$2"),
        ]
        .into_iter()
        .filter_map(|(pattern, replacement)| Regex::new(pattern).ok().map(|re| (re, replacement)))
        .collect()
    })
}

/// Lower-cases and treats `-` between version numbers as `.` for the families that publish
/// both spellings.
pub fn normalize_for_match(name: &str) -> String {
    rules()
        .iter()
        .fold(name.to_lowercase(), |text, (re, replacement)| {
            re.replace_all(&text, *replacement).into_owned()
        })
}

fn provider(entry: &str) -> &str {
    entry.split_once('/').map_or("", |(p, _)| p)
}

fn model_id(entry: &str) -> &str {
    entry.split_once('/').map_or("", |(_, m)| m)
}

/// One entry of `available` that serves `target`, or `None`.
pub fn match_available(
    target: &str,
    available: &[String],
    providers: Option<&[String]>,
) -> Option<String> {
    let wanted = normalize_for_match(target);
    let providers = providers.filter(|p| !p.is_empty());
    let rank = |entry: &str| -> usize {
        providers.map_or(0, |list| {
            list.iter()
                .position(|p| p == provider(entry))
                .unwrap_or(list.len())
        })
    };
    let candidates: Vec<(usize, &String, String)> = available
        .iter()
        .enumerate()
        .filter(|(_, entry)| providers.is_none_or(|list| list.iter().any(|p| p == provider(entry))))
        .map(|(index, entry)| (index, entry, normalize_for_match(entry)))
        .filter(|(_, _, normal)| normal.contains(&wanted))
        .collect();
    if let Some((_, entry, _)) = candidates.iter().find(|(_, _, normal)| *normal == wanted) {
        return Some((*entry).clone());
    }
    let best = |pool: Vec<&(usize, &String, String)>| {
        pool.into_iter()
            .min_by_key(|(index, entry, _)| (rank(entry), model_id(entry).len(), *index))
            .map(|(_, entry, _)| (*entry).clone())
    };
    let exact_id: Vec<&(usize, &String, String)> = candidates
        .iter()
        .filter(|(_, entry, _)| normalize_for_match(model_id(entry)) == wanted)
        .collect();
    if !exact_id.is_empty() {
        return best(exact_id);
    }
    best(candidates.iter().collect())
}

/// Whether `target` is served by some entry of `available`.
pub fn is_available(target: &str, available: &[String]) -> bool {
    match_available(target, available, None).is_some()
}
