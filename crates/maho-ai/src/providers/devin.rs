//! Port of senpi packages/ai/src/providers/devin.ts.

use crate::api::devin_agent::discovery::{DevinDiscoveryOptions, fetch_devin_models};
use crate::api::devin_agent::paths::DEVIN_DEFAULT_BASE_URL;
use crate::models::{
    CreateProviderOptions, Credential, ModelsError, ModelsPublication, Provider, ProviderApi, RefreshModelsContext,
    create_provider,
};
use crate::models_store::ModelsStoreEntry;
use crate::providers::builtin_api_streams;
use crate::types::{
    AssistantMessageEventStream, BoxFuture, Context, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, Model,
    ProviderHeaders, SimpleStreamOptions, StreamOptions,
};
use crate::utils::diagnostics::now_ms;
use indexmap::IndexMap;
use serde_json::Value;
use std::sync::{Arc, RwLock};

/// Devin (Cognition) Cascade provider.
///
/// Cascade's catalog is credential-scoped, so the static seed is a floor rather than the truth:
/// once a credential exists, `refresh_models` replaces it with the account's real lanes and KEEPS
/// the seed whenever discovery fails or reports nothing, because publishing an empty catalog would
/// strip a working provider.
pub fn devin_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    api.insert("devin-agent".to_owned(), builtin_api_streams("devin-agent"));
    let seed = super::devin_models::devin_models();
    let provider = create_provider(CreateProviderOptions {
        id: "devin".into(),
        name: Some("Devin".into()),
        base_url: Some(DEVIN_DEFAULT_BASE_URL.to_owned()),
        headers: None,
        models: seed.clone(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::ByApi(api),
    });
    Arc::new(DevinProvider { provider, models: Arc::new(RwLock::new(seed)), client: reqwest::Client::new() })
}

/// `{ ...provider, getModels, refreshModels }`: the created provider with its catalog replaced.
struct DevinProvider {
    provider: Arc<dyn Provider>,
    models: Arc<RwLock<Vec<Model>>>,
    client: reqwest::Client,
}

/// `credential?.type === "oauth" ? credential.access : credential?.key`.
fn credential_api_key(credential: Option<&Credential>) -> Option<String> {
    let credential = credential?;
    let field = if credential.get("type").and_then(Value::as_str) == Some("oauth") { "access" } else { "key" };
    credential.get(field).and_then(Value::as_str).filter(|key| !key.is_empty()).map(str::to_owned)
}

impl DevinProvider {
    fn replace_models(&self, next: Vec<Model>) -> Box<dyn FnOnce() + Send> {
        let models = self.models.clone();
        Box::new(move || *models.write().unwrap_or_else(|p| p.into_inner()) = next)
    }
}

impl Provider for DevinProvider {
    fn id(&self) -> &str {
        self.provider.id()
    }

    fn name(&self) -> &str {
        self.provider.name()
    }

    fn base_url(&self) -> Option<&str> {
        self.provider.base_url()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.provider.headers()
    }

    fn retry_policy(&self) -> Option<&crate::utils::retry_profile::types::RetryPolicyProfile> {
        self.provider.retry_policy()
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn supports_refresh(&self) -> bool {
        true
    }

    fn refresh_models<'a>(&'a self, context: RefreshModelsContext) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async move {
            if let Some(stored) = &context.stored {
                // Restore-path normalization: catalogs persisted before the
                // no-advertised-thinking-level fix still carry reasoning: true, which
                // would re-expose the dead thinking-level control until the next
                // successful discovery overwrites them.
                let restored: Vec<Model> = stored
                    .models
                    .iter()
                    .filter(|model| model.provider == "devin")
                    .map(|model| Model { reasoning: false, ..model.clone() })
                    .collect();
                if !restored.is_empty() {
                    let _ = context
                        .publish(ModelsPublication { persist: None, update: Some(self.replace_models(restored)) })
                        .await;
                }
            }
            if !context.allow_network || context.signal.aborted() {
                return Ok(());
            }
            let Some(api_key) = credential_api_key(context.credential.as_ref()) else { return Ok(()) };
            let discovered = fetch_devin_models(&DevinDiscoveryOptions {
                api_key: Some(&api_key),
                base_url: Some(DEVIN_DEFAULT_BASE_URL),
                timeout_ms: None,
                signal: Some(&context.signal),
                client: &self.client,
            })
            .await;
            let Some(discovered) = discovered else { return Ok(()) };
            if context.signal.aborted() {
                return Ok(());
            }
            let entry =
                ModelsStoreEntry { models: discovered.clone(), checked_at: Some(now_ms()), ..ModelsStoreEntry::default() };
            let _ = context
                .publish(ModelsPublication { persist: Some(Some(entry)), update: Some(self.replace_models(discovered)) })
                .await;
            Ok(())
        })
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        self.provider.filter_models(models, credential)
    }

    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        self.provider.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.provider.stream_simple(model, context, options)
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.provider.fetch_deferred(model, handle, options)
    }

    fn cancel_deferred<'a>(
        &'a self,
        model: &'a Model,
        handle: &'a DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.provider.cancel_deferred(model, handle, options)
    }

    fn supports_cancel_deferred(&self) -> bool {
        self.provider.supports_cancel_deferred()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthResolution, AuthResolutionOverrides, CreateModelsOptions, ModelsAuth, ModelsRefreshOptions, create_models};
    use crate::models_store::{InMemoryModelsStore, ModelsStore};
    use crate::utils::abort::AbortSignal;

    struct StoredOnlyAuth;

    impl ModelsAuth for StoredOnlyAuth {
        fn resolve<'a>(
            &'a self,
            _provider: &'a dyn Provider,
            _overrides: &'a AuthResolutionOverrides,
        ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
            Box::pin(async { Ok(None) })
        }

        fn refresh_credential<'a>(
            &'a self,
            _provider: &'a dyn Provider,
            _stored: Option<&'a Credential>,
            _signal: &'a AbortSignal,
        ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[tokio::test]
    async fn normalizes_stale_stored_catalogs_that_still_advertise_a_thinking_level() {
        let seed = super::super::devin_models::devin_models().into_iter().next().expect("devin seed");
        let stale = ModelsStoreEntry { models: vec![Model { reasoning: true, ..seed.clone() }], ..ModelsStoreEntry::default() };
        let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
        store.write("devin", &stale, None).await.expect("seed store");
        let models = create_models(Some(CreateModelsOptions { models_store: Some(store), auth: Some(Arc::new(StoredOnlyAuth)) }));
        models.set_provider(devin_provider());

        let result = models.refresh(ModelsRefreshOptions { allow_network: Some(false), ..ModelsRefreshOptions::default() }).await;
        assert!(result.errors.is_empty());
        let restored: Vec<Model> = models.get_models(Some("devin")).into_iter().filter(|model| model.id == seed.id).collect();
        assert!(!restored.is_empty());
        assert!(restored.iter().all(|model| !model.reasoning));
    }

    #[test]
    fn reads_the_session_token_from_oauth_access_or_api_key() {
        let oauth = serde_json::json!({ "type": "oauth", "access": "tok", "key": "ignored" });
        let api_key = serde_json::json!({ "type": "api_key", "key": "abc" });
        assert_eq!(credential_api_key(Some(&oauth)).as_deref(), Some("tok"));
        assert_eq!(credential_api_key(Some(&api_key)).as_deref(), Some("abc"));
        assert_eq!(credential_api_key(None), None);
    }
}
