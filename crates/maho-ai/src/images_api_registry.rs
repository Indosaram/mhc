//! Port of senpi packages/ai/src/images-api-registry.ts.

use crate::node::provider_scope::{ProviderScopeError, checked_active_scope};
use crate::types::{AssistantImages, BoxFuture, ImagesApi, ImagesContext, ImagesModel, ImagesOptions, ProviderImages};
use indexmap::IndexMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Mismatched api: {actual} expected {expected}")]
pub struct MismatchedImagesApi {
    pub actual: String,
    pub expected: String,
}

#[derive(Clone)]
pub struct ImagesApiProviderInternal {
    pub api: ImagesApi,
    images: Arc<dyn ProviderImages>,
}

impl std::fmt::Debug for ImagesApiProviderInternal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagesApiProviderInternal").field("api", &self.api).finish()
    }
}

impl ImagesApiProviderInternal {
    pub fn new(api: impl Into<ImagesApi>, images: Arc<dyn ProviderImages>) -> Self {
        Self { api: api.into(), images }
    }

    pub fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> Result<BoxFuture<'a, AssistantImages>, MismatchedImagesApi> {
        if model.api != self.api {
            return Err(MismatchedImagesApi { actual: model.api.clone(), expected: self.api.clone() });
        }
        Ok(self.images.generate_images(model, context, options))
    }
}

#[derive(Clone, Debug)]
pub struct RegisteredImagesApiProvider {
    pub provider: ImagesApiProviderInternal,
    pub source_id: Option<String>,
}

type Registry = IndexMap<String, RegisteredImagesApiProvider>;

static IMAGES_API_PROVIDER_REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(IndexMap::new()));
static BUILTIN_IMAGES_API_PROVIDER_REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

fn lock(registry: &'static LazyLock<Mutex<Registry>>) -> MutexGuard<'static, Registry> {
    registry.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn register_images_api_provider(
    api: &str,
    images: Arc<dyn ProviderImages>,
    source_id: Option<&str>,
) -> Result<(), ProviderScopeError> {
    let registered = RegisteredImagesApiProvider {
        provider: ImagesApiProviderInternal::new(api, images),
        source_id: source_id.map(str::to_owned),
    };
    match checked_active_scope()? {
        Some(scope) => scope.with_images_overlay(|overlay| overlay.insert(api.to_owned(), registered)),
        None => lock(&IMAGES_API_PROVIDER_REGISTRY).insert(api.to_owned(), registered),
    };
    Ok(())
}

pub fn register_builtin_images_api_provider(api: &str, images: Arc<dyn ProviderImages>) {
    let registered = lock(&BUILTIN_IMAGES_API_PROVIDER_REGISTRY)
        .entry(api.to_owned())
        .or_insert_with(|| RegisteredImagesApiProvider { provider: ImagesApiProviderInternal::new(api, images), source_id: None })
        .clone();
    lock(&IMAGES_API_PROVIDER_REGISTRY).entry(api.to_owned()).or_insert(registered);
}

pub fn get_images_api_provider(api: &str) -> Result<Option<ImagesApiProviderInternal>, ProviderScopeError> {
    if let Some(scope) = checked_active_scope()? {
        let scoped = scope.with_images_overlay(|overlay| overlay.get(api).map(|entry| entry.provider.clone()));
        return Ok(scoped.or_else(|| lock(&BUILTIN_IMAGES_API_PROVIDER_REGISTRY).get(api).map(|e| e.provider.clone())));
    }
    Ok(lock(&IMAGES_API_PROVIDER_REGISTRY).get(api).map(|entry| entry.provider.clone()))
}

pub fn get_images_api_providers() -> Result<Vec<ImagesApiProviderInternal>, ProviderScopeError> {
    if let Some(scope) = checked_active_scope()? {
        let mut providers = lock(&BUILTIN_IMAGES_API_PROVIDER_REGISTRY).clone();
        scope.with_images_overlay(|overlay| {
            for (api, entry) in overlay.iter() {
                providers.insert(api.clone(), entry.clone());
            }
        });
        return Ok(providers.into_values().map(|entry| entry.provider).collect());
    }
    Ok(lock(&IMAGES_API_PROVIDER_REGISTRY).values().map(|entry| entry.provider.clone()).collect())
}

pub fn unregister_images_api_providers(source_id: &str) -> Result<(), ProviderScopeError> {
    let matches = |entry: &RegisteredImagesApiProvider| entry.source_id.as_deref() == Some(source_id);
    match checked_active_scope()? {
        Some(scope) => scope.with_images_overlay(|overlay| overlay.retain(|_, entry| !matches(entry))),
        None => lock(&IMAGES_API_PROVIDER_REGISTRY).retain(|_, entry| !matches(entry)),
    }
    Ok(())
}

pub fn clear_images_api_providers() -> Result<(), ProviderScopeError> {
    match checked_active_scope()? {
        Some(scope) => scope.with_images_overlay(std::collections::HashMap::clear),
        None => lock(&IMAGES_API_PROVIDER_REGISTRY).clear(),
    }
    Ok(())
}

/// Restores the registry to the shipped builtins (or clears the active scope's overlay).
pub fn reset_images_api_providers() -> Result<(), ProviderScopeError> {
    if let Some(scope) = checked_active_scope()? {
        scope.with_images_overlay(std::collections::HashMap::clear);
        return Ok(());
    }
    let builtins = lock(&BUILTIN_IMAGES_API_PROVIDER_REGISTRY).clone();
    let mut registry = lock(&IMAGES_API_PROVIDER_REGISTRY);
    registry.clear();
    registry.extend(builtins);
    Ok(())
}
