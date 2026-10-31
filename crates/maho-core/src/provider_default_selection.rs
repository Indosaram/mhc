//! Port of senpi `packages/coding-agent/src/core/provider-default-selection.ts`.

use std::cell::RefCell;
use std::collections::HashMap;

use maho_ai::model::Model;

use crate::provider_composer::AuthStatus;

pub trait ProviderAuthStatusSource {
    /// `None` for compatibility sources without auth status: they report every provider as configured.
    fn get_provider_auth_status(&self, _provider_id: &str) -> Option<AuthStatus> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderDefaultProvenance {
    ProviderDefault,
    FirstAvailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderDefaultSelection {
    pub model: Model,
    pub provenance: ProviderDefaultProvenance,
}

/// True when a provider is available only through a shared cloud credential chain (ambient AWS env,
/// Google ADC). Such a provider stays selectable but never outranks one the user configured (senpi#2327).
pub fn is_ambient_only_provider(source: &dyn ProviderAuthStatusSource, provider_id: &str) -> bool {
    source
        .get_provider_auth_status(provider_id)
        .is_some_and(|status| status.ambient)
}

pub fn create_ambient_provider_check(
    source: &dyn ProviderAuthStatusSource,
) -> impl Fn(&str) -> bool + '_ {
    let cache: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
    move |provider_id: &str| {
        if let Some(ambient) = cache.borrow().get(provider_id) {
            return *ambient;
        }
        let ambient = is_ambient_only_provider(source, provider_id);
        cache.borrow_mut().insert(provider_id.to_owned(), ambient);
        ambient
    }
}

/// The automatic default among available models. Models from providers the user configured are
/// considered first; ambient-only providers only when nothing else is available. Within that group the
/// first `provider_defaults` entry (declared order) that is available wins, else the group's first
/// available model.
pub fn select_provider_default(
    available_models: &[Model],
    provider_defaults: &[(String, String)],
    source: &dyn ProviderAuthStatusSource,
) -> Option<ProviderDefaultSelection> {
    let is_ambient = create_ambient_provider_check(source);
    let configured: Vec<&Model> = available_models
        .iter()
        .filter(|model| !is_ambient(model.provider.as_str()))
        .collect();
    let group: Vec<&Model> = if configured.is_empty() {
        available_models.iter().collect()
    } else {
        configured
    };
    for (provider, model_id) in provider_defaults {
        if let Some(found) = group
            .iter()
            .find(|model| &model.provider == provider && &model.id == model_id)
        {
            return Some(ProviderDefaultSelection {
                model: (*found).clone(),
                provenance: ProviderDefaultProvenance::ProviderDefault,
            });
        }
    }
    group.first().map(|model| ProviderDefaultSelection {
        model: (*model).clone(),
        provenance: ProviderDefaultProvenance::FirstAvailable,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn model(provider: &str, id: &str) -> Model {
        let mut model =
            maho_ai::models_generated::get_builtin_model("anthropic", "claude-sonnet-4-5")
                .expect("builtin anthropic model")
                .clone();
        model.provider = provider.to_owned();
        model.id = id.to_owned();
        model
    }

    pub(crate) struct Ambient(pub &'static [&'static str]);

    impl ProviderAuthStatusSource for Ambient {
        fn get_provider_auth_status(&self, provider_id: &str) -> Option<AuthStatus> {
            Some(AuthStatus {
                ambient: self.0.contains(&provider_id),
                configured: true,
                source: None,
                label: None,
            })
        }
    }

    #[test]
    fn configured_providers_outrank_ambient_ones() {
        let models = vec![model("bedrock", "b"), model("openai", "o")];
        let selection = select_provider_default(&models, &[], &Ambient(&["bedrock"]));
        assert_eq!(selection.as_ref().map(|s| s.model.id.as_str()), Some("o"));
        assert_eq!(
            selection.map(|s| s.provenance),
            Some(ProviderDefaultProvenance::FirstAvailable)
        );
        let only_ambient = select_provider_default(&models[..1], &[], &Ambient(&["bedrock"]));
        assert_eq!(only_ambient.map(|s| s.model.id), Some("b".to_owned()));
    }

    #[test]
    fn the_first_declared_default_in_the_group_wins() {
        let models = vec![
            model("openai", "o1"),
            model("anthropic", "a1"),
            model("anthropic", "a2"),
        ];
        let defaults = vec![
            ("google".to_owned(), "g".to_owned()),
            ("anthropic".to_owned(), "a2".to_owned()),
        ];
        let selection = select_provider_default(&models, &defaults, &Ambient(&[]));
        assert_eq!(selection.as_ref().map(|s| s.model.id.as_str()), Some("a2"));
        assert_eq!(
            selection.map(|s| s.provenance),
            Some(ProviderDefaultProvenance::ProviderDefault)
        );
        assert!(select_provider_default(&[], &defaults, &Ambient(&[])).is_none());
    }
}
