//! What setup needs from the application (after oh-my-pi's `SetupHost`, reduced to the provider
//! and model steps).

/// A provider offered by setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderChoice {
    pub id: String,
    /// Display name (the id when the catalog has none).
    pub name: String,
    /// A credential is present (auth.json or environment).
    pub has_credential: bool,
}

/// The application side of setup.
pub trait SetupHost {
    /// Providers of the catalog, in display order.
    fn providers(&self) -> Vec<ProviderChoice>;
    /// Model ids of `provider`, in display order.
    fn models(&self, provider: &str) -> Vec<String>;
    /// Stores an API key for `provider`.
    fn store_key(&mut self, provider: &str, key: &str) -> Result<(), String>;
    /// Makes `provider/model` the default model.
    fn set_default_model(&mut self, provider: &str, model: &str) -> Result<(), String>;
    /// Records a finished setup.
    fn mark_complete(&mut self, version: u32, provider: &str, model: &str) -> Result<(), String>;
}

/// The decisions of one setup run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub provider: String,
    pub model: String,
    pub key: Option<String>,
}

/// Applies `choice` in the spec's order (§4): key, default model, then completion.
pub fn apply(host: &mut dyn SetupHost, choice: &Choice, version: u32) -> Result<(), String> {
    if let Some(key) = choice.key.as_deref().filter(|key| !key.trim().is_empty()) {
        host.store_key(&choice.provider, key.trim())?;
    }
    host.set_default_model(&choice.provider, &choice.model)?;
    host.mark_complete(version, &choice.provider, &choice.model)
}

/// Resolves a model answer (`provider/id` or `id`) against `provider`'s models.
pub fn resolve_model(provider: &str, models: &[String], answer: &str) -> Option<String> {
    let answer = answer.trim();
    let id = answer
        .strip_prefix(provider)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(answer);
    models.iter().find(|model| model.as_str() == id).cloned()
}

/// Resolves a provider answer (id, case-insensitive) against the catalog.
pub fn resolve_provider<'a>(
    providers: &'a [ProviderChoice],
    answer: &str,
) -> Option<&'a ProviderChoice> {
    let answer = answer.trim();
    providers
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(answer))
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    /// A recording host for tests.
    #[derive(Default)]
    pub struct FakeHost {
        pub catalog: Vec<(ProviderChoice, Vec<String>)>,
        pub log: Vec<String>,
    }

    impl FakeHost {
        pub fn sample() -> Self {
            let provider = |id: &str, cred| ProviderChoice {
                id: id.into(),
                name: id.to_uppercase(),
                has_credential: cred,
            };
            Self {
                catalog: vec![
                    (provider("alpha", true), vec!["a1".into(), "a2".into()]),
                    (provider("beta", false), vec!["b1".into()]),
                ],
                log: Vec::new(),
            }
        }
    }

    impl SetupHost for FakeHost {
        fn providers(&self) -> Vec<ProviderChoice> {
            self.catalog.iter().map(|(p, _)| p.clone()).collect()
        }
        fn models(&self, provider: &str) -> Vec<String> {
            self.catalog
                .iter()
                .find(|(p, _)| p.id == provider)
                .map(|(_, m)| m.clone())
                .unwrap_or_default()
        }
        fn store_key(&mut self, provider: &str, key: &str) -> Result<(), String> {
            self.log.push(format!("key {provider} {key}"));
            Ok(())
        }
        fn set_default_model(&mut self, provider: &str, model: &str) -> Result<(), String> {
            self.log.push(format!("model {provider}/{model}"));
            Ok(())
        }
        fn mark_complete(&mut self, version: u32, provider: &str, model: &str) -> Result<(), String> {
            self.log.push(format!("done {version} {provider}/{model}"));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeHost;
    use super::*;

    #[test]
    fn apply_writes_key_model_then_completion() {
        let mut host = FakeHost::sample();
        apply(
            &mut host,
            &Choice {
                provider: "beta".into(),
                model: "b1".into(),
                key: Some(" sk-1 ".into()),
            },
            1,
        )
        .expect("apply");
        assert_eq!(host.log, ["key beta sk-1", "model beta/b1", "done 1 beta/b1"]);
    }

    #[test]
    fn model_and_provider_answers() {
        let models = vec!["m1".to_owned(), "m2".to_owned()];
        assert_eq!(resolve_model("p", &models, "p/m2"), Some("m2".into()));
        assert_eq!(resolve_model("p", &models, " m1 "), Some("m1".into()));
        assert_eq!(resolve_model("p", &models, "q/m1"), None);
        let host = FakeHost::sample();
        let providers = host.providers();
        assert_eq!(
            resolve_provider(&providers, "BETA").map(|p| p.id.as_str()),
            Some("beta")
        );
        assert!(resolve_provider(&providers, "gamma").is_none());
    }
}
