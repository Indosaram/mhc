//! `planner-explicit-pin.ts`: parse an explicit task model pin ONCE and resolve it against the live
//! registry, or fail closed (#9722).
//!
//! The raw `provider/model:level` string is never trusted as a model id again. A miss is a typed
//! `model_unavailable` naming the pin and the settings default the child would otherwise have ridden;
//! a malformed pin is a typed `invalid_target`. An excluded canonical identity is refused with the
//! matching `exclude_models` pattern, so the caller sees exactly what the refusal protected it from.

use senpi_task::{
    host::SenpiModelRegistry,
    model_exclusion::excluded_model_pattern,
    state::{ResolvedModelRecord, ResolvedModelSource},
};

/// `NO_REGISTRY_MESSAGE`: the one wording every fail-closed path shares.
pub const NO_REGISTRY_MESSAGE: &str =
    "No senpi model registry is available yet to resolve a task model.";

/// The settings-default route a pin-less child would ride, named in `model_unavailable` errors.
pub type ResolveDefaultRoute = std::sync::Arc<dyn Fn() -> Option<DefaultRoute> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultRoute {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplicitPinCode {
    InvalidTarget,
    ModelUnavailable,
}

impl ExplicitPinCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidTarget => "invalid_target",
            Self::ModelUnavailable => "model_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitPinError {
    pub code: ExplicitPinCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplicitPinResolution {
    Resolved {
        canonical: String,
        metadata: ResolvedModelRecord,
        thinking_level: Option<String>,
    },
    Error(ExplicitPinError),
}

/// The decorators a pin may carry: `provider/model:thinking-level`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinDecorators {
    pub base: String,
    pub thinking_level: Option<String>,
    pub service_tier: Option<String>,
}

/// `splitModelDecorators`: the base id plus its `:thinking-level` and `:service-tier` suffixes.
pub fn split_model_decorators(pin: &str) -> PinDecorators {
    let mut base = pin.trim().to_string();
    let mut thinking_level = None;
    let mut service_tier = None;
    // The suffixes are `:level` and `:tier`, in that order, and neither may contain a further colon.
    while let Some((head, tail)) = base.rsplit_once(':') {
        if tail.is_empty() || head.is_empty() || tail.contains('/') {
            break;
        }
        let tail_lower = tail.to_ascii_lowercase();
        if matches!(tail_lower.as_str(), "priority" | "standard" | "flex") {
            if service_tier.is_none() {
                service_tier = Some(tail.to_string());
                base = head.to_string();
                continue;
            }
            break;
        }
        if thinking_level.is_none() {
            thinking_level = Some(tail.to_string());
            base = head.to_string();
            continue;
        }
        break;
    }
    PinDecorators {
        base,
        thinking_level,
        service_tier,
    }
}

fn with_default_route(message: String, resolve_default_route: Option<&ResolveDefaultRoute>) -> String {
    match resolve_default_route.and_then(|route| route()) {
        None => message,
        Some(route) => format!(
            "{message} The child would otherwise start on the settings default route {}/{}.",
            route.provider, route.model_id
        ),
    }
}

fn explicit_pin_resolved(
    pin: &str,
    provider: &str,
    model_id: &str,
    thinking_level: Option<String>,
) -> ExplicitPinResolution {
    let mut metadata = ResolvedModelRecord::new(ResolvedModelSource::Explicit, provider, model_id);
    metadata.display = pin.to_string();
    metadata.reasoning = thinking_level.clone();
    ExplicitPinResolution::Resolved {
        canonical: format!("{provider}/{model_id}"),
        metadata,
        thinking_level,
    }
}

/// `resolveExplicitPin`: the canonical identity, its metadata, and its thinking level, or a typed
/// refusal naming the `exclude_models` pattern that keeps it out.
pub fn resolve_explicit_pin(
    pin: &str,
    registry: Option<&dyn SenpiModelRegistry>,
    resolve_default_route: Option<&ResolveDefaultRoute>,
    exclude_models: Option<&[String]>,
) -> ExplicitPinResolution {
    let resolution = resolve_live_pin(pin, registry, resolve_default_route);
    let ExplicitPinResolution::Resolved { canonical, .. } = &resolution else {
        return resolution;
    };
    let Some(pattern) = excluded_model_pattern(canonical, exclude_models.unwrap_or(&[])) else {
        return resolution;
    };
    ExplicitPinResolution::Error(ExplicitPinError {
        code: ExplicitPinCode::ModelUnavailable,
        message: format!(
            "The task model pin \"{pin}\" resolves to {canonical}, which exclude_models in omo.json keeps out (\"{pattern}\")."
        ),
    })
}

fn resolve_live_pin(
    pin: &str,
    registry: Option<&dyn SenpiModelRegistry>,
    resolve_default_route: Option<&ResolveDefaultRoute>,
) -> ExplicitPinResolution {
    let Some(registry) = registry else {
        return ExplicitPinResolution::Error(ExplicitPinError {
            code: ExplicitPinCode::ModelUnavailable,
            message: with_default_route(NO_REGISTRY_MESSAGE.to_string(), resolve_default_route),
        });
    };
    resolve_pin_by_exact_id(pin, registry, resolve_default_route)
}

