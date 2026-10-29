//! Fresh versus inherited reflection workload pricing.
#[derive(Clone, Copy, Debug)]
pub struct ReflectionModelPricing {
    pub input: f64,
    pub cache_read: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ReflectionLaunchCandidate {
    pub model: String,
    pub thinking: Option<String>,
    pub cost: Option<ReflectionModelPricing>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchChoice { Fresh, Inherit }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchReason { Cheaper, OnlyCandidate, NoPricing }

#[derive(Clone, Debug)]
pub struct ReflectionLaunchChoice {
    pub choice: LaunchChoice,
    pub model: String,
    pub thinking: Option<String>,
    pub reason: LaunchReason,
    pub fresh_cost: Option<f64>,
    pub inherit_cost: Option<f64>,
}

pub struct ReflectionLaunchInput<'a> {
    pub fresh: Option<&'a ReflectionLaunchCandidate>,
    pub session: Option<&'a ReflectionLaunchCandidate>,
    pub prefix_tokens: f64,
    pub workload_tokens: f64,
    pub cache_reusable: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MissingLaunchCandidate;
impl std::fmt::Display for MissingLaunchCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("chooseReflectionLaunchModel requires at least one candidate")
    }
}
impl std::error::Error for MissingLaunchCandidate {}

pub fn estimate_fresh_cost(candidate: &ReflectionLaunchCandidate, workload_tokens: f64) -> Option<f64> {
    Some(workload_tokens * candidate.cost?.input)
}

pub fn estimate_inherit_cost(candidate: &ReflectionLaunchCandidate, prefix_tokens: f64, workload_tokens: f64, cache_reusable: bool) -> Option<f64> {
    let cost = candidate.cost?;
    if !cache_reusable { return Some(workload_tokens * cost.input); }
    let cached_tokens = prefix_tokens.min(workload_tokens);
    Some(cached_tokens * cost.cache_read.unwrap_or(cost.input) + (workload_tokens - cached_tokens).max(0.0) * cost.input)
}

pub fn choose_reflection_launch_model(input: &ReflectionLaunchInput<'_>) -> Result<ReflectionLaunchChoice, MissingLaunchCandidate> {
    let pick = |choice, candidate: &ReflectionLaunchCandidate, reason| ReflectionLaunchChoice {
        choice, model: candidate.model.clone(), thinking: candidate.thinking.clone(), reason, fresh_cost: None, inherit_cost: None,
    };
    match (input.fresh, input.session) {
        (None, None) => Err(MissingLaunchCandidate),
        (Some(fresh), None) => Ok(pick(LaunchChoice::Fresh, fresh, LaunchReason::OnlyCandidate)),
        (None, Some(session)) => Ok(pick(LaunchChoice::Inherit, session, LaunchReason::OnlyCandidate)),
        (Some(fresh), Some(session)) => {
            let fresh_cost = estimate_fresh_cost(fresh, input.workload_tokens);
            let inherit_cost = estimate_inherit_cost(session, input.prefix_tokens, input.workload_tokens, input.cache_reusable);
            let (Some(f), Some(i)) = (fresh_cost, inherit_cost) else { return Ok(pick(LaunchChoice::Fresh, fresh, LaunchReason::NoPricing)); };
            let mut result = if i < f { pick(LaunchChoice::Inherit, session, LaunchReason::Cheaper) } else { pick(LaunchChoice::Fresh, fresh, LaunchReason::Cheaper) };
            result.fresh_cost = fresh_cost;
            result.inherit_cost = inherit_cost;
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(input: f64, cache_read: f64) -> ReflectionLaunchCandidate { ReflectionLaunchCandidate { model: "fixture/model".into(), thinking: None, cost: Some(ReflectionModelPricing { input, cache_read: Some(cache_read) }) } }
    fn choose(fresh: Option<&ReflectionLaunchCandidate>, session: Option<&ReflectionLaunchCandidate>, cache_reusable: bool) -> ReflectionLaunchChoice { choose_reflection_launch_model(&ReflectionLaunchInput { fresh, session, prefix_tokens: 190_000.0, workload_tokens: 200_000.0, cache_reusable }).unwrap() }
    #[test]
    fn cheaper_fresh_without_cache() { assert_eq!(choose(Some(&candidate(0.3, 0.03)), Some(&candidate(5.0, 0.5)), false).choice, LaunchChoice::Fresh); }
    #[test]
    fn cheaper_session_without_cache() { assert_eq!(choose(Some(&candidate(1.75, 0.175)), Some(&candidate(0.8, 0.08)), false).choice, LaunchChoice::Inherit); }
    #[test]
    fn cached_prefix_wins() { assert_eq!(choose(Some(&candidate(0.3, 0.03)), Some(&candidate(5.0, 0.01)), true).choice, LaunchChoice::Inherit); }
    #[test]
    fn uncached_prefix_loses() { assert_eq!(choose(Some(&candidate(0.3, 0.03)), Some(&candidate(5.0, 0.01)), false).choice, LaunchChoice::Fresh); }
    #[test]
    fn missing_price_is_deterministic() { let mut fresh = candidate(0.3, 0.03); fresh.cost = None; assert_eq!(choose(Some(&fresh), Some(&candidate(0.8, 0.08)), false).reason, LaunchReason::NoPricing); }
    #[test]
    fn session_only_inherits() { assert_eq!(choose(None, Some(&candidate(0.8, 0.08)), false).choice, LaunchChoice::Inherit); }
    #[test]
    fn missing_candidates_error() { assert!(choose_reflection_launch_model(&ReflectionLaunchInput { fresh: None, session: None, prefix_tokens: 0.0, workload_tokens: 0.0, cache_reusable: false }).is_err()); }
}
