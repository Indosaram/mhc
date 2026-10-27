//! Model profiles: named model chains for the main session at session start (spec
//! model-selection §6). Pure resolution; the session-start glue lives in `ext`.

use crate::grammar::{normalize_reasoning, split_reasoning_suffix};
use crate::matching::match_available;
use crate::transforms::{strip_provider, transform_for};
use maho_ai::types::ThinkingLevel;
use serde_json::Value;
use std::collections::BTreeSet;

/// Built-in profile ids, in default order (interface facts).
pub use maho_config::BUILTIN_MODEL_PROFILE_IDS as DEFAULT_PROFILE_ORDER;
/// The retired profile id, treated as unset unless the user defines it.
pub const RETIRED_PROFILE_ID: &str = "recommended";

/// One rung of a profile chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileRung {
    /// Ranked providers; empty means "served by any provider".
    pub providers: Vec<String>,
    pub model: String,
    pub reasoning: Option<String>,
}

impl ProfileRung {
    /// `firstprovider/model`, or the bare model.
    pub fn label(&self) -> String {
        match self.providers.first() {
            Some(provider) => format!("{provider}/{}", strip_provider(provider, &self.model)),
            None => self.model.clone(),
        }
    }
}

/// A model profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProfile {
    pub id: String,
    pub display_name: String,
    pub family: Option<String>,
    pub tier: Option<String>,
    pub rungs: Vec<ProfileRung>,
    /// Defined by the user (rungs with providers then match exactly).
    pub user_defined: bool,
}

fn level_name(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

fn recommended_rung(model: &str, reasoning: Option<&str>) -> ProfileRung {
    let default_level = maho_ext_recommended_models::RECOMMENDED_DEFAULT_MODELS
        .iter()
        .find(|(id, _)| *id == model)
        .map(|(_, level)| level_name(*level));
    ProfileRung {
        providers: maho_ext_recommended_models::ranked_providers(model)
            .iter()
            .map(|p| (*p).to_owned())
            .collect(),
        model: model.to_owned(),
        reasoning: reasoning.or(default_level).map(str::to_owned),
    }
}

fn label_for(family: &str, tier: &str) -> String {
    let family = match family {
        "daily" => "Daily",
        _ => "Geeky",
    };
    let tier = match tier {
        "heavy" => "heavy",
        _ => "normal",
    };
    format!("{family} ({tier})")
}

/// The built-in profiles, sourced from the MIT recommended-model list
/// (`maho-ext-recommended-models`): model ids, provider rankings and default thinking levels.
///
/// Composition (suggested in the spec, to be confirmed by the maintainer): `daily-normal` is the
/// first recommended general model at medium effort with the non-GPT alternates after it;
/// `daily-heavy` is the recommended top model at high effort; the `geeky-*` lanes use the
/// recommended GPT-family models.
pub fn builtin_profiles() -> Vec<ModelProfile> {
    let lanes: [(&str, &str, &str, Vec<ProfileRung>); 4] = [
        (
            "daily-normal",
            "daily",
            "normal",
            vec![
                recommended_rung("claude-opus-5-5", Some("medium")),
                recommended_rung("kimi-k3", None),
                recommended_rung("glm-5.3", None),
            ],
        ),
        (
            "daily-heavy",
            "daily",
            "heavy",
            vec![
                recommended_rung("claude-fable-5-1", Some("high")),
                recommended_rung("claude-opus-5-5", Some("high")),
            ],
        ),
        (
            "geeky-normal",
            "geeky",
            "normal",
            vec![
                recommended_rung("gpt-6.1-sol", Some("medium")),
                recommended_rung("gpt-6-astra", Some("medium")),
            ],
        ),
        (
            "geeky-heavy",
            "geeky",
            "heavy",
            vec![
                recommended_rung("gpt-6-astra", None),
                recommended_rung("gpt-6.1-sol", Some("high")),
            ],
        ),
    ];
    lanes
        .into_iter()
        .map(|(id, family, tier, rungs)| ModelProfile {
            id: id.into(),
            display_name: label_for(family, tier),
            family: Some(family.into()),
            tier: Some(tier.into()),
            rungs,
            user_defined: false,
        })
        .collect()
}

/// Parses one user rung: `provider/model[:level]`, a bare `model[:level]` (any provider), or an
/// object (`reasoning` wins over a suffix). Empty reasoning is dropped.
pub fn parse_user_rung(value: &Value) -> Option<ProfileRung> {
    let (text, explicit) = match value {
        Value::String(text) => (text.as_str(), None),
        Value::Object(object) => (
            object.get("model")?.as_str()?,
            object.get("reasoning").and_then(Value::as_str),
        ),
        _ => return None,
    };
    let (base, suffix) = split_reasoning_suffix(text, false);
    if base.is_empty() {
        return None;
    }
    let reasoning = explicit
        .and_then(normalize_reasoning)
        .or_else(|| suffix.and_then(|s| normalize_reasoning(&s)));
    let (providers, model) = match base.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
            (vec![provider.to_owned()], model.to_owned())
        }
        _ => (Vec::new(), base),
    };
    Some(ProfileRung {
        providers,
        model,
        reasoning,
    })
}

