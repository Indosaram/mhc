//! Port of omo-senpi `components/model-profile/resolve.ts` at omo `v5.1.47`: the pure resolver that
//! overlays `omo.json` `model_profiles` on the builtin table and picks the first rung the live
//! registry can serve.
//!
// allow: SIZE_OK - the repository's port rule is one senpi source file per Rust module, so this
// module stays the single translation of `resolve.ts`.
//!
//! Pure by construction: `available_models` is the flat `provider/id` list, so the same call is unit
//! testable and the session-start component ([`crate::index`]) owns every side effect. This module is
//! the ONLY producer of the unknown-profile message.
//!
//! Rung matching uses the SAME primitives the category chains match with
//! (`model_core::fuzzy_match_model` + `model_core::transform_model_for_provider`), one rung at a
//! time, so a profile and a category can never disagree on provider spelling or on which registry id
//! counts as "that model". A rung that lists providers is served only by them, so a builtin lane
//! never lands the session on a gateway's copy of its model (#9146); only a user's bare model id,
//! which names no provider, matches anywhere.
//!
//! A builtin rung that none of its listed providers serves may still resolve on a provider the user
//! DECLARED in `models.json` ([`crate::custom_providers`]), but only on an id declared for it, in
//! declaration order (#10485): the declaration is the permission, so a provider's own catalog never
//! lends a builtin lane an id the user never named.

use std::collections::BTreeSet;

use indexmap::IndexMap;
use model_core::{fuzzy_match_model, transform_model_for_provider};
use maho_ext_api::JsonValue;

