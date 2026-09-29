//! Port of senpi packages/ai/src/api-registry.ts.

use crate::node::provider_scope::{ProviderScopeError, checked_active_scope};
use crate::types::{
    Api, AssistantMessageEventStream, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions,
};
use indexmap::IndexMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, RwLock};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiRegistryError {
    #[error(transparent)]
    Scope(#[from] ProviderScopeError),
    #[error("No API provider registered for api: {0}")]
    NoProvider(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Mismatched api: {actual} expected {expected}")]
pub struct MismatchedApi {
    pub actual: String,
    pub expected: String,
}

/// `ApiProviderInternal`: streams wrapped so a model of another API is rejected.
#[derive(Clone)]
pub struct ApiProviderInternal {
    pub api: Api,
    streams: Arc<dyn ProviderStreams>,
}

impl std::fmt::Debug for ApiProviderInternal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiProviderInternal").field("api", &self.api).finish()
    }
}

impl ApiProviderInternal {
    pub fn new(api: impl Into<Api>, streams: Arc<dyn ProviderStreams>) -> Self {
        Self { api: api.into(), streams }
    }

    fn check(&self, model: &Model) -> Result<(), MismatchedApi> {
        if model.api == self.api {
            Ok(())
        } else {
            Err(MismatchedApi { actual: model.api.clone(), expected: self.api.clone() })
        }
    }

    pub fn try_stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> Result<AssistantMessageEventStream, MismatchedApi> {
        self.check(model)?;
        Ok(self.streams.stream(model, context, options))
    }

    pub fn try_stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, MismatchedApi> {
        self.check(model)?;
        Ok(self.streams.stream_simple(model, context, options))
    }

    /// Streams, turning an API mismatch into an error-terminated stream.
    pub fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        self.try_stream(model, context, options)
            .unwrap_or_else(|error| crate::utils::lazy::error_stream(model, &error.to_string()))
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.try_stream_simple(model, context, options)
            .unwrap_or_else(|error| crate::utils::lazy::error_stream(model, &error.to_string()))
    }

    pub fn streams(&self) -> &Arc<dyn ProviderStreams> {
        &self.streams
    }
}

#[derive(Clone, Debug)]
pub struct RegisteredApiProvider {
    pub provider: ApiProviderInternal,
    pub source_id: Option<String>,
}

type Registry = IndexMap<String, RegisteredApiProvider>;

static API_PROVIDER_REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(IndexMap::new()));
static BUILTIN_API_PROVIDER_REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

/// Hook for the faux test provider (providers/faux, todo 13), consulted before the registry.
pub type FauxProviderLookup = fn(&str) -> Option<ApiProviderInternal>;
static FAUX_LOOKUP: RwLock<Option<FauxProviderLookup>> = RwLock::new(None);

pub fn install_faux_provider_lookup(lookup: Option<FauxProviderLookup>) {
    *FAUX_LOOKUP.write().unwrap_or_else(|p| p.into_inner()) = lookup;
}

fn lock(registry: &'static LazyLock<Mutex<Registry>>) -> MutexGuard<'static, Registry> {
    registry.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn register_api_provider(api: &str, streams: Arc<dyn ProviderStreams>, source_id: Option<&str>) -> Result<(), ApiRegistryError> {
    let registered = RegisteredApiProvider {
        provider: ApiProviderInternal::new(api, streams),
        source_id: source_id.map(str::to_owned),
    };
    match checked_active_scope()? {
        Some(scope) => scope.with_overlay(|overlay| overlay.insert(api.to_owned(), registered)),
        None => lock(&API_PROVIDER_REGISTRY).insert(api.to_owned(), registered),
    };
    Ok(())
}

/// Registers a shipped API; an existing builtin registration for the API wins.
pub fn register_builtin_api_provider(api: &str, streams: Arc<dyn ProviderStreams>) {
    let registered = lock(&BUILTIN_API_PROVIDER_REGISTRY)
        .entry(api.to_owned())
        .or_insert_with(|| RegisteredApiProvider { provider: ApiProviderInternal::new(api, streams), source_id: None })
        .clone();
    lock(&API_PROVIDER_REGISTRY).entry(api.to_owned()).or_insert(registered);
}

pub fn get_builtin_api_provider(api: &str) -> Option<ApiProviderInternal> {
    lock(&BUILTIN_API_PROVIDER_REGISTRY).get(api).map(|entry| entry.provider.clone())
}

pub fn get_api_provider(api: &str) -> Result<Option<ApiProviderInternal>, ApiRegistryError> {
    if let Some(scope) = checked_active_scope()? {
        let scoped = scope.with_overlay(|overlay| overlay.get(api).map(|entry| entry.provider.clone()));
        return Ok(scoped.or_else(|| get_builtin_api_provider(api)));
    }
    let faux = *FAUX_LOOKUP.read().unwrap_or_else(|p| p.into_inner());
    if let Some(provider) = faux.and_then(|lookup| lookup(api)) {
        return Ok(Some(provider));
    }
    Ok(lock(&API_PROVIDER_REGISTRY).get(api).map(|entry| entry.provider.clone()))
}

pub fn get_api_providers() -> Result<Vec<ApiProviderInternal>, ApiRegistryError> {
    if let Some(scope) = checked_active_scope()? {
        let mut providers = lock(&BUILTIN_API_PROVIDER_REGISTRY).clone();
        scope.with_overlay(|overlay| {
            for (api, entry) in overlay.iter() {
                providers.insert(api.clone(), entry.clone());
            }
        });
        return Ok(providers.into_values().map(|entry| entry.provider).collect());
    }
    Ok(lock(&API_PROVIDER_REGISTRY).values().map(|entry| entry.provider.clone()).collect())
}

pub fn unregister_api_providers(source_id: &str) -> Result<(), ApiRegistryError> {
    let matches = |entry: &RegisteredApiProvider| entry.source_id.as_deref() == Some(source_id);
    match checked_active_scope()? {
        Some(scope) => scope.with_overlay(|overlay| overlay.retain(|_, entry| !matches(entry))),
        None => lock(&API_PROVIDER_REGISTRY).retain(|_, entry| !matches(entry)),
    }
    Ok(())
}

pub fn clear_api_providers() -> Result<(), ApiRegistryError> {
    match checked_active_scope()? {
        Some(scope) => scope.with_overlay(std::collections::HashMap::clear),
        None => lock(&API_PROVIDER_REGISTRY).clear(),
    }
    Ok(())
}
