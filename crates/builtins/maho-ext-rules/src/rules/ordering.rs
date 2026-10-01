use std::cmp::Ordering;
use super::types::RuleCandidate;
fn source_priority(source: &str) -> usize { super::constants::SOURCE_PRIORITY.iter().find(|(name, _)| *name == source).map_or(usize::MAX, |(_, priority)| *priority) }
pub fn compare_candidates(a: &RuleCandidate, b: &RuleCandidate) -> Ordering { a.is_global.cmp(&b.is_global).then_with(|| a.distance.cmp(&b.distance)).then_with(|| source_priority(&a.source).cmp(&source_priority(&b.source))).then_with(|| a.relative_path.encode_utf16().cmp(b.relative_path.encode_utf16())).then_with(|| a.real_path.encode_utf16().cmp(b.real_path.encode_utf16())) }
pub fn sort_candidates(candidates: &[RuleCandidate]) -> Vec<RuleCandidate> { let mut sorted = candidates.to_vec(); sorted.sort_by(compare_candidates); sorted }