fn resolve_pin_by_exact_id(
    pin: &str,
    registry: &dyn SenpiModelRegistry,
    resolve_default_route: Option<&ResolveDefaultRoute>,
) -> ExplicitPinResolution {
    let decorators = split_model_decorators(pin);
    let Some(slash) = decorators.base.find('/') else {
        return ExplicitPinResolution::Error(ExplicitPinError {
            code: ExplicitPinCode::InvalidTarget,
            message: format!(
                "The task model pin \"{pin}\" is malformed: use provider/model with an optional :thinking-level suffix."
            ),
        });
    };
    if slash == 0 || slash + 1 == decorators.base.len() {
        return ExplicitPinResolution::Error(ExplicitPinError {
            code: ExplicitPinCode::InvalidTarget,
            message: format!(
                "The task model pin \"{pin}\" is malformed: use provider/model with an optional :thinking-level suffix."
            ),
        });
    }
    if let Some(tier) = &decorators.service_tier {
        return ExplicitPinResolution::Error(ExplicitPinError {
            code: ExplicitPinCode::InvalidTarget,
            message: format!(
                "The task model pin \"{pin}\" carries service tier \"{tier}\", which a task pin cannot express; drop the suffix or set the tier on the parent."
            ),
        });
    }
    let provider = &decorators.base[..slash];
    let model_id = &decorators.base[slash + 1..];
    if registry.find(provider, model_id).is_none() {
        return ExplicitPinResolution::Error(ExplicitPinError {
            code: ExplicitPinCode::ModelUnavailable,
            message: with_default_route(
                format!("The task model pin \"{pin}\" did not resolve to a model in the live registry."),
                resolve_default_route,
            ),
        });
    }
    explicit_pin_resolved(pin, provider, model_id, decorators.thinking_level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    struct Registry {
        models: BTreeSet<String>,
    }

    impl SenpiModelRegistry for Registry {
        fn get_available(&self) -> Result<serde_json::Value, senpi_task::host::HostError> {
            Ok(json!(self
                .models
                .iter()
                .map(|key| {
                    let (provider, id) = key.split_once('/').unwrap_or((key.as_str(), ""));
                    json!({"provider": provider, "id": id})
                })
                .collect::<Vec<_>>()))
        }
        fn find(&self, provider: &str, model_id: &str) -> Option<serde_json::Value> {
            self.models
                .contains(&format!("{provider}/{model_id}"))
                .then(|| json!({"provider": provider, "id": model_id}))
        }
    }

    fn registry() -> Registry {
        Registry {
            models: ["openai/gpt-5", "openai/gpt-5-fast"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        }
    }

    #[test]
    fn an_excluded_pin_is_a_typed_refusal_naming_the_pattern() {
        let patterns = vec!["openai/gpt-5*".to_string()];
        match resolve_explicit_pin("openai/gpt-5", Some(&registry()), None, Some(&patterns)) {
            ExplicitPinResolution::Error(error) => {
                assert_eq!(error.code, ExplicitPinCode::ModelUnavailable);
                assert!(error.message.contains("openai/gpt-5*"), "{}", error.message);
            }
            ExplicitPinResolution::Resolved { .. } => panic!("an excluded pin must be refused"),
        }
    }

    #[test]
    fn a_pin_outside_the_list_resolves_with_its_thinking_level() {
        let patterns = vec!["anthropic/*".to_string()];
        match resolve_explicit_pin("openai/gpt-5:high", Some(&registry()), None, Some(&patterns)) {
            ExplicitPinResolution::Resolved {
                canonical,
                thinking_level,
                metadata,
            } => {
                assert_eq!(canonical, "openai/gpt-5");
                assert_eq!(thinking_level.as_deref(), Some("high"));
                assert_eq!(metadata.display, "openai/gpt-5:high");
            }
            ExplicitPinResolution::Error(error) => panic!("unexpected refusal: {}", error.message),
        }
    }

    #[test]
    fn a_malformed_pin_is_invalid_target() {
        match resolve_explicit_pin("gpt-5", Some(&registry()), None, None) {
            ExplicitPinResolution::Error(error) => {
                assert_eq!(error.code, ExplicitPinCode::InvalidTarget)
            }
            ExplicitPinResolution::Resolved { .. } => panic!("a pin without a provider is malformed"),
        }
    }

    #[test]
    fn a_missing_registry_names_the_default_route_the_child_would_have_ridden() {
        let route: ResolveDefaultRoute = std::sync::Arc::new(|| {
            Some(DefaultRoute {
                provider: "openai".into(),
                model_id: "gpt-5".into(),
            })
        });
        match resolve_explicit_pin("openai/gpt-5", None, Some(&route), None) {
            ExplicitPinResolution::Error(error) => {
                assert_eq!(error.code, ExplicitPinCode::ModelUnavailable);
                assert!(error.message.contains("openai/gpt-5"));
            }
            ExplicitPinResolution::Resolved { .. } => panic!("no registry must fail closed"),
        }
    }

    #[test]
    fn a_service_tier_suffix_cannot_be_expressed_by_a_pin() {
        match resolve_explicit_pin("openai/gpt-5:priority", Some(&registry()), None, None) {
            ExplicitPinResolution::Error(error) => {
                assert_eq!(error.code, ExplicitPinCode::InvalidTarget)
            }
            ExplicitPinResolution::Resolved { .. } => panic!("a tier suffix is refused"),
        }
    }
}
