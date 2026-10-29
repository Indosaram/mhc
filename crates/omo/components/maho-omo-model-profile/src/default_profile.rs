use std::collections::BTreeSet;
use std::future::Future;

use crate::builtin_profiles::DEFAULT_MODEL_PROFILE_ORDER;
use crate::request_auth::AuthFailure;
use crate::resolve::ModelProfileResolution;

#[derive(Default)]
pub struct ProbeState {
    pub excluded_providers: BTreeSet<String>,
    pub excluded_selectors: BTreeSet<String>,
    pub failures: Vec<AuthFailure>,
}

pub fn new_probe_state() -> ProbeState {
    ProbeState::default()
}

pub async fn resolve_default_model_profile<T, F: Future<Output = (T, ProbeState)>>(
    mut probes: ProbeState,
    mut resolve_for: impl FnMut(&'static str, ProbeState) -> F,
    resolution: impl Fn(&T) -> &ModelProfileResolution,
) -> (T, ProbeState) {
    let [first, rest @ ..] = DEFAULT_MODEL_PROFILE_ORDER;
    let (primary, state) = resolve_for(first, probes).await;
    probes = state;
    if matches!(resolution(&primary), ModelProfileResolution::Resolved { .. }) {
        return (primary, probes);
    }
    for profile_id in rest {
        let (candidate, state) = resolve_for(profile_id, probes).await;
        probes = state;
        if matches!(resolution(&candidate), ModelProfileResolution::Resolved { .. }) {
            return (candidate, probes);
        }
    }
    (primary, probes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{ResolveModelProfileInput, resolve_model_profile};

    #[tokio::test]
    async fn defaults_choose_the_first_resolving_lane_or_report_the_primary() {
        for (models, expected, attempted) in [
            (vec!["anthropic/claude-opus-5-5"], "daily-normal", 1),
            (vec!["anthropic/claude-fable-5-1"], "daily-heavy", 2),
            (vec!["chatgpt-subscription/gpt-6.1-sol"], "geeky-normal", 3),
            (vec!["chatgpt-subscription/gpt-6-astra"], "geeky-normal", 3),
            (vec![], "daily-normal", 4),
        ] {
            let available: Vec<String> = models.into_iter().map(str::to_owned).collect();
            let available = &available;
            let (result, probes) = resolve_default_model_profile(
                new_probe_state(),
                |active, mut probes| async move {
                    probes.excluded_selectors.insert(active.to_owned());
                    let resolution = resolve_model_profile(&ResolveModelProfileInput {
                        profiles: None, active, available_models: available, custom_providers: None,
                    });
                    (resolution, probes)
                },
                |resolution| resolution,
            ).await;
            let id = match &result {
                ModelProfileResolution::Resolved { profile, .. }
                | ModelProfileResolution::Unavailable { profile, .. }
                | ModelProfileResolution::Empty { profile } => profile.id.as_str(),
                ModelProfileResolution::Unknown { name, .. } => name,
            };
            assert_eq!(id, expected);
            assert_eq!(probes.excluded_selectors.len(), attempted);
        }

        // When geeky-normal fails to resolve, the walk falls through to geeky-heavy.
        let available = vec!["chatgpt-subscription/gpt-6-astra".to_owned()];
        let profiles = serde_json::json!({
            "geeky-normal": { "models": ["openai/nonexistent"] }
        });
        let (result, probes) = resolve_default_model_profile(
            new_probe_state(),
            |active, mut probes| {
                let profiles = &profiles;
                let available = &available;
                async move {
                    probes.excluded_selectors.insert(active.to_owned());
                    let resolution = resolve_model_profile(&ResolveModelProfileInput {
                        profiles: Some(profiles), active, available_models: available, custom_providers: None,
                    });
                    (resolution, probes)
                }
            },
            |resolution| resolution,
        ).await;
        let id = match &result {
            ModelProfileResolution::Resolved { profile, .. } => profile.id.as_str(),
            _ => panic!("expected resolved"),
        };
        assert_eq!(id, "geeky-heavy");
        assert_eq!(probes.excluded_selectors.len(), 4);
    }
}