/// Built-ins in their order, each replaced wholesale by a user entry of the same id, then new
/// user ids appended. `display_name`/`family`/`tier` fall back to the replaced built-in.
pub fn merge_profiles(model_profiles: Option<&Value>) -> Vec<ModelProfile> {
    let mut out = builtin_profiles();
    let Some(entries) = model_profiles.and_then(Value::as_object) else {
        return out;
    };
    for (id, entry) in entries {
        let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
        let rungs: Vec<ProfileRung> = entry
            .get("models")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(parse_user_rung).collect())
            .unwrap_or_default();
        match out.iter_mut().find(|profile| &profile.id == id) {
            Some(builtin) => {
                builtin.display_name =
                    text("display_name").unwrap_or_else(|| builtin.display_name.clone());
                builtin.family = text("family").or_else(|| builtin.family.clone());
                builtin.tier = text("tier").or_else(|| builtin.tier.clone());
                builtin.rungs = rungs;
                builtin.user_defined = true;
            }
            None => out.push(ModelProfile {
                id: id.clone(),
                display_name: text("display_name").unwrap_or_else(|| id.clone()),
                family: text("family"),
                tier: text("tier"),
                rungs,
                user_defined: true,
            }),
        }
    }
    out
}

/// Candidates excluded by the request-auth probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusions {
    pub providers: BTreeSet<String>,
    /// `provider/model` entries.
    pub models: BTreeSet<String>,
}

impl Exclusions {
    fn filter(&self, available: &[String]) -> Vec<String> {
        available
            .iter()
            .filter(|entry| {
                let provider = entry.split_once('/').map_or("", |(p, _)| p);
                !self.providers.contains(provider) && !self.models.contains(*entry)
            })
            .cloned()
            .collect()
    }
}

/// How a profile resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileResolution {
    Resolved {
        profile: String,
        provider: String,
        model_id: String,
        reasoning: Option<String>,
        skipped: Vec<String>,
        pinned: bool,
    },
    Unavailable {
        profile: String,
        chain: Vec<String>,
    },
    Empty {
        profile: String,
    },
    Unknown {
        name: String,
        known: Vec<String>,
        message: String,
    },
}

fn split_entry(entry: &str) -> (String, String) {
    match entry.split_once('/') {
        Some((provider, model)) => (provider.to_owned(), model.to_owned()),
        None => (String::new(), entry.to_owned()),
    }
}

