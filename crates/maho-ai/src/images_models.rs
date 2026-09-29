//! Port of senpi packages/ai/src/images-models.ts. Provider auth goes through
//! `ImagesProvider::resolve_auth` (the `ProviderAuth` of auth/, todo 13).

use crate::images::images_error;
use crate::models::{AuthResolution, AuthResolutionOverrides, ModelsError, ModelsErrorCode};
use crate::types::{AssistantImages, BoxFuture, ImagesContext, ImagesModel, ImagesOptions, ProviderImages};
use futures::FutureExt;
use futures::future::{BoxFuture as SharedFuture, Shared};
use indexmap::IndexMap;
use std::sync::{Arc, Mutex, RwLock};

pub trait ImagesProvider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn get_models(&self) -> Vec<ImagesModel>;
    fn supports_refresh(&self) -> bool {
        false
    }
    fn refresh_models(&self) -> BoxFuture<'_, Result<(), ModelsError>> {
        Box::pin(async { Ok(()) })
    }
    fn resolve_auth<'a>(
        &'a self,
        overrides: &'a AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>>;
    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, AssistantImages>;
}

#[derive(Default)]
pub struct ImagesModels {
    providers: RwLock<IndexMap<String, Arc<dyn ImagesProvider>>>,
}

pub fn create_images_models() -> ImagesModels {
    ImagesModels::default()
}

impl ImagesModels {
    pub fn set_provider(&self, provider: Arc<dyn ImagesProvider>) {
        self.providers.write().unwrap_or_else(|p| p.into_inner()).insert(provider.id().to_owned(), provider);
    }

    pub fn delete_provider(&self, id: &str) {
        self.providers.write().unwrap_or_else(|p| p.into_inner()).shift_remove(id);
    }

    pub fn clear_providers(&self) {
        self.providers.write().unwrap_or_else(|p| p.into_inner()).clear();
    }

    pub fn get_providers(&self) -> Vec<Arc<dyn ImagesProvider>> {
        self.providers.read().unwrap_or_else(|p| p.into_inner()).values().cloned().collect()
    }

    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn ImagesProvider>> {
        self.providers.read().unwrap_or_else(|p| p.into_inner()).get(id).cloned()
    }

    pub fn get_models(&self, provider: Option<&str>) -> Vec<ImagesModel> {
        match provider {
            Some(id) => self.get_provider(id).map(|p| p.get_models()).unwrap_or_default(),
            None => self.get_providers().iter().flat_map(|p| p.get_models()).collect(),
        }
    }

    pub fn get_model(&self, provider: &str, id: &str) -> Option<ImagesModel> {
        self.get_models(Some(provider)).into_iter().find(|model| model.id == id)
    }

    /// One provider: errors propagate as `model_source`. All providers: settled, errors dropped.
    pub async fn refresh(&self, provider: Option<&str>) -> Result<(), ModelsError> {
        if let Some(id) = provider {
            let Some(entry) = self.get_provider(id).filter(|p| p.supports_refresh()) else { return Ok(()) };
            return entry.refresh_models().await.map_err(|error| match error.code {
                ModelsErrorCode::ModelSource => error,
                _ => ModelsError::with_cause(ModelsErrorCode::ModelSource, format!("Model refresh failed for {id}"), &error.message),
            });
        }
        let providers = self.get_providers();
        futures::future::join_all(providers.iter().map(|p| p.refresh_models())).await;
        Ok(())
    }