use crate::builtin_profiles::{
    BuiltinModelProfile, ModelProfileFamily, ModelProfileTier, builtin_model_profiles,
};
use crate::custom_providers::DeclaredModelProvider;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProfileSource {
    Builtin,
    User,
    Pin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProfileSummary {
    pub id: String,
    pub display_name: String,
    /// The lane's one-line warning, when it has one; a picker renders it in bold.
    pub caution: Option<String>,
    pub source: ModelProfileSource,
    pub family: Option<ModelProfileFamily>,
    pub tier: Option<ModelProfileTier>,
}

/// One rung of a profile chain, after builtin entries and user config entries are unified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProfileRung {
    pub providers: Vec<String>,
    pub model: String,
    pub reasoning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProfileDefinition {
    pub profile: ModelProfileSummary,
    pub models: Vec<ModelProfileRung>,
}

/// `omo.json` `model_profiles` is consumed as the resolved JSON the config loader produced; the
/// schema and the loader stay with their owner (`omo-config-core`), never re-implemented here.
pub struct ResolveModelProfileInput<'a> {
    pub profiles: Option<&'a JsonValue>,
    /// `omo.json` `model_profile`: either a profile id or a literal `provider/model` pin.
    pub active: &'a str,
    /// The flat `provider/id` list the live senpi registry reports.
    pub available_models: &'a [String],
    /// [`crate::custom_providers::custom_model_providers`]: models.json providers with their
    /// declared model ids, in declaration order.
    pub custom_providers: Option<&'a [DeclaredModelProvider]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelProfileResolution {
    Resolved {
        profile: ModelProfileSummary,
        provider: String,
        model_id: String,
        reasoning: Option<String>,
        skipped: Vec<String>,
    },
    Unavailable {
        profile: ModelProfileSummary,
        chain: Vec<String>,
    },
    Empty {
        profile: ModelProfileSummary,
    },
    Unknown {
        name: String,
        known: Vec<String>,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RungMatch {
    provider: String,
    model_id: String,
    reasoning: Option<String>,
}

fn builtin_rung(rung: &crate::builtin_profiles::BuiltinRung) -> ModelProfileRung {
    ModelProfileRung {
        providers: rung.providers.iter().map(|provider| (*provider).to_owned()).collect(),
        model: rung.model.to_owned(),
        reasoning: rung.variant.map(str::to_owned),
    }
}

/// A user chain entry is either a string - `provider/model`, a bare model id, either one optionally
/// carrying the canonical `:<reasoning>` suffix - or the `{ model, reasoning }` object form. A bare
/// id leaves `providers` EMPTY on purpose: the matcher then accepts that model from whichever
/// provider the registry serves it through, which is exactly what a user who named no provider
/// asked for.
fn user_rung(entry: &JsonValue) -> Option<ModelProfileRung> {
    let (raw, object_reasoning) = match entry {
        JsonValue::String(text) => (text.trim().to_owned(), None),
        JsonValue::Object(map) => {
            let model = map.get("model")?.as_str()?.trim().to_owned();
            let reasoning = map.get("reasoning").and_then(JsonValue::as_str).map(str::to_owned);
            (model, reasoning)
        }
        _ => return None,
    };
    let suffix_index = raw.rfind(':');
    let (selector, suffix_reasoning) = match suffix_index {
        Some(index) if index > 0 => (
            raw[..index].to_owned(),
            Some(raw[index + 1..].to_owned()),
        ),
        _ => (raw.clone(), None),
    };
    let reasoning = object_reasoning.or(suffix_reasoning);

    let separator_index = selector.find('/');
    let scoped = separator_index.is_some_and(|index| index > 0 && index < selector.len() - 1);
    let providers = if scoped {
        vec![selector[..separator_index.unwrap_or_default()].to_owned()]
    } else {
        Vec::new()
    };
    let model = if scoped {
        selector[separator_index.unwrap_or_default() + 1..].to_owned()
    } else {
        selector
    };
    Some(ModelProfileRung {
        providers,
        model,
        reasoning: reasoning.filter(|reasoning| !reasoning.is_empty()),
    })
}

fn family_of(value: &JsonValue) -> Option<ModelProfileFamily> {
    match value.get("family").and_then(JsonValue::as_str) {
        Some("daily") => Some(ModelProfileFamily::Daily),
        Some("geeky") => Some(ModelProfileFamily::Geeky),
        _ => None,
    }
}

fn tier_of(value: &JsonValue) -> Option<ModelProfileTier> {
    match value.get("tier").and_then(JsonValue::as_str) {
        Some("normal") => Some(ModelProfileTier::Normal),
        Some("heavy") => Some(ModelProfileTier::Heavy),
        _ => None,
    }
}

fn caution_of(value: &JsonValue) -> Option<String> {
    value.get("caution").and_then(JsonValue::as_str).map(str::to_owned)
}

/// The builtin table overlaid with `omo.json` `model_profiles`. A user entry replaces a builtin of
/// the same name WHOLESALE - no per-field merge, so a label-only override does not inherit the
/// builtin chain and is reported as `empty` rather than silently running builtin models under a
/// user's label.
pub fn merge_model_profiles(
    profiles: Option<&JsonValue>,
) -> IndexMap<String, ModelProfileDefinition> {
    let mut merged: IndexMap<String, ModelProfileDefinition> = IndexMap::new();
    for (id, builtin) in builtin_model_profiles() {
        merged.insert(id.to_owned(), builtin_definition(id, &builtin));
    }
    if let Some(JsonValue::Object(entries)) = profiles {
        for (id, entry) in entries {
            let replaced = merged.get(id).map(|definition| definition.profile.clone());
            let family = family_of(entry).or_else(|| replaced.as_ref().and_then(|profile| profile.family));
            let tier = tier_of(entry).or_else(|| replaced.as_ref().and_then(|profile| profile.tier));
            let caution = caution_of(entry).or_else(|| replaced.as_ref().and_then(|profile| profile.caution.clone()));
            let display_name = entry
                .get("display_name")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .or_else(|| replaced.as_ref().map(|profile| profile.display_name.clone()))
                .unwrap_or_else(|| id.clone());
            let models = entry
                .get("models")
                .and_then(JsonValue::as_array)
                .map(|entries| entries.iter().filter_map(user_rung).collect())
                .unwrap_or_default();
            merged.insert(
                id.clone(),
                ModelProfileDefinition {
                    profile: ModelProfileSummary {
                        id: id.clone(),
                        display_name,
                        caution,
                        source: ModelProfileSource::User,
                        family,
                        tier,
                    },
                    models,
                },
            );
        }
    }
    merged
}

fn builtin_definition(id: &str, builtin: &BuiltinModelProfile) -> ModelProfileDefinition {
    ModelProfileDefinition {
        profile: ModelProfileSummary {
            id: id.to_owned(),
            display_name: builtin.display_name.to_owned(),
            caution: builtin.caution.map(str::to_owned),
            source: ModelProfileSource::Builtin,
            family: Some(builtin.family),
            tier: Some(builtin.tier),
        },
        models: builtin.models.iter().map(builtin_rung).collect(),
    }
}

fn format_rung(rung: &ModelProfileRung) -> String {
    match rung.providers.first() {
        Some(provider) => format!("{provider}/{}", rung.model),
        None => rung.model.clone(),
    }
}

fn unknown_profile_message(name: &str, known: &[String]) -> String {
    format!("model_profile \"{name}\" is not defined; known profiles: {}", known.join(", "))
}

fn model_id_for_provider(provider: &str, model: &str) -> String {
    model
        .strip_prefix(&format!("{provider}/"))
        .map(str::to_owned)
        .unwrap_or_else(|| model.to_owned())
}

fn split_selector(selector: &str, reasoning: &Option<String>) -> Option<RungMatch> {
    let separator_index = selector.find('/')?;
    if separator_index == 0 || separator_index == selector.len() - 1 {
        return None;
    }
    Some(RungMatch {
        provider: selector[..separator_index].to_owned(),
        model_id: selector[separator_index + 1..].to_owned(),
        reasoning: reasoning.clone(),
    })
}

fn match_rung(rung: &ModelProfileRung, available: &indexmap::IndexSet<String>) -> Option<RungMatch> {
    if rung.providers.is_empty() {
        let matched = fuzzy_match_model(&rung.model, available, None)?;
        return split_selector(&matched, &rung.reasoning);
    }
    for provider in &rung.providers {
        let entry_model_id = model_id_for_provider(provider, &rung.model);
        let transformed_model_id = transform_model_for_provider(provider, &entry_model_id);
        // A listed provider may publish the rung id as the chain spells it rather than transformed
        // (kimi-coding/kimi-k3 for a kimi-k3 rung); both spellings stay on this provider.
        let candidate_ids = if transformed_model_id == entry_model_id {
            vec![entry_model_id]
        } else {
            vec![transformed_model_id, entry_model_id]
        };
        for model_id in candidate_ids {
            let full_model = format!("{provider}/{model_id}");
            if let Some(matched) =
                fuzzy_match_model(&full_model, available, Some(std::slice::from_ref(provider)))
            {
                return split_selector(&matched, &rung.reasoning);
            }
        }
    }
    None
}

fn match_scoped_user_rung(
    rung: &ModelProfileRung,
    available: &indexmap::IndexSet<String>,
) -> Option<RungMatch> {
    for provider in &rung.providers {
        if !available.contains(&format!("{provider}/{}", rung.model)) {
            continue;
        }
        return Some(RungMatch {
            provider: provider.clone(),
            model_id: rung.model.clone(),
            reasoning: rung.reasoning.clone(),
        });
    }
    None
}

// A builtin rung falls back to a provider the user declared in models.json only on a model id the
// user declared for it, served as the exact `<provider>/<model id>`: a gateway's re-published copy
// carries a vendor-prefixed id, an undeclared provider is not in the list, and a built-in catalog
// extended with other models does not serve this one, so #9146 still holds.
fn match_custom_provider(
    rung: &ModelProfileRung,
    available: &indexmap::IndexSet<String>,
    custom_providers: &[DeclaredModelProvider],
) -> Option<RungMatch> {
    let declared: Vec<String> = custom_providers
        .iter()
        .filter(|declared| {
            !rung.providers.contains(&declared.provider)
                && declared.model_ids.contains(&rung.model)
        })
        .map(|declared| declared.provider.clone())
        .collect();
    match_scoped_user_rung(
        &ModelProfileRung { providers: declared, ..rung.clone() },
        available,
    )
}

fn match_profile_rung(
    rung: &ModelProfileRung,
    available: &indexmap::IndexSet<String>,
    definition: &ModelProfileDefinition,
    custom_providers: &[DeclaredModelProvider],
) -> Option<RungMatch> {
    if definition.profile.source == ModelProfileSource::User && !rung.providers.is_empty() {
        return match_scoped_user_rung(rung, available);
    }
    let listed = match_rung(rung, available);
    if listed.is_some() || definition.profile.source != ModelProfileSource::Builtin {
        return listed;
    }
    match_custom_provider(rung, available, custom_providers)
}

/// Resolve the active `model_profile` against the live registry listing.
pub fn resolve_model_profile(input: &ResolveModelProfileInput<'_>) -> ModelProfileResolution {
    let active = input.active.trim();
    let available: indexmap::IndexSet<String> = input.available_models.iter().cloned().collect();

    // A value carrying a slash IS the pin: the tier and the pin share one key, so no second source of
    // truth exists and `settings.json` is never consulted for "the user pinned a model".
    if active.contains('/') {
        let profile = ModelProfileSummary {
            id: active.to_owned(),
            display_name: active.to_owned(),
            caution: None,
            source: ModelProfileSource::Pin,
            family: None,
            tier: None,
        };
        let matched = if available.is_empty() {
            None
        } else {
            user_rung(&JsonValue::String(active.to_owned()))
                .and_then(|rung| match_rung(&rung, &available))
        };
        return match matched {
            Some(matched) => ModelProfileResolution::Resolved {
                profile,
                provider: matched.provider,
                model_id: matched.model_id,
                reasoning: matched.reasoning,
                skipped: Vec::new(),
            },
            None => ModelProfileResolution::Unavailable {
                profile,
                chain: vec![active.to_owned()],
            },
        };
    }

    let profiles = merge_model_profiles(input.profiles);
    let Some(definition) = profiles.get(active) else {
        let known: Vec<String> = profiles.keys().cloned().collect::<BTreeSet<_>>().into_iter().collect();
        return ModelProfileResolution::Unknown {
            name: active.to_owned(),
            message: unknown_profile_message(active, &known),
            known,
        };
    };
    if definition.models.is_empty() {
        return ModelProfileResolution::Empty {
            profile: definition.profile.clone(),
        };
    }

    let mut skipped: Vec<String> = Vec::new();
    let custom_providers = input.custom_providers.unwrap_or(&[]);
    if !available.is_empty() {
        for rung in &definition.models {
            if let Some(matched) = match_profile_rung(rung, &available, definition, custom_providers) {
                return ModelProfileResolution::Resolved {
                    profile: definition.profile.clone(),
                    provider: matched.provider,
                    model_id: matched.model_id,
                    reasoning: matched.reasoning,
                    skipped,
                };
            }
            skipped.push(format_rung(rung));
        }
    }
    ModelProfileResolution::Unavailable {
        profile: definition.profile.clone(),
        chain: definition.models.iter().map(format_rung).collect(),
    }
}