/// Matches one rung against `available` (§6.4).
pub fn match_rung(
    rung: &ProfileRung,
    user_defined: bool,
    available: &[String],
) -> Option<(String, String)> {
    if rung.providers.is_empty() {
        return match_available(&rung.model, available, None).map(|entry| split_entry(&entry));
    }
    if user_defined {
        return rung.providers.iter().find_map(|provider| {
            let model = strip_provider(provider, &rung.model);
            let entry = format!("{provider}/{model}");
            available
                .contains(&entry)
                .then(|| (provider.clone(), model.to_owned()))
        });
    }
    for provider in &rung.providers {
        let only = std::slice::from_ref(provider);
        let transformed = transform_for(provider, &rung.model);
        let as_written = strip_provider(provider, &rung.model).to_owned();
        let mut ids = vec![transformed.clone()];
        if as_written != transformed {
            ids.push(as_written);
        }
        for id in ids {
            if let Some(entry) = match_available(&format!("{provider}/{id}"), available, Some(only))
            {
                return Some(split_entry(&entry));
            }
        }
    }
    None
}

/// Resolves `active` (a profile id or a `provider/model[:level]` pin) against `available`.
pub fn resolve_profile(
    profiles: &[ModelProfile],
    active: &str,
    available: &[String],
    exclusions: &Exclusions,
) -> ProfileResolution {
    let active = active.trim();
    let pool = exclusions.filter(available);
    if active.contains('/') {
        let rung = parse_user_rung(&Value::String(active.to_owned()));
        if let Some(rung) = &rung
            && !available.is_empty()
            && let Some((provider, model_id)) = match_rung(rung, false, available)
        {
            return ProfileResolution::Resolved {
                profile: active.to_owned(),
                provider,
                model_id,
                reasoning: rung.reasoning.clone(),
                skipped: Vec::new(),
                pinned: true,
            };
        }
        return ProfileResolution::Unavailable {
            profile: active.to_owned(),
            chain: vec![rung.map_or_else(|| active.to_owned(), |r| r.label())],
        };
    }
    let Some(profile) = profiles.iter().find(|p| p.id == active) else {
        let mut known: Vec<String> = profiles.iter().map(|p| p.id.clone()).collect();
        known.sort();
        return ProfileResolution::Unknown {
            name: active.to_owned(),
            message: format!(
                "unknown model profile \"{active}\"; known profiles: {}",
                known.join(", ")
            ),
            known,
        };
    };
    if profile.rungs.is_empty() {
        return ProfileResolution::Empty {
            profile: profile.id.clone(),
        };
    }
    if !pool.is_empty() {
        let mut skipped = Vec::new();
        for rung in &profile.rungs {
            if let Some((provider, model_id)) = match_rung(rung, profile.user_defined, &pool) {
                return ProfileResolution::Resolved {
                    profile: profile.id.clone(),
                    provider,
                    model_id,
                    reasoning: rung.reasoning.clone(),
                    skipped,
                    pinned: false,
                };
            }
            skipped.push(rung.label());
        }
    }
    ProfileResolution::Unavailable {
        profile: profile.id.clone(),
        chain: profile.rungs.iter().map(ProfileRung::label).collect(),
    }
}

/// The default lanes in order; the first `Resolved` wins, else the `daily-normal` result.
pub fn resolve_default(
    profiles: &[ModelProfile],
    available: &[String],
    exclusions: &Exclusions,
) -> ProfileResolution {
    let mut first = None;
    for id in DEFAULT_PROFILE_ORDER {
        let result = resolve_profile(profiles, id, available, exclusions);
        if matches!(result, ProfileResolution::Resolved { .. }) {
            return result;
        }
        first.get_or_insert(result);
    }
    first.unwrap_or(ProfileResolution::Empty {
        profile: DEFAULT_PROFILE_ORDER[0].into(),
    })
}

/// The configured profile to use: `None` when unset, blank, or the retired id that the user did
/// not define.
pub fn effective_profile(
    configured: Option<&str>,
    model_profiles: Option<&Value>,
) -> Option<String> {
    let configured = configured.map(str::trim).filter(|c| !c.is_empty())?;
    if configured == RETIRED_PROFILE_ID
        && !model_profiles
            .and_then(Value::as_object)
            .is_some_and(|m| m.contains_key(RETIRED_PROFILE_ID))
    {
        return None;
    }
    Some(configured.to_owned())
}
