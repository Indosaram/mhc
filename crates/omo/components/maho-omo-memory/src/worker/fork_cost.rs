//! Per-turn fork cache billing and quick worker route selection.
#[derive(Clone, Copy, Debug)]
pub struct Pricing { pub input: f64, pub cache_read: Option<f64>, pub output: Option<f64> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryLaunchSurface { Reflection, Dream, Facts }
#[derive(Clone, Copy, Debug)]
pub struct MemoryWorkloadProfile { pub input_tokens: f64, pub cache_read_tokens: f64, pub output_tokens: f64, pub turns: u32 }
pub const REFLECTION_PROFILE: MemoryWorkloadProfile = MemoryWorkloadProfile { input_tokens: 44_502.0, cache_read_tokens: 643_584.0, output_tokens: 6_996.0, turns: 21 };
pub const FACTS_PROFILE: MemoryWorkloadProfile = MemoryWorkloadProfile { input_tokens: 9_742.0, cache_read_tokens: 9_583.0, output_tokens: 184.0, turns: 2 };
impl MemoryLaunchSurface {
    pub const fn profile(self) -> MemoryWorkloadProfile { match self { Self::Reflection | Self::Dream => REFLECTION_PROFILE, Self::Facts => FACTS_PROFILE } }
}
pub struct ForkCostInput { pub pricing: Pricing, pub parent_context_tokens: f64, pub turns: u32, pub output_tokens: f64, pub cache_hit: bool }
pub fn estimate_fork_cost(input: &ForkCostInput) -> f64 {
    let pricing = input.pricing;
    let cache_read = pricing.cache_read.unwrap_or(pricing.input);
    let mut total = input.parent_context_tokens * if input.cache_hit { cache_read } else { pricing.input } + 3_800.0 * pricing.input;
    for turn in 1..input.turns { total += (input.parent_context_tokens + 3_800.0 + 500.0 * f64::from(turn)) * cache_read; }
    (total + input.output_tokens * pricing.output.unwrap_or(0.0)) / 1_000_000.0
}
pub fn estimate_quick_cost(pricing: Pricing, profile: MemoryWorkloadProfile) -> f64 {
    (profile.input_tokens * pricing.input + profile.cache_read_tokens * pricing.cache_read.unwrap_or(pricing.input) + profile.output_tokens * pricing.output.unwrap_or(0.0)) / 1_000_000.0
}
#[derive(Clone, Debug)]
pub struct MemoryRouteCandidate { pub model: String, pub thinking: Option<String>, pub cost: Option<Pricing> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route { Fork, Quick }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason { Cheaper, OnlyCandidate, NoPricing, SurfaceExcluded, UnknownContext }
#[derive(Clone, Debug)]
pub struct MemoryLaunchRoute { pub route: Route, pub model: String, pub thinking: Option<String>, pub reason: Reason, pub fork_cost: Option<f64>, pub quick_cost: Option<f64> }
pub struct MemoryLaunchRouteInput<'a> { pub surface: MemoryLaunchSurface, pub quick: Option<&'a MemoryRouteCandidate>, pub session: Option<&'a MemoryRouteCandidate>, pub parent_context_tokens: Option<f64>, pub turns: u32, pub cache_hit: bool }
#[derive(Debug)]
pub struct MissingQuickCandidate;
impl std::fmt::Display for MissingQuickCandidate { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("chooseMemoryLaunchRoute requires a quick candidate") } }
impl std::error::Error for MissingQuickCandidate {}
pub fn choose_memory_launch_route(input: &MemoryLaunchRouteInput<'_>) -> Result<MemoryLaunchRoute, MissingQuickCandidate> {
    let pick = |route, candidate: &MemoryRouteCandidate, reason| MemoryLaunchRoute { route, model: candidate.model.clone(), thinking: candidate.thinking.clone(), reason, fork_cost: None, quick_cost: None };
    let Some(quick) = input.quick else { return input.session.map(|session| pick(Route::Fork, session, Reason::OnlyCandidate)).ok_or(MissingQuickCandidate); };
    if input.surface == MemoryLaunchSurface::Facts { return Ok(pick(Route::Quick, quick, Reason::SurfaceExcluded)); }
    let Some(session) = input.session else { return Ok(pick(Route::Quick, quick, Reason::OnlyCandidate)); };
    let Some(pricing) = session.cost else { return Ok(pick(Route::Quick, quick, Reason::NoPricing)); };
    let Some(parent_context_tokens) = input.parent_context_tokens else { return Ok(pick(Route::Quick, quick, Reason::UnknownContext)); };
    let profile = input.surface.profile();
    let fork_cost = estimate_fork_cost(&ForkCostInput { pricing, parent_context_tokens, turns: input.turns, output_tokens: profile.output_tokens, cache_hit: input.cache_hit });
    let Some(quick_pricing) = quick.cost else { return Ok(pick(Route::Quick, quick, Reason::NoPricing)); };
    let quick_cost = estimate_quick_cost(quick_pricing, profile);
    let mut result = if fork_cost < quick_cost { pick(Route::Fork, session, Reason::Cheaper) } else { pick(Route::Quick, quick, Reason::Cheaper) };
    result.fork_cost = Some(fork_cost);
    result.quick_cost = Some(quick_cost);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const KIMI: Pricing = Pricing { input: 0.6, cache_read: Some(0.15), output: Some(2.5) };
    const LUNA: Pricing = Pricing { input: 0.25, cache_read: Some(0.025), output: Some(2.0) };
    const OPUS: Pricing = Pricing { input: 5.0, cache_read: Some(0.5), output: Some(25.0) };
    fn fork(parent: f64, turns: u32, cache_hit: bool) -> f64 { estimate_fork_cost(&ForkCostInput { pricing: KIMI, parent_context_tokens: parent, turns, output_tokens: 6996.0, cache_hit }) }
    fn candidate(cost: Option<Pricing>) -> MemoryRouteCandidate { MemoryRouteCandidate { model: "fixture/model".into(), thinking: None, cost } }
    fn route(surface: MemoryLaunchSurface, session: Option<&MemoryRouteCandidate>, parent_context_tokens: Option<f64>, turns: u32) -> MemoryLaunchRoute { choose_memory_launch_route(&MemoryLaunchRouteInput { surface, quick: Some(&candidate(Some(KIMI))), session, parent_context_tokens, turns, cache_hit: true }).unwrap() }
    #[test]
    fn per_turn_cache_billing() { let one = fork(156_872.0, 1, true); let five = fork(156_872.0, 5, true); let twenty_one = fork(156_872.0, 21, true); assert!((one / 0.26042 - 0.17).abs() < 0.05); assert!((five / 0.26042 - 0.54).abs() < 0.05); assert!((twenty_one / 0.26042 - 2.08).abs() < 0.05); assert!(twenty_one > five * 3.0); }
    #[test]
    fn cache_miss_costs_more() { assert!(fork(156_872.0, 21, false) > fork(156_872.0, 21, true)); }
    #[test]
    fn larger_context_costs_more() { assert!(fork(360_463.0, 21, true) > fork(156_872.0, 21, true) * 1.5); }
    #[test]
    fn short_cheap_session_forks() { let choice = route(MemoryLaunchSurface::Reflection, Some(&candidate(Some(LUNA))), Some(156_872.0), 3); assert_eq!(choice.route, Route::Fork); assert!(choice.fork_cost.unwrap() < choice.quick_cost.unwrap()); }
    #[test]
    fn expensive_session_uses_quick() { assert_eq!(route(MemoryLaunchSurface::Reflection, Some(&candidate(Some(OPUS))), Some(156_872.0), 1).route, Route::Quick); }
    #[test]
    fn long_job_uses_quick() { assert_eq!(route(MemoryLaunchSurface::Reflection, Some(&candidate(Some(KIMI))), Some(360_463.0), 21).route, Route::Quick); }
    #[test]
    fn facts_excludes_fork() { let choice = route(MemoryLaunchSurface::Facts, Some(&candidate(Some(LUNA))), Some(1000.0), 1); assert_eq!(choice.route, Route::Quick); assert_eq!(choice.reason, Reason::SurfaceExcluded); }
    #[test]
    fn unpriced_session_uses_quick() { assert_eq!(route(MemoryLaunchSurface::Reflection, Some(&candidate(None)), Some(156_872.0), 3).reason, Reason::NoPricing); }
    #[test]
    fn no_session_uses_quick() { assert_eq!(route(MemoryLaunchSurface::Reflection, None, Some(156_872.0), 3).route, Route::Quick); }
    #[test]
    fn unknown_context_uses_quick() { assert_eq!(route(MemoryLaunchSurface::Reflection, Some(&candidate(Some(LUNA))), None, 3).reason, Reason::UnknownContext); }
    #[test]
    fn measured_profile_matches_range() { let cost = estimate_quick_cost(KIMI, REFLECTION_PROFILE); assert!(cost > 0.26042 * 0.5); assert!(cost < 0.26042 * 2.0); }
    #[test]
    fn facts_are_cheaper() { assert!(estimate_quick_cost(KIMI, FACTS_PROFILE) < estimate_quick_cost(KIMI, REFLECTION_PROFILE) / 5.0); }
}