    pub async fn get_auth(&self, provider_id: &str, overrides: &AuthResolutionOverrides) -> Result<Option<AuthResolution>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else { return Ok(None) };
        provider.resolve_auth(overrides).await
    }

    /// Never fails: setup errors become an `AssistantImages` with `stopReason: "error"`.
    pub async fn generate_images(&self, model: &ImagesModel, context: &ImagesContext, options: Option<ImagesOptions>) -> AssistantImages {
        let Some(provider) = self.get_provider(&model.provider) else {
            return images_error(model, format!("Unknown provider: {}", model.provider));
        };
        let request = options.as_ref().map(|o| o.request.clone()).unwrap_or_default();
        let overrides = AuthResolutionOverrides {
            api_key: request.api_key.clone(),
            env: request.env.clone(),
            signal: request.signal.clone(),
            ..AuthResolutionOverrides::default()
        };
        let resolution = match provider.resolve_auth(&overrides).await {
            Ok(resolution) => resolution,
            Err(error) => return images_error(model, error.message),
        };
        let Some(resolution) = resolution else {
            return provider.generate_images(model, context, options).await;
        };
        let mut request_model = model.clone();
        if let Some(base_url) = &resolution.auth.base_url {
            request_model.base_url = base_url.clone();
        }
        let mut options = options.unwrap_or_default();
        options.request.api_key = request.api_key.clone().or(resolution.auth.api_key);
        if resolution.auth.headers.is_some() || request.headers.is_some() {
            let mut headers = resolution.auth.headers.unwrap_or_default();
            headers.extend(request.headers.unwrap_or_default());
            options.request.headers = Some(headers);
        }
        if resolution.env.is_some() || request.env.is_some() {
            let mut env = resolution.env.unwrap_or_default();
            env.extend(request.env.unwrap_or_default());
            options.request.env = Some(env);
        }
        provider.generate_images(&request_model, context, Some(options)).await
    }
}

pub type ResolveImagesAuth =
    Arc<dyn Fn(&AuthResolutionOverrides) -> BoxFuture<'static, Result<Option<AuthResolution>, ModelsError>> + Send + Sync>;
pub type RefreshImagesModels = Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<ImagesModel>, ModelsError>> + Send + Sync>;

pub struct CreateImagesProviderOptions {
    pub id: String,
    pub name: Option<String>,
    pub auth: ResolveImagesAuth,
    pub models: Vec<ImagesModel>,
    pub refresh_models: Option<RefreshImagesModels>,
    pub api: Arc<dyn ProviderImages>,
}

type InflightRefresh = Shared<SharedFuture<'static, Result<(), ModelsError>>>;

struct CreatedImagesProvider {
    input: CreateImagesProviderOptions,
    name: String,
    models: Arc<RwLock<Vec<ImagesModel>>>,
    inflight: Arc<Mutex<Option<InflightRefresh>>>,
}

impl ImagesProvider for CreatedImagesProvider {
    fn id(&self) -> &str {
        &self.input.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn get_models(&self) -> Vec<ImagesModel> {
        self.models.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn supports_refresh(&self) -> bool {
        self.input.refresh_models.is_some()
    }

    /// Concurrent refreshes share one in-flight fetch.
    fn refresh_models(&self) -> BoxFuture<'_, Result<(), ModelsError>> {
        let Some(refresh) = self.input.refresh_models.clone() else { return Box::pin(async { Ok(()) }) };
        let shared = {
            let mut inflight = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
            inflight
                .get_or_insert_with(|| {
                    let (models, slot) = (self.models.clone(), self.inflight.clone());
                    let run: SharedFuture<'static, Result<(), ModelsError>> = Box::pin(async move {
                        let outcome = refresh().await;
                        *slot.lock().unwrap_or_else(|p| p.into_inner()) = None;
                        *models.write().unwrap_or_else(|p| p.into_inner()) = outcome?;
                        Ok(())
                    });
                    run.shared()
                })
                .clone()
        };
        Box::pin(shared)
    }

    fn resolve_auth<'a>(&'a self, overrides: &'a AuthResolutionOverrides) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
        (self.input.auth)(overrides)
    }

    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, AssistantImages> {
        self.input.api.generate_images(model, context, options)
    }
}

pub fn create_images_provider(input: CreateImagesProviderOptions) -> Arc<dyn ImagesProvider> {
    let name = input.name.clone().unwrap_or_else(|| input.id.clone());
    let models = Arc::new(RwLock::new(input.models.clone()));
    Arc::new(CreatedImagesProvider { input, name, models, inflight: Arc::new(Mutex::new(None)) })
}

#[cfg(test)]
mod tests;
