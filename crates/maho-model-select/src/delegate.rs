//! Delegated-task model choice (spec model-selection §5).

use crate::grammar::{normalize_model, parse_variant, provider_of};
use crate::matching::match_available;
use crate::transforms::{strip_provider, transform_for};

/// One rung of a built-in fallback chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainRung {
    pub providers: Vec<String>,
    pub model: String,
    pub variant: Option<String>,
}

/// Inputs of [`resolve_delegate_model`].
#[derive(Clone, Debug, Default)]
pub struct DelegateInput {
    pub user_model: Option<String>,
    pub user_fallbacks: Vec<String>,
    pub category_default: Option<String>,
    pub category_default_is_user_configured: bool,
    pub chain: Vec<ChainRung>,
    pub allow_unlisted_providers: bool,
    pub available: Vec<String>,
    pub system_default: Option<String>,
    pub connected_providers: Option<Vec<String>>,
    pub has_model_cache: bool,
    pub has_connected_cache: bool,
}

/// The chosen model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub model: String,
    pub variant: Option<String>,
    /// Index of the chain rung that produced the model.
    pub rung: Option<usize>,
    pub from_fallback: bool,
}

/// Outcome of [`resolve_delegate_model`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DelegateChoice {
    Resolved(Resolved),
    /// No cache information at all: the caller retries later.
    Deferred,
    /// No model.
    None,
}

fn resolved(
    model: String,
    variant: Option<String>,
    rung: Option<usize>,
    from_fallback: bool,
) -> DelegateChoice {
    DelegateChoice::Resolved(Resolved {
        model,
        variant,
        rung,
        from_fallback,
    })
}

fn hint(model: &str) -> Option<Vec<String>> {
    provider_of(model).map(|p| vec![p.to_owned()])
}

fn matches(model: &str, available: &[String]) -> Option<String> {
    match_available(model, available, hint(model).as_deref())
}

fn log(message: &str) {
    maho_support::log(message, None);
}

/// Picks the model for a delegated task (first match wins).
pub fn resolve_delegate_model(input: &DelegateInput) -> DelegateChoice {
    let available = &input.available;
    let warm = !available.is_empty();
    let connected: Option<&Vec<String>> =
        input.connected_providers.as_ref().filter(|c| !c.is_empty());
    let is_connected = |model: &str| match (connected, provider_of(model)) {
        (Some(list), Some(provider)) => list.iter().any(|p| p == provider),
        _ => true,
    };

    if let Some(user) = normalize_model(input.user_model.as_deref()) {
        let (base, variant) = parse_variant(&user);
        let primary = if variant.is_some() {
            base
        } else {
            user.clone()
        };
        if warm && !input.user_fallbacks.is_empty() && matches(&primary, available).is_none() {
            for fallback in &input.user_fallbacks {
                let (model, fb_variant) = parse_variant(fallback);
                if let Some(found) = matches(&model, available) {
                    log(&format!(
                        "model {primary} is not available; using the configured fallback {found}"
                    ));
                    return resolved(found, fb_variant, None, true);
                }
            }
        }
        return resolved(primary, variant, None, false);
    }

    if !warm && connected.is_none() && !input.has_model_cache && !input.has_connected_cache {
        return DelegateChoice::Deferred;
    }

    if let Some(default) = normalize_model(input.category_default.as_deref()) {
        let (model, variant) = parse_variant(&default);
        if input.category_default_is_user_configured {
            return resolved(model, variant, None, false);
        }
        if !warm {
            if is_connected(&model) {
                return resolved(model, variant, None, false);
            }
            log(&format!(
                "skipping the category default {model}: its provider is not connected"
            ));
        } else if let Some(found) = matches(&model, available) {
            let last = model.rsplit('/').next().unwrap_or(&model);
            if last.len() > "-high".len() && last.ends_with("-high") && found != model {
                return resolved(model, variant, None, false);
            }
            return resolved(found, variant, None, false);
        }
    }

    for fallback in &input.user_fallbacks {
        let (model, variant) = parse_variant(fallback);
        if !warm {
            if is_connected(&model) {
                return resolved(model, variant, None, true);
            }
        } else if let Some(found) = matches(&model, available) {
            return resolved(found, variant, None, true);
        }
    }

    if let Some(choice) = resolve_chain(input, warm, connected) {
        return choice;
    }

    if let Some(system) = normalize_model(input.system_default.as_deref()) {
        return resolved(system, None, None, false);
    }
    DelegateChoice::None
}

fn resolve_chain(
    input: &DelegateInput,
    warm: bool,
    connected: Option<&Vec<String>>,
) -> Option<DelegateChoice> {
    let chain = &input.chain;
    if !warm {
        return match connected {
            None => {
                let rung = chain.first()?;
                let provider = rung.providers.first()?;
                Some(resolved(
                    format!("{provider}/{}", transform_for(provider, &rung.model)),
                    rung.variant.clone(),
                    Some(0),
                    true,
                ))
            }
            Some(list) => chain.iter().enumerate().find_map(|(index, rung)| {
                let provider = rung.providers.iter().find(|p| list.contains(p))?;
                log(&format!(
                    "using chain rung {} through the connected provider {provider}",
                    rung.model
                ));
                Some(resolved(
                    format!("{provider}/{}", transform_for(provider, &rung.model)),
                    rung.variant.clone(),
                    Some(index),
                    true,
                ))
            }),
        };
    }
    let explicit_high = input
        .category_default
        .as_deref()
        .map(|d| parse_variant(d).0)
        .filter(|d| d.len() > "-high".len() && d.ends_with("-high"));
    for (index, rung) in chain.iter().enumerate() {
        for provider in &rung.providers {
            let only = std::slice::from_ref(provider);
            let transformed = transform_for(provider, &rung.model);
            let as_written = strip_provider(provider, &rung.model);
            let found = match_available(
                &format!("{provider}/{transformed}"),
                &input.available,
                Some(only),
            )
            .or_else(|| {
                (transformed != as_written)
                    .then(|| {
                        match_available(
                            &format!("{provider}/{as_written}"),
                            &input.available,
                            Some(only),
                        )
                    })
                    .flatten()
            });
            if let Some(found) = found {
                if let Some(high) = &explicit_high
                    && rung.variant.as_deref() == Some("high")
                    && Some(found.as_str()) == high.strip_suffix("-high")
                {
                    return Some(resolved(high.clone(), None, Some(index), true));
                }
                return Some(resolved(found, rung.variant.clone(), Some(index), true));
            }
        }
        if input.allow_unlisted_providers {
            let later: Vec<&String> = chain[index + 1..]
                .iter()
                .filter(|other| other.model == rung.model)
                .flat_map(|other| other.providers.iter())
                .collect();
            let pool: Vec<String> = input
                .available
                .iter()
                .filter(|entry| provider_of(entry).is_none_or(|p| !later.iter().any(|l| *l == p)))
                .cloned()
                .collect();
            if let Some(found) = match_available(&rung.model, &pool, None) {
                return Some(resolved(found, rung.variant.clone(), Some(index), true));
            }
        }
    }
    None
}
