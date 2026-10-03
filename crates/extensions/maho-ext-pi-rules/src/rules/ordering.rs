use std::cmp::Ordering;
use super::{constants::SOURCE_PRIORITY, types::RuleCandidate};

pub fn compare_candidates(a: &RuleCandidate, b: &RuleCandidate) -> Ordering {
    let priority = |source: &str| SOURCE_PRIORITY.iter().find(|(name, _)| *name == source).map_or(usize::MAX, |(_, value)| *value);
    let text_order = |a: &str, b: &str| a.encode_utf16().cmp(b.encode_utf16());
    a.is_global.cmp(&b.is_global)
        .then(a.distance.cmp(&b.distance))
        .then(priority(&a.source).cmp(&priority(&b.source)))
        .then_with(|| text_order(&a.relative_path, &b.relative_path))
        .then_with(|| text_order(&a.real_path, &b.real_path))
}

pub fn sort_candidates(candidates: &[RuleCandidate]) -> Vec<RuleCandidate> {
    let mut result = candidates.to_vec();
    result.sort_by(compare_candidates);
    result
}
