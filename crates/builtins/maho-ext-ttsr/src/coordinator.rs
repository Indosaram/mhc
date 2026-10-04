use crate::types::{collapse_remediation, CONTROL_LEAK_REMEDIATION, DetectionOwner, DetectionResolution, DetectorMatch, GenerationDetectionState};
pub fn create_generation_state() -> GenerationDetectionState { GenerationDetectionState::default() }
pub fn resolve_detection(direct_leak: Option<&DetectorMatch>, collapse: Option<&DetectorMatch>, corroborated_leak: Option<&DetectorMatch>) -> Option<DetectionResolution> {
    if let Some(leak) = direct_leak.or(corroborated_leak) {
        let mut observed_rules = vec![DetectionOwner::ControlTokenLeak];
        if collapse.is_some() { observed_rules.push(DetectionOwner::CollapseRepetition); }
        return Some(DetectionResolution { owner: DetectionOwner::ControlTokenLeak, observed_rules, detection: leak.clone(), remediation: CONTROL_LEAK_REMEDIATION });
    }
    collapse.map(|detection| DetectionResolution { owner: DetectionOwner::CollapseRepetition, observed_rules: vec![DetectionOwner::CollapseRepetition], detection: detection.clone(), remediation: collapse_remediation() })
}
pub fn claim_abort(state: &mut GenerationDetectionState, resolution: &DetectionResolution, now_ms: u64) -> bool {
    if state.abort_claimed || state.user_cancelled { return false; }
    state.abort_claimed = true;
    state.abort_owner = Some(resolution.owner);
    state.self_abort_at = Some(now_ms);
    true
}
pub fn mark_user_cancelled(state: &mut GenerationDetectionState) { state.user_cancelled = true; }
#[cfg(test)] mod tests {
    use super::*;
    use crate::types::{DetectorRule, RuleRemediation};
    fn detection(rule: DetectorRule, offset: usize) -> DetectorMatch { DetectorMatch { rule, reason: String::new(), anomaly_start_offset: offset, garbage_start_offset: offset, detail: Default::default() } }
    fn collapse() -> DetectionResolution { resolve_detection(None, Some(&detection(DetectorRule::CollapseRepetition, 12)), None).unwrap() }
    fn leak() -> DetectionResolution { resolve_detection(Some(&detection(DetectorRule::ControlTokenLeak, 12)), None, None).unwrap() }
    #[test] fn no_matches_resolves_none() { let result = resolve_detection(None, None, None); assert!(result.is_none()); }
    #[test] fn direct_leak_owns_generation() { let matched = detection(DetectorRule::ControlTokenLeak, 12); let result = resolve_detection(Some(&matched), None, None).unwrap(); assert_eq!(result.owner, DetectionOwner::ControlTokenLeak); assert_eq!(result.observed_rules, [DetectionOwner::ControlTokenLeak]); assert_eq!(result.remediation.corruption_scope(), "generation"); }
    #[test] fn collapse_owns_output_region() { let result = collapse(); assert_eq!(result.owner, DetectionOwner::CollapseRepetition); assert_eq!(result.remediation.corruption_scope(), "output-region"); }
    #[test] fn leak_wins_and_collapse_stays_observed() { let leak = detection(DetectorRule::ControlTokenLeak, 12); let collapse = detection(DetectorRule::CollapseRepetition, 12); let result = resolve_detection(Some(&leak), Some(&collapse), None).unwrap(); assert_eq!(result.observed_rules, [DetectionOwner::ControlTokenLeak, DetectionOwner::CollapseRepetition]); assert_eq!(result.remediation, RuleRemediation::ProviderError); }
    #[test] fn corroborated_leak_owns_without_direct_leak() { let leak = detection(DetectorRule::ControlTokenLeak, 12); let collapse = detection(DetectorRule::CollapseRepetition, 12); let result = resolve_detection(None, Some(&collapse), Some(&leak)).unwrap(); assert_eq!(result.owner, DetectionOwner::ControlTokenLeak); assert_eq!(result.detection, leak); }
    #[test] fn corroborated_leak_without_collapse_observes_leak_only() { let leak = detection(DetectorRule::ControlTokenLeak, 12); let result = resolve_detection(None, None, Some(&leak)).unwrap(); assert_eq!(result.observed_rules, [DetectionOwner::ControlTokenLeak]); }
    #[test] fn direct_leak_is_reported_over_corroborated() { let direct = detection(DetectorRule::ControlTokenLeak, 3); let corroborated = detection(DetectorRule::ControlTokenLeak, 40); let result = resolve_detection(Some(&direct), None, Some(&corroborated)).unwrap(); assert_eq!(result.detection.anomaly_start_offset, 3); }
    #[test] fn first_abort_claim_stamps_owner_and_time() { let mut state = create_generation_state(); let result = claim_abort(&mut state, &collapse(), 42); assert!(result); assert_eq!(state.abort_owner, Some(DetectionOwner::CollapseRepetition)); assert_eq!(state.self_abort_at, Some(42)); }
    #[test] fn second_claim_preserves_owner() { let mut state = create_generation_state(); claim_abort(&mut state, &collapse(), 42); let result = claim_abort(&mut state, &leak(), 43); assert!(!result); assert_eq!(state.abort_owner, Some(DetectionOwner::CollapseRepetition)); }
    #[test] fn late_detection_cannot_claim_again() { let mut state = create_generation_state(); claim_abort(&mut state, &leak(), 42); let result = (0..3).map(|_| claim_abort(&mut state, &collapse(), 43)).collect::<Vec<_>>(); assert_eq!(result, [false; 3]); }
    #[test] fn user_cancellation_prevents_abort_claim() { let mut state = create_generation_state(); mark_user_cancelled(&mut state); let result = claim_abort(&mut state, &leak(), 42); assert!(!result); assert_eq!(state.abort_owner, None); }
    #[test] fn cancellation_after_claim_preserves_latch() { let mut state = create_generation_state(); claim_abort(&mut state, &leak(), 42); mark_user_cancelled(&mut state); let result = claim_abort(&mut state, &leak(), 43); assert!(!result); assert_eq!(state.abort_owner, Some(DetectionOwner::ControlTokenLeak)); }
}
