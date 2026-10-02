use std::collections::BTreeSet;
use serde_json::Value;
use senpi_task::{category::{CategoryResolutionResult, ResolveCategoryOptions, resolve_category}, host::{HostError, SenpiModelRegistry}};
use super::{memory_model_attempts::ReflectionModelCandidate, model_cost::{LaunchChoice, ReflectionLaunchCandidate, ReflectionLaunchInput, choose_reflection_launch_model}, registry_fallback::{read_model_pricing, select_registry_fallback_models}};

pub struct ReflectionSessionModel { pub provider: String, pub id: String, pub thinking: Option<String> }
#[derive(Debug)]
pub enum ReflectionModelResolution {
    Resolved { category: String, model: String, thinking: Option<String>, source: Option<&'static str>, fallbacks: Vec<ReflectionModelCandidate> },
    CategoryUnavailable { category: String, cause: &'static str, attempted_chain: Option<Vec<senpi_task::DelegateFallbackEntry>>, missing_providers: Option<Vec<String>> },
}

fn normalize_thinking(value: Option<&str>) -> Option<String> {
    let value = match value? { "none" => "off", other => other };
    ["off", "minimal", "low", "medium", "high", "xhigh", "max"].contains(&value).then(|| value.into())
}

fn beyond_category(category: &str, registry: Option<&dyn SenpiModelRegistry>, session: Option<&ReflectionSessionModel>) -> Result<Option<ReflectionModelResolution>, HostError> {
    let candidates = match registry { Some(registry) => select_registry_fallback_models(&registry.get_available()?), None => vec![] };
    let fresh = candidates.first().map(|candidate| ReflectionLaunchCandidate { model: candidate.model.clone(), thinking: None, cost: candidate.cost });
    let inherited = session.map(|session| ReflectionLaunchCandidate { model: format!("{}/{}", session.provider, session.id), thinking: session.thinking.clone(), cost: registry.and_then(|registry| registry.find(&session.provider, &session.id)).as_ref().and_then(read_model_pricing) });
    if fresh.is_none() && inherited.is_none() { return Ok(None); }
    let decision = choose_reflection_launch_model(&ReflectionLaunchInput { fresh: fresh.as_ref(), session: inherited.as_ref(), prefix_tokens: 0.0, workload_tokens: 50_000.0, cache_reusable: false }).map_err(|error| HostError { message: error.to_string() })?;
    let inherit = decision.choice == LaunchChoice::Inherit;
    Ok(Some(ReflectionModelResolution::Resolved { category: category.into(), model: decision.model, thinking: normalize_thinking(decision.thinking.as_deref()), source: Some(if inherit { "session_inherit" } else { "registry_fallback" }), fallbacks: if inherit { vec![] } else { candidates.into_iter().skip(1).map(|candidate| ReflectionModelCandidate { model: candidate.model, thinking: None }).collect() } }))
}

pub fn resolve_reflection_model(category: &str, config: &Value, registry: Option<&dyn SenpiModelRegistry>, session: Option<&ReflectionSessionModel>) -> Result<ReflectionModelResolution, HostError> {
    let unavailable = |cause, attempted_chain, missing_providers| ReflectionModelResolution::CategoryUnavailable { category: category.into(), cause, attempted_chain, missing_providers };
    let Some(registry) = registry else { return Ok(beyond_category(category, None, session)?.unwrap_or_else(|| unavailable("no_registry", None, None))); };
    let resolution = resolve_category(category, config, registry, &ResolveCategoryOptions::default())?;
    if let CategoryResolutionResult::Resolved { category: resolved_category, spec, .. } = resolution {
        let thinking = normalize_thinking(spec.reasoning.as_deref().or(spec.reasoning_effort.as_deref()).or(spec.variant.as_deref()));
        let selected = format!("{}/{}", spec.provider, spec.model_id);
        let mut fallbacks: Vec<_> = spec.fallback_models.unwrap_or_default().into_iter().map(|fallback| ReflectionModelCandidate { model: format!("{}/{}", fallback.provider, fallback.model_id), thinking: normalize_thinking(fallback.reasoning_effort.as_deref().or(fallback.variant.as_deref())) }).collect();
        let category_config = &config["categories"][category];
        let canonical = category_config["models"].as_array().filter(|entries| !entries.is_empty());
        let legacy = category_config.get("fallback_models");
        let configured = if let Some(canonical) = canonical { canonical.clone() } else if let Some(legacy) = legacy { let mut entries = vec![Value::String(selected.clone())]; if let Some(array) = legacy.as_array() { entries.extend(array.iter().cloned()); } else { entries.push(legacy.clone()); } entries } else { vec![] };
        let selector = |entry: &Value| entry.as_str().or_else(|| entry.get("model").and_then(Value::as_str)).map(str::to_owned);
        if let Some(index) = configured.iter().position(|entry| selector(entry).as_deref() == Some(&selected)) {
            for entry in &configured[index + 1..] {
                let Some(model) = selector(entry) else { continue; };
                let Some((provider, id)) = model.split_once('/').filter(|(provider, id)| !provider.is_empty() && !id.is_empty()) else { continue; };
                if registry.find(provider, id).is_none() { continue; }
                let thinking = normalize_thinking(entry.get("reasoning").or_else(|| entry.get("reasoningEffort")).or_else(|| entry.get("variant")).and_then(Value::as_str));
                fallbacks.push(ReflectionModelCandidate { model, thinking });
            }
        }
        let mut seen = BTreeSet::new();
        fallbacks.retain(|candidate| seen.insert(candidate.model.clone()));
        return Ok(ReflectionModelResolution::Resolved { category: resolved_category, model: selected, thinking, source: None, fallbacks });
    }
    if matches!(resolution, CategoryResolutionResult::ModelUnavailable(_))
        && let Some(pinned) = config["categories"][category]["model"].as_str().filter(|model| model.contains('/'))
    {
        let mut parts = pinned.split('/');
        if let (Some(provider), Some(id)) = (parts.next(), parts.next())
            && registry.find(provider, id).is_some()
        { return Ok(ReflectionModelResolution::Resolved { category: category.into(), model: pinned.into(), thinking: None, source: None, fallbacks: vec![] }); }
    }
    if !matches!(resolution, CategoryResolutionResult::Disabled { .. })
        && let Some(fallback) = beyond_category(category, Some(registry), session)?
    { return Ok(fallback); }
    Ok(match resolution {
        CategoryResolutionResult::NotFound { .. } => unavailable("not_found", None, None),
        CategoryResolutionResult::ModelUnavailable(details) => unavailable("model_unavailable", details.attempted_chain, details.missing_providers),
        CategoryResolutionResult::Disabled { .. } | CategoryResolutionResult::Resolved { .. } => unavailable("unknown", None, None),
    })
}

pub fn should_warn_category_unavailable(config: &Value, category: &str) -> bool {
    let category_config = &config["categories"][category];
    if category_config.get("model").is_some() { return false; }
    category_config.get("warn_unavailable").filter(|value| !value.is_null()).or_else(|| config.get("task")?.get("warnings")?.get("unavailable_categories")).and_then(Value::as_bool) != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Registry { available: Value, catalog: Vec<Value> }
    impl SenpiModelRegistry for Registry {
        fn get_available(&self) -> Result<Value, HostError> { Ok(self.available.clone()) }
        fn find(&self, provider: &str, id: &str) -> Option<Value> { self.catalog.iter().find(|model| model["provider"] == provider && model["id"] == id).cloned() }
    }
    fn registry(available: bool) -> Registry { let model = json!({"provider":"omo-mock","id":"mock-1"}); Registry { available: if available { json!([model]) } else { json!([]) }, catalog: vec![model] } }
    #[test] fn category_preserves_model_and_thinking() { let result = resolve_reflection_model("quick", &json!({"categories":{"quick":{"model":"omo-mock/mock-1","reasoning":"high"}}}), Some(&registry(true)), None).unwrap(); assert!(matches!(result, ReflectionModelResolution::Resolved { model, thinking: Some(thinking), fallbacks, .. } if model == "omo-mock/mock-1" && thinking == "high" && fallbacks.is_empty())); }
    #[test] fn stale_pinned_catalog_wins() { let result = resolve_reflection_model("quick", &json!({"categories":{"quick":{"model":"omo-mock/mock-1"}}}), Some(&registry(false)), None).unwrap(); assert!(matches!(result, ReflectionModelResolution::Resolved { model, .. } if model == "omo-mock/mock-1")); }
    #[test] fn missing_pinned_model_fails_closed() { let result = resolve_reflection_model("quick", &json!({"categories":{"quick":{"model":"omo-mock/ghost"}}}), Some(&Registry { available: json!([]), catalog: vec![] }), None).unwrap(); assert!(matches!(result, ReflectionModelResolution::CategoryUnavailable { .. })); }
    #[test] fn no_registry_uses_session() { let session = ReflectionSessionModel { provider: "live".into(), id: "model".into(), thinking: Some("none".into()) }; let result = resolve_reflection_model("quick", &json!({}), None, Some(&session)).unwrap(); assert!(matches!(result, ReflectionModelResolution::Resolved { source: Some("session_inherit"), thinking: Some(thinking), .. } if thinking == "off")); }
    #[test] fn disabled_category_never_inherits() { let session = ReflectionSessionModel { provider: "live".into(), id: "model".into(), thinking: None }; let result = resolve_reflection_model("quick", &json!({"categories":{"quick":{"disable":true}}}), Some(&registry(true)), Some(&session)).unwrap(); assert!(matches!(result, ReflectionModelResolution::CategoryUnavailable { cause: "unknown", .. })); }
    #[test] fn no_registry_or_session_fails_closed() { assert!(matches!(resolve_reflection_model("quick", &json!({}), None, None).unwrap(), ReflectionModelResolution::CategoryUnavailable { cause: "no_registry", .. })); }
    #[test] fn warning_override_precedence() { assert!(should_warn_category_unavailable(&json!({}), "quick")); assert!(!should_warn_category_unavailable(&json!({"categories":{"quick":{"model":"p/m","warn_unavailable":true}}}), "quick")); assert!(!should_warn_category_unavailable(&json!({"task":{"warnings":{"unavailable_categories":false}}}), "quick")); assert!(should_warn_category_unavailable(&json!({"categories":{"quick":{"warn_unavailable":true}},"task":{"warnings":{"unavailable_categories":false}}}), "quick")); }
    #[test] fn thinking_values_are_normalized() { for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] { assert_eq!(normalize_thinking(Some(level)).as_deref(), Some(level)); } assert_eq!(normalize_thinking(Some("none")).as_deref(), Some("off")); for value in [None, Some("auto"), Some("bogus")] { assert!(normalize_thinking(value).is_none()); } }
    fn priced_registry() -> Registry {
        let catalog = vec![json!({"provider":"google","id":"gemini-3.1-pro","cost":{"input":2,"cacheRead":0.2},"contextWindow":1000000}), json!({"provider":"google","id":"gemini-3.6-flash","cost":{"input":0.3,"cacheRead":0.03},"contextWindow":1000000}), json!({"provider":"google","id":"text-embedding-005","cost":{"input":0.01},"contextWindow":8192})];
        Registry { available: Value::Array(catalog.clone()), catalog }
    }
    #[test] fn registry_ladder_chooses_cheapest_chat() { let result = resolve_reflection_model("quick", &json!({}), Some(&priced_registry()), None).unwrap(); assert!(matches!(result, ReflectionModelResolution::Resolved { model, source: Some("registry_fallback"), fallbacks, .. } if model == "google/gemini-3.6-flash" && fallbacks.len() == 1 && fallbacks[0].model == "google/gemini-3.1-pro")); }
    #[test] fn expensive_session_does_not_override_registry() { let session = ReflectionSessionModel { provider: "google".into(), id: "gemini-3.1-pro".into(), thinking: None }; assert!(matches!(resolve_reflection_model("quick", &json!({}), Some(&priced_registry()), Some(&session)).unwrap(), ReflectionModelResolution::Resolved { source: Some("registry_fallback"), .. })); }
    #[test] fn cheaper_session_wins_without_cache_reuse() { let mut registry = priced_registry(); registry.available = json!([registry.catalog[0]]); let session = ReflectionSessionModel { provider: "google".into(), id: "gemini-3.6-flash".into(), thinking: None }; assert!(matches!(resolve_reflection_model("quick", &json!({}), Some(&registry), Some(&session)).unwrap(), ReflectionModelResolution::Resolved { source: Some("session_inherit"), fallbacks, .. } if fallbacks.is_empty())); }
    #[test] fn empty_registry_inherits_live_session() { let session = ReflectionSessionModel { provider: "live".into(), id: "model".into(), thinking: Some("low".into()) }; assert!(matches!(resolve_reflection_model("quick", &json!({}), Some(&Registry { available: json!([]), catalog: vec![] }), Some(&session)).unwrap(), ReflectionModelResolution::Resolved { source: Some("session_inherit"), thinking: Some(thinking), .. } if thinking == "low")); }
    #[test] fn unavailable_pin_can_use_registry_ladder() { assert!(matches!(resolve_reflection_model("quick", &json!({"categories":{"quick":{"model":"google/ghost"}}}), Some(&priced_registry()), None).unwrap(), ReflectionModelResolution::Resolved { source: Some("registry_fallback"), .. })); }
    #[test] fn empty_registry_reports_attempted_chain() { assert!(matches!(resolve_reflection_model("quick", &json!({}), Some(&Registry { available: json!([]), catalog: vec![] }), None).unwrap(), ReflectionModelResolution::CategoryUnavailable { attempted_chain: Some(chain), .. } if !chain.is_empty())); }
    #[test] fn canonical_stale_chain_preserves_fallbacks() { let catalog = vec![json!({"provider":"extension-only","id":"primary"}), json!({"provider":"omo-mock","id":"mock-1"})]; let registry = Registry { available: json!([]), catalog }; let result = resolve_reflection_model("quick", &json!({"categories":{"quick":{"models":[{"model":"extension-only/primary","reasoning":"off"},{"model":"omo-mock/mock-1","reasoning":"minimal"}]}}}), Some(&registry), None).unwrap(); assert!(matches!(result, ReflectionModelResolution::Resolved { model, thinking: Some(thinking), fallbacks, .. } if model == "extension-only/primary" && thinking == "off" && fallbacks.len() == 1 && fallbacks[0].thinking.as_deref() == Some("minimal"))); }
    #[test] fn legacy_stale_chain_preserves_fallbacks() { let catalog = vec![json!({"provider":"extension-only","id":"primary"}), json!({"provider":"omo-mock","id":"mock-1"})]; let registry = Registry { available: json!([]), catalog }; let result = resolve_reflection_model("quick", &json!({"categories":{"quick":{"model":"extension-only/primary","reasoning":"off","fallback_models":[{"model":"omo-mock/mock-1","reasoning":"minimal"}]}}}), Some(&registry), None).unwrap(); assert!(matches!(result, ReflectionModelResolution::Resolved { fallbacks, .. } if fallbacks.len() == 1 && fallbacks[0].thinking.as_deref() == Some("minimal"))); }
}
