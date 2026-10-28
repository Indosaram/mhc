//! Port of omo-senpi `components/model-profile/describe.ts` at omo `v5.1.47`: the diagnostic view of
//! the lane the main session would start on.
//!
//! This is the session-start walk minus its credential probe: the configured `model_profile` (a
//! retired id counts as unset), or the first default lane that resolves, with `exclude_models`
//! applied the way the session-start component applies it. It is a pure projection over the merged
//! profile table and [`crate::resolve::resolve_model_profile`], so it performs no credential probing
//! and loads no custom providers - a rung that would only resolve through a `models.json`
//! declaration reads `no-login` here, which is exactly what the caller can then explain.

use maho_ext_api::JsonValue;
use omo_config_core::excluded_model_pattern;

use crate::builtin_profiles::{DEFAULT_MODEL_PROFILE_ORDER, RETIRED_MODEL_PROFILE_IDS};
use crate::resolve::{
    ModelProfileResolution, ModelProfileRung, ModelProfileSource, ResolveModelProfileInput,
    merge_model_profiles, resolve_model_profile,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProfileRungSource {
    BuiltIn,
    OmoJson,
    Pin,
}

impl ModelProfileRungSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::OmoJson => "omo.json",
            Self::Pin => "pin",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProfileRungState {
    Applied,
    Excluded,
    NoLogin,
    NotReached,
}

impl ModelProfileRungState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Excluded => "excluded",
            Self::NoLogin => "no-login",
            Self::NotReached => "not-reached",
        }
    }
}

/// Upstream `selection`: `set` when omo.json names the profile, `default` when the unset walk picked
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProfileSelection {
    Set,
    Default,
}

impl ModelProfileSelection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Default => "default",
        }
    }
}

/// Upstream `ModelProfileRungView`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProfileRungView {
    pub model: String,
    pub source: ModelProfileRungSource,
    pub state: ModelProfileRungState,
    pub excluded_by: Option<String>,
}

/// Upstream `ModelProfileView`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProfileView {
    /// The lane id, or the literal `provider/model` pin.
    pub profile: String,
    pub selection: ModelProfileSelection,
    pub applied: Option<String>,
    pub rungs: Vec<ModelProfileRungView>,
    pub reason: Option<String>,
}

fn rung_label(rung: &ModelProfileRung) -> String {
    match rung.providers.first() {
        Some(provider) => format!("{provider}/{}", rung.model),
        None => rung.model.clone(),
    }
}

fn user_profiles(config: &JsonValue) -> Option<&JsonValue> {
    config.get("model_profiles")
}

fn exclude_models(config: &JsonValue) -> Option<Vec<String>> {
    let JsonValue::Array(entries) = config.get("exclude_models")? else {
        return None;
    };
    Some(entries.iter().filter_map(JsonValue::as_str).map(str::to_owned).collect())
}

/// Upstream `rungsOf`: the labels of one profile's chain and where they came from. A pin is a
/// one-rung chain of itself, an unknown id has no rungs.
fn rungs_of(profile: &str, config: &JsonValue) -> Vec<(String, ModelProfileRungSource)> {
    if profile.contains('/') {
        return vec![(profile.to_owned(), ModelProfileRungSource::Pin)];
    }
    let merged = merge_model_profiles(user_profiles(config));
    let Some(definition) = merged.get(profile) else {
        return Vec::new();
    };
    let source = if definition.profile.source == ModelProfileSource::User {
        ModelProfileRungSource::OmoJson
    } else {
        ModelProfileRungSource::BuiltIn
    };
    definition.models.iter().map(|rung| (rung_label(rung), source)).collect()
}

/// Upstream `view`: the resolved selector, then every rung's state - applied, excluded by an
/// `exclude_models` entry, not reached because an earlier rung already applied, or no login.
fn view(
    profile: &str,
    selection: ModelProfileSelection,
    resolution: &ModelProfileResolution,
    config: &JsonValue,
) -> ModelProfileView {
    let (applied, applied_index): (Option<String>, i64) = match resolution {
        ModelProfileResolution::Resolved { provider, model_id, skipped, .. } => {
            (Some(format!("{provider}/{model_id}")), skipped.len() as i64)
        }
        _ => (None, -1),
    };
    let patterns = exclude_models(config);
    let rungs = rungs_of(profile, config)
        .into_iter()
        .enumerate()
        .map(|(index, (label, source))| {
            let index = index as i64;
            if index == applied_index {
                return ModelProfileRungView {
                    model: applied.clone().unwrap_or(label),
                    source,
                    state: ModelProfileRungState::Applied,
                    excluded_by: None,
                };
            }
            if let Some(excluded_by) = excluded_model_pattern(&label, patterns.as_deref()) {
                return ModelProfileRungView {
                    model: label,
                    source,
                    state: ModelProfileRungState::Excluded,
                    excluded_by: Some(excluded_by),
                };
            }
            let state = if applied_index != -1 && index > applied_index {
                ModelProfileRungState::NotReached
            } else {
                ModelProfileRungState::NoLogin
            };
            ModelProfileRungView { model: label, source, state, excluded_by: None }
        })
        .collect();
    let reason = match resolution {
        ModelProfileResolution::Unknown { message, .. } => Some(message.clone()),
        ModelProfileResolution::Empty { .. } => Some("the profile lists no models".to_owned()),
        _ => None,
    };
    ModelProfileView { profile: profile.to_owned(), selection, applied, rungs, reason }
}

/// The lane the main session would start on, by the session-start walk minus its credential probe:
/// the configured `model_profile` (a retired id counts as unset), or the first default lane that
/// resolves. `exclude_models` is applied the way the session-start component applies it.
pub fn describe_model_profile(config: &JsonValue, available_models: &[String]) -> ModelProfileView {
    let patterns = exclude_models(config);
    let available: Vec<String> = available_models
        .iter()
        .filter(|selector| excluded_model_pattern(selector.as_str(), patterns.as_deref()).is_none())
        .cloned()
        .collect();
    let configured = config
        .get("model_profile")
        .and_then(JsonValue::as_str)
        .map(str::trim);
    let named = configured.filter(|value| !value.is_empty());
    // A retired builtin id starts the unset default; a user's own entry of that name still applies.
    let explicit = named.filter(|name| {
        !(RETIRED_MODEL_PROFILE_IDS.contains(name)
            && user_profiles(config).and_then(|profiles| profiles.get(*name)).is_none())
    });
    let resolve = |active: &str| {
        resolve_model_profile(&ResolveModelProfileInput {
            profiles: user_profiles(config),
            active,
            available_models: &available,
            custom_providers: None,
        })
    };
    if let Some(explicit) = explicit {
        let resolution = resolve(explicit);
        return view(explicit, ModelProfileSelection::Set, &resolution, config);
    }
    let first = DEFAULT_MODEL_PROFILE_ORDER[0];
    let lane = DEFAULT_MODEL_PROFILE_ORDER
        .iter()
        .copied()
        .find(|id| matches!(resolve(id), ModelProfileResolution::Resolved { .. }))
        .unwrap_or(first);
    let resolution = resolve(lane);
    view(lane, ModelProfileSelection::Default, &resolution, config)
}
