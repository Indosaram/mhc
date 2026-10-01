#[derive(Clone, Debug, Default)]
pub struct DreamScoreOperands { pub search_hits: f64, pub unreflected_steps: f64, pub recency: f64, pub source_count: f64, pub size_fit: f64, pub is_current: f64, pub penalty: f64 }
#[derive(Clone, Debug)]
pub struct DreamScoredConversation { pub conversation_id: String, pub total_bytes: usize, pub last_activity_ms: f64, pub operands: DreamScoreOperands, pub score: f64 }
#[derive(Debug, PartialEq, Eq)]
pub struct DreamSelection { pub conversation_ids: Vec<String>, pub total_bytes: usize }
pub struct DreamScoreInput<'a> {
    pub search_match_count: f64, pub steps_since_last_successful_reflection: f64, pub last_activity: &'a str,
    pub distinct_source_count: f64, pub total_bytes: f64, pub target_bytes: f64, pub is_current: bool,
    pub newest_message_covered: bool, pub now_ms: f64,
}
pub fn score_dream_candidate(input: &DreamScoreInput<'_>) -> (DreamScoreOperands, f64) {
    let recency = chrono::DateTime::parse_from_rfc3339(input.last_activity).ok().map(|time| {
        let age_hours = (input.now_ms - time.timestamp_millis().to_string().parse::<f64>().unwrap_or(f64::NAN)).max(0.0) / 3_600_000.0;
        if age_hours.is_finite() { (-age_hours / 168.0).exp().min(1.0) } else { 0.0 }
    }).unwrap_or(0.0);
    let operands = DreamScoreOperands {
        search_hits: input.search_match_count.clamp(0.0, 10.0) / 10.0,
        unreflected_steps: input.steps_since_last_successful_reflection.clamp(0.0, 50.0) / 50.0,
        recency, source_count: input.distinct_source_count.clamp(0.0, 200.0) / 200.0,
        size_fit: if input.total_bytes <= input.target_bytes || input.total_bytes == 0.0 { 1.0 } else { (input.target_bytes / input.total_bytes).min(1.0) },
        is_current: if input.is_current { 1.0 } else { 0.0 }, penalty: if input.newest_message_covered { 1.0 } else { 0.0 },
    };
    let score = 50.0 * operands.search_hits + operands.unreflected_steps + operands.recency + operands.source_count + operands.size_fit + operands.is_current - operands.penalty;
    (operands, score)
}
pub fn compare_conversation_ids(left: &str, right: &str) -> std::cmp::Ordering { left.encode_utf16().cmp(right.encode_utf16()) }
pub fn rank_dream_candidates(candidates: &[DreamScoredConversation]) -> Vec<DreamScoredConversation> {
    let mut ranked = candidates.to_vec();
    ranked.sort_by(|left, right| right.score.total_cmp(&left.score).then_with(|| right.operands.unreflected_steps.total_cmp(&left.operands.unreflected_steps)).then_with(|| right.last_activity_ms.total_cmp(&left.last_activity_ms)).then_with(|| compare_conversation_ids(&left.conversation_id, &right.conversation_id)));
    ranked
}
pub fn pack_dream_candidates(ranked: &[DreamScoredConversation], max_conversations: usize, max_bytes: usize) -> DreamSelection {
    let mut result = DreamSelection { conversation_ids: Vec::new(), total_bytes: 0 };
    for candidate in ranked {
        if result.conversation_ids.len() >= max_conversations { break; }
        if candidate.total_bytes > max_bytes - result.total_bytes { continue; }
        result.conversation_ids.push(candidate.conversation_id.clone()); result.total_bytes += candidate.total_bytes;
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scored(id: &str, steps: f64, activity: f64, bytes: usize) -> DreamScoredConversation { DreamScoredConversation { conversation_id: id.into(), total_bytes: bytes, last_activity_ms: activity, operands: DreamScoreOperands { unreflected_steps: steps, ..Default::default() }, score: 7.0 } }
    #[test]
    fn fixed_scoring_vector() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-10T12:00:00.000Z").unwrap().timestamp_millis().to_string().parse().unwrap();
        let (operands, score) = score_dream_candidate(&DreamScoreInput { search_match_count: 3.0, steps_since_last_successful_reflection: 25.0, last_activity: "2026-08-03T12:00:00.000Z", distinct_source_count: 100.0, total_bytes: 30.0, target_bytes: 20.0, is_current: true, newest_message_covered: true, now_ms: now });
        assert!((operands.recency - (-1.0f64).exp()).abs() < 1e-12); assert!((score - (15.0 + 0.5 + (-1.0f64).exp() + 0.5 + 2.0 / 3.0)).abs() < 1e-12);
    }
    #[test]
    fn rank_ties_by_steps_activity_and_id() { let ranked = rank_dream_candidates(&[scored("zeta",0.8,1.0,1),scored("beta",0.7,2.0,1),scored("alpha",0.7,2.0,1)]); assert_eq!(ranked.iter().map(|candidate| candidate.conversation_id.as_str()).collect::<Vec<_>>(), ["zeta","alpha","beta"]); }
    #[test]
    fn oversized_candidate_skipped() { let ranked = [scored("first",0.0,0.0,6),scored("oversized",0.0,0.0,11),scored("later",0.0,0.0,4)]; assert_eq!(pack_dream_candidates(&ranked,3,10), DreamSelection { conversation_ids: vec!["first".into(),"later".into()], total_bytes: 10 }); assert_eq!(pack_dream_candidates(&ranked,1,10).conversation_ids, ["first"]); }
}
