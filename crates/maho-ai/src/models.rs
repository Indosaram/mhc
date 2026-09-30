//! Port of senpi packages/ai/src/models.ts.
//!
//! Auth resolution, credential storage and OAuth login live in `auth/` (todo 13); the collection
//! reaches them through the `ModelsAuth` seam. `EnvModelsAuth` is the default and resolves API keys
//! from provider env vars only.

use crate::env_api_keys::get_env_api_key;
use crate::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
use crate::types::{
    AssistantMessage, AssistantMessageEventStream, BoxFuture, Context, DeferredHandle, Model, ModelCostRates,
    ModelThinkingLevel, ProviderEnv, ProviderHeaders, ProviderStreams, SimpleStreamOptions, StreamOptions,
    ThinkingLevelMap, Usage, UsageCost,
};
use crate::utils::abort::{AbortController, AbortReason, AbortSignal, operation_signal, race_with_abort_signal};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::diagnostics::now_ms;
use crate::utils::lazy::error_stream;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

pub const PROVIDER_NOT_CONFIGURED_PREFIX: &str = "Provider is not configured: ";

pub fn provider_not_configured_message(provider_id: &str) -> String {
    format!("{PROVIDER_NOT_CONFIGURED_PREFIX}{provider_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelsErrorCode {
    ModelSource,
    ModelValidation,
    Provider,
    Stream,
    Auth,
    OAuth,
}

impl ModelsErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelsErrorCode::ModelSource => "model_source",
            ModelsErrorCode::ModelValidation => "model_validation",
            ModelsErrorCode::Provider => "provider",
            ModelsErrorCode::Stream => "stream",
            ModelsErrorCode::Auth => "auth",
            ModelsErrorCode::OAuth => "oauth",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ModelsError {
    pub code: ModelsErrorCode,
    pub message: String,
}

impl ModelsError {
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// `new ModelsError(code, message, { cause })`: appends the cause detail unless already present.
    pub fn with_cause(code: ModelsErrorCode, message: impl Into<String>, cause: &str) -> Self {
        let message = message.into();
        let detail = crate::utils::js::trim(cause);
        if detail.is_empty() || message.contains(detail) {
            Self::new(code, message)
        } else {
            Self::new(code, format!("{message}: {detail}"))
        }
    }
}

/// Stored provider credential (`auth/types.ts` `Credential`, owned by todo 13), carried as JSON.
pub type Credential = Value;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderAuthResult {
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthResolution {
    pub auth: ProviderAuthResult,
    pub env: Option<ProviderEnv>,
}

#[derive(Debug, Clone, Default)]
pub struct AuthResolutionOverrides {
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub min_oauth_validity_ms: Option<u64>,
    pub slot_name: Option<String>,
    pub signal: Option<AbortSignal>,
}

/// The auth surface the models collection needs.
pub trait ModelsAuth: Send + Sync {
    fn resolve<'a>(
        &'a self,
        provider: &'a dyn Provider,
        overrides: &'a AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>>;

    fn read_credential<'a>(
        &'a self,
        _provider_id: &'a str,
        _signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async { Ok(None) })
    }

    /// Credential used for a network refresh (`resolveRefreshCredential`).
    fn refresh_credential<'a>(
        &'a self,
        provider: &'a dyn Provider,
        stored: Option<&'a Credential>,
        signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>>;
}

/// Env-var-only auth: an explicit API key override wins, then the provider's known env vars.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvModelsAuth;

impl ModelsAuth for EnvModelsAuth {
    fn resolve<'a>(
        &'a self,
        provider: &'a dyn Provider,
        overrides: &'a AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
        Box::pin(async move {
            let api_key = overrides.api_key.clone().or_else(|| get_env_api_key(provider.id(), overrides.env.as_ref()));
            Ok(api_key.map(|api_key| AuthResolution {
                auth: ProviderAuthResult { api_key: Some(api_key), ..ProviderAuthResult::default() },
                env: overrides.env.clone(),
            }))
        })
    }

    fn refresh_credential<'a>(
        &'a self,
        provider: &'a dyn Provider,
        _stored: Option<&'a Credential>,
        _signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async move {
            Ok(get_env_api_key(provider.id(), None).map(|key| serde_json::json!({ "type": "api_key", "key": key })))
        })
    }
}

/// `ModelsPublication`: `persist: None` leaves the store untouched, `Some(None)` deletes the entry.
#[derive(Default)]
pub struct ModelsPublication {
    pub persist: Option<Option<ModelsStoreEntry>>,
    pub update: Option<Box<dyn FnOnce() + Send>>,
}

type PublishFn = Arc<dyn Fn(ModelsPublication) -> BoxFuture<'static, Result<bool, AbortReason>> + Send + Sync>;

pub struct RefreshModelsContext {
    pub credential: Option<Credential>,
    pub stored: Option<ModelsStoreEntry>,
    publish: PublishFn,
    pub allow_network: bool,
    pub force: Option<bool>,
    pub signal: AbortSignal,
}

impl RefreshModelsContext {
    /// Persists and applies a model update unless the refresh was superseded or aborted.
    pub async fn publish(&self, publication: ModelsPublication) -> Result<bool, AbortReason> {
        (self.publish)(publication).await
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelsRefreshOptions {
    pub allow_network: Option<bool>,
    pub providers: Option<Vec<String>>,
    pub force: Option<bool>,
    pub signal: Option<AbortSignal>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelsRefreshResult {
    pub aborted: bool,
    pub errors: IndexMap<String, ModelsError>,
}

pub type TransformHeaders = Arc<dyn Fn(ProviderHeaders) -> BoxFuture<'static, ProviderHeaders> + Send + Sync>;

#[derive(Clone, Default)]
pub struct ModelsRequestTransforms {
    pub transform_headers: Option<TransformHeaders>,
}

/// A provider held by a `Models` collection.
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn base_url(&self) -> Option<&str> {
        None
    }
    fn headers(&self) -> Option<&ProviderHeaders> {
        None
    }
    fn get_models(&self) -> Vec<Model>;
    fn supports_refresh(&self) -> bool {
        false
    }
    fn refresh_models<'a>(&'a self, _context: RefreshModelsContext) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async { Ok(()) })
    }
    fn filter_models(&self, models: Vec<Model>, _credential: Option<&Credential>) -> Vec<Model> {
        models
    }
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream;
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream;
    fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: Option<crate::types::DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        None
    }
}

pub type FetchModels = Arc<dyn Fn(&RefreshModelsContext) -> BoxFuture<'static, Result<Vec<Model>, ModelsError>> + Send + Sync>;
pub type RestoreModels = Arc<dyn Fn(Vec<Model>) -> Result<Vec<Model>, ModelsError> + Send + Sync>;
pub type FilterModels = Arc<dyn Fn(Vec<Model>, Option<&Credential>) -> Vec<Model> + Send + Sync>;

/// `ProviderStreams | Partial<Record<TApi, ProviderStreams>>`.
#[derive(Clone)]
pub enum ProviderApi {
    Single(Arc<dyn ProviderStreams>),
    ByApi(IndexMap<String, Arc<dyn ProviderStreams>>),
}

#[derive(Clone)]
pub struct CreateProviderOptions {
    pub id: String,
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub models: Vec<Model>,
    pub fetch_models: Option<FetchModels>,
    pub restore_models: Option<RestoreModels>,
    pub filter_models: Option<FilterModels>,
    pub api: ProviderApi,
}

struct CreatedProvider {
    input: CreateProviderOptions,
    name: String,
    dynamic_models: Arc<RwLock<Vec<Model>>>,
}

impl CreatedProvider {
    fn api_for(&self, model: &Model) -> Option<&Arc<dyn ProviderStreams>> {
        match &self.input.api {
            ProviderApi::Single(streams) => Some(streams),
            ProviderApi::ByApi(by_api) => by_api.get(&model.api),
        }
    }

    fn no_api(&self, model: &Model) -> AssistantMessageEventStream {
        error_stream(model, &format!("Provider {} has no API implementation for \"{}\"", self.input.id, model.api))
    }
}

impl Provider for CreatedProvider {
    fn id(&self) -> &str {
        &self.input.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.input.base_url.as_deref()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.input.headers.as_ref()
    }

    fn get_models(&self) -> Vec<Model> {
        let mut merged = self.input.models.clone();
        for model in self.dynamic_models.read().unwrap_or_else(|p| p.into_inner()).iter() {
            match merged.iter_mut().find(|entry| entry.id == model.id) {
                Some(slot) => *slot = model.clone(),
                None => merged.push(model.clone()),
            }
        }
        merged
    }

    fn supports_refresh(&self) -> bool {
        self.input.fetch_models.is_some()
    }

    fn refresh_models<'a>(&'a self, context: RefreshModelsContext) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async move {
            let Some(fetch_models) = self.input.fetch_models.clone() else { return Ok(()) };
            if let Some(stored) = &context.stored {
                let mut restored: Vec<Model> =
                    stored.models.iter().filter(|model| model.provider == self.input.id).cloned().collect();
                if let Some(restore) = &self.input.restore_models {
                    restored = restore(restored.clone()).unwrap_or_default();
                }
                let dynamic = self.dynamic_models.clone();
                let published = context
                    .publish(ModelsPublication {
                        persist: None,
                        update: Some(Box::new(move || {
                            *dynamic.write().unwrap_or_else(|p| p.into_inner()) = restored;
                        })),
                    })
                    .await
                    .unwrap_or(false);
                if !published {
                    return Ok(());
                }
            }
            if !context.allow_network || context.signal.aborted() {
                return Ok(());
            }
            let refreshed = fetch_models(&context).await?;
            if context.signal.aborted() || refreshed.is_empty() {
                return Ok(());
            }
            let dynamic = self.dynamic_models.clone();
            let entry = ModelsStoreEntry { models: refreshed.clone(), checked_at: Some(now_ms()), ..ModelsStoreEntry::default() };
            let _ = context
                .publish(ModelsPublication {
                    persist: Some(Some(entry)),
                    update: Some(Box::new(move || {
                        *dynamic.write().unwrap_or_else(|p| p.into_inner()) = refreshed;
                    })),
                })
                .await;
            Ok(())
        })
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        match &self.input.filter_models {
            Some(filter) => filter(models, credential),
            None => models,
        }
    }

    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        match self.api_for(model) {
            Some(streams) => streams.stream(model, context, options),
            None => self.no_api(model),
        }
    }

    fn stream_simple(&self, model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
        match self.api_for(model) {
            Some(streams) => streams.stream_simple(model, context, options),
            None => self.no_api(model),
        }
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<crate::types::DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        let streams = self.api_for(model)?;
        if !streams.supports_deferred() {
            return Some(error_stream(model, &format!("Provider {} does not support deferred responses", self.input.id)));
        }
        streams.fetch_deferred(model, handle, options)
    }
}

pub fn create_provider(input: CreateProviderOptions) -> Arc<dyn Provider> {
    let name = input.name.clone().unwrap_or_else(|| input.id.clone());
    Arc::new(CreatedProvider { input, name, dynamic_models: Arc::new(RwLock::new(Vec::new())) })
}

#[derive(Clone, Default)]
pub struct CreateModelsOptions {
    pub models_store: Option<Arc<dyn ModelsStore>>,
    pub auth: Option<Arc<dyn ModelsAuth>>,
}

#[derive(Default)]
struct RefreshState {
    generations: HashMap<String, u64>,
    controllers: HashMap<String, (u64, AbortController)>,
}

struct ModelsInner {
    providers: RwLock<IndexMap<String, Arc<dyn Provider>>>,
    models_store: Arc<dyn ModelsStore>,
    auth: Arc<dyn ModelsAuth>,
    refresh: Mutex<RefreshState>,
    publication_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// `MutableModels`: a provider collection with model lookup, refresh and authenticated streaming.
#[derive(Clone)]
pub struct Models {
    inner: Arc<ModelsInner>,
}

pub fn create_models(options: Option<CreateModelsOptions>) -> Models {
    let options = options.unwrap_or_default();
    Models {
        inner: Arc::new(ModelsInner {
            providers: RwLock::new(IndexMap::new()),
            models_store: options.models_store.unwrap_or_else(|| Arc::new(InMemoryModelsStore::new())),
            auth: options.auth.unwrap_or_else(|| Arc::new(EnvModelsAuth)),
            refresh: Mutex::new(RefreshState::default()),
            publication_locks: Mutex::new(HashMap::new()),
        }),
    }
}

fn merge_headers(base: Option<&ProviderHeaders>, overrides: Option<&ProviderHeaders>) -> Option<ProviderHeaders> {
    if base.is_none() && overrides.is_none() {
        return None;
    }
    let mut merged = base.cloned().unwrap_or_default();
    for (name, value) in overrides.into_iter().flatten() {
        let lower = name.to_lowercase();
        merged.retain(|existing, _| existing.to_lowercase() != lower);
        merged.insert(name.clone(), value.clone());
    }
    Some(merged)
}

impl Models {
    fn refresh_state(&self) -> MutexGuard<'_, RefreshState> {
        self.inner.refresh.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn set_provider(&self, provider: Arc<dyn Provider>) {
        self.supersede_provider_refresh(provider.id());
        self.inner.providers.write().unwrap_or_else(|p| p.into_inner()).insert(provider.id().to_owned(), provider);
    }

    pub fn delete_provider(&self, id: &str) {
        self.supersede_provider_refresh(id);
        self.inner.providers.write().unwrap_or_else(|p| p.into_inner()).shift_remove(id);
    }

    pub fn clear_providers(&self) {
        let mut ids: Vec<String> = self.inner.providers.read().unwrap_or_else(|p| p.into_inner()).keys().cloned().collect();
        ids.extend(self.refresh_state().controllers.keys().cloned());
        for id in ids {
            self.supersede_provider_refresh(&id);
        }
        self.inner.providers.write().unwrap_or_else(|p| p.into_inner()).clear();
    }

    pub fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
        self.inner.providers.read().unwrap_or_else(|p| p.into_inner()).values().cloned().collect()
    }

    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.inner.providers.read().unwrap_or_else(|p| p.into_inner()).get(id).cloned()
    }

    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        match provider {
            Some(id) => self.get_provider(id).map(|p| p.get_models()).unwrap_or_default(),
            None => self.get_providers().iter().flat_map(|p| p.get_models()).collect(),
        }
    }

    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.get_models(Some(provider)).into_iter().find(|model| model.id == id)
    }

    fn supersede_provider_refresh(&self, provider_id: &str) -> u64 {
        let mut state = self.refresh_state();
        let generation = state.generations.get(provider_id).copied().unwrap_or(0) + 1;
        state.generations.insert(provider_id.to_owned(), generation);
        if let Some((_, previous)) = state.controllers.remove(provider_id) {
            drop(state);
            previous.abort(None);
        }
        generation
    }

    fn begin_provider_refresh(&self, provider_id: &str) -> (u64, AbortController) {
        let generation = self.supersede_provider_refresh(provider_id);
        let controller = AbortController::new();
        self.refresh_state().controllers.insert(provider_id.to_owned(), (generation, controller.clone()));
        (generation, controller)
    }

    fn is_current(&self, provider_id: &str, generation: u64) -> bool {
        self.refresh_state().generations.get(provider_id) == Some(&generation)
    }

    fn publisher(&self, provider_id: &str, generation: u64, signal: AbortSignal) -> PublishFn {
        let models = self.clone();
        let provider_id = provider_id.to_owned();
        Arc::new(move |publication: ModelsPublication| {
            let models = models.clone();
            let provider_id = provider_id.clone();
            let signal = signal.clone();
            Box::pin(async move {
                let lock = models
                    .inner
                    .publication_locks
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .entry(provider_id.clone())
                    .or_default()
                    .clone();
                let queued = async {
                    let _turn = lock.lock().await;
                    if signal.aborted() || !models.is_current(&provider_id, generation) {
                        return Ok(false);
                    }
                    let options = ModelsStoreOperationOptions { signal: Some(signal.clone()) };
                    match publication.persist {
                        Some(None) => models.inner.models_store.delete(&provider_id, Some(&options)).await?,
                        Some(Some(entry)) => models.inner.models_store.write(&provider_id, &entry, Some(&options)).await?,
                        None => {}
                    }
                    if signal.aborted() || !models.is_current(&provider_id, generation) {
                        return Ok(false);
                    }
                    if let Some(update) = publication.update {
                        update();
                    }
                    Ok(true)
                };
                race_with_abort_signal(queued, &signal).await?
            })
        })
    }

    async fn run_refresh_phase(
        &self,
        provider: &Arc<dyn Provider>,
        credential: Option<Credential>,
        allow_network: bool,
        force: Option<bool>,
        generation: u64,
        signal: &AbortSignal,
    ) -> Result<(), ModelsError> {
        let options = ModelsStoreOperationOptions { signal: Some(signal.clone()) };
        let stored = self
            .inner
            .models_store
            .read(provider.id(), Some(&options))
            .await
            .map_err(|reason| ModelsError::new(ModelsErrorCode::ModelSource, reason.message))?;
        provider
            .refresh_models(RefreshModelsContext {
                credential,
                stored,
                publish: self.publisher(provider.id(), generation, signal.clone()),
                allow_network,
                force: if allow_network { force } else { None },
                signal: signal.clone(),
            })
            .await
    }

    async fn refresh_one(&self, provider: Arc<dyn Provider>, options: &ModelsRefreshOptions, caller: &AbortSignal) -> Option<ModelsError> {
        let (generation, controller) = self.begin_provider_refresh(provider.id());
        let combined = combine_abort_signals(&[Some(caller.clone()), Some(controller.signal())]);
        let signal = combined.signal.clone().unwrap_or_else(|| controller.signal());
        let allow_network = options.allow_network.unwrap_or(true);
        let operation = async {
            let (stored, credential_error) = match self.inner.auth.read_credential(provider.id(), &signal).await {
                Ok(credential) => (credential, None),
                Err(error) => (None, Some(error)),
            };
            self.run_refresh_phase(&provider, stored.clone(), false, None, generation, &signal).await?;
            if let Some(error) = credential_error {
                return Err(error);
            }
            if !allow_network || signal.aborted() {
                return Ok(());
            }
            let Some(credential) = self.inner.auth.refresh_credential(provider.as_ref(), stored.as_ref(), &signal).await? else {
                return Ok(());
            };
            self.run_refresh_phase(&provider, Some(credential), true, options.force, generation, &signal).await
        };
        let outcome = race_with_abort_signal(operation, &signal).await;
        combined.cleanup();
        {
            let mut state = self.refresh_state();
            if state.controllers.get(provider.id()).is_some_and(|(g, _)| *g == generation) {
                state.controllers.remove(provider.id());
            }
        }
        match outcome {
            Ok(Err(error)) if !signal.aborted() => Some(error),
            _ => None,
        }
    }

    pub async fn refresh(&self, options: ModelsRefreshOptions) -> ModelsRefreshResult {
        let caller = operation_signal(options.signal.clone());
        let mut result = ModelsRefreshResult::default();
        if caller.aborted() {
            result.aborted = true;
            return result;
        }
        let refreshable: Vec<Arc<dyn Provider>> = self
            .get_providers()
            .into_iter()
            .filter(|p| p.supports_refresh() && options.providers.as_ref().is_none_or(|ids| ids.iter().any(|id| id == p.id())))
            .collect();
        let runs = refreshable.into_iter().map(|provider| {
            let id = provider.id().to_owned();
            let options = &options;
            let caller = &caller;
            async move { (id, self.refresh_one(provider, options, caller).await) }
        });
        if let Ok(outcomes) = race_with_abort_signal(futures::future::join_all(runs), &caller).await {
            for (id, error) in outcomes {
                if let Some(error) = error {
                    result.errors.insert(id, error);
                }
            }
        }
        result.aborted = caller.aborted();
        result
    }

    /// Models of configured providers, filtered by each provider against its credential.
    pub async fn get_available(&self, provider_id: Option<&str>) -> Vec<Model> {
        let providers: Vec<Arc<dyn Provider>> = match provider_id {
            Some(id) => self.get_provider(id).into_iter().collect(),
            None => self.get_providers(),
        };
        let signal = AbortController::new().signal();
        let mut available = Vec::new();
        for provider in providers {
            let overrides = AuthResolutionOverrides::default();
            if !matches!(self.inner.auth.resolve(provider.as_ref(), &overrides).await, Ok(Some(_))) {
                continue;
            }
            let credential = self.inner.auth.read_credential(provider.id(), &signal).await.ok().flatten();
            available.extend(provider.filter_models(provider.get_models(), credential.as_ref()));
        }
        available
    }

    pub async fn get_auth(&self, provider_id: &str, overrides: &AuthResolutionOverrides) -> Result<Option<AuthResolution>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else { return Ok(None) };
        self.inner.auth.resolve(provider.as_ref(), overrides).await
    }

    fn require_provider(&self, model: &Model) -> Result<Arc<dyn Provider>, ModelsError> {
        self.get_provider(&model.provider)
            .ok_or_else(|| ModelsError::new(ModelsErrorCode::Provider, format!("Unknown provider: {}", model.provider)))
    }

    async fn apply_auth(
        &self,
        model: &Model,
        request: &crate::types::ProviderRequestOptions,
        transforms: &ModelsRequestTransforms,
    ) -> Result<(Arc<dyn Provider>, Model, crate::types::ProviderRequestOptions), ModelsError> {
        let provider = self.require_provider(model)?;
        let overrides = AuthResolutionOverrides {
            api_key: request.api_key.clone(),
            env: request.env.clone(),
            signal: request.signal.clone(),
            ..AuthResolutionOverrides::default()
        };
        let resolution = self
            .inner
            .auth
            .resolve(provider.as_ref(), &overrides)
            .await?
            .ok_or_else(|| ModelsError::new(ModelsErrorCode::Auth, provider_not_configured_message(&model.provider)))?;
        let mut headers = merge_headers(resolution.auth.headers.as_ref(), request.headers.as_ref());
        if let Some(transform) = &transforms.transform_headers {
            headers = Some(transform(headers.unwrap_or_default()).await);
        }
        let env = match (&resolution.env, &request.env) {
            (None, None) => None,
            (base, extra) => {
                let mut merged = base.clone().unwrap_or_default();
                merged.extend(extra.clone().unwrap_or_default());
                Some(merged)
            }
        };
        let mut request_model = model.clone();
        if let Some(base_url) = resolution.auth.base_url {
            request_model.base_url = base_url;
        }
        let mut options = request.clone();
        options.api_key = request.api_key.clone().or(resolution.auth.api_key);
        options.headers = headers;
        options.env = env;
        Ok((provider, request_model, options))
    }

    /// Relays `source` into `target` (the lazy stream contract of api/lazy.ts).
    fn forward(target: AssistantMessageEventStream, source: AssistantMessageEventStream) {
        tokio::spawn(async move {
            loop {
                match source.next().await {
                    Ok(Some(event)) => target.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        target.fail(error);
                        return;
                    }
                }
            }
            target.end(None);
        });
    }

    fn lazy(
        &self,
        model: &Model,
        setup: impl std::future::Future<Output = Result<AssistantMessageEventStream, ModelsError>> + Send + 'static,
    ) -> AssistantMessageEventStream {
        let outer = AssistantMessageEventStream::assistant();
        let target = outer.clone();
        let model = model.clone();
        tokio::spawn(async move {
            match setup.await {
                Ok(source) => Self::forward(target, source),
                Err(error) => {
                    let failed = error_stream(&model, &error.message);
                    Self::forward(target, failed);
                }
            }
        });
        outer
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
        transforms: ModelsRequestTransforms,
    ) -> AssistantMessageEventStream {
        let models = self.clone();
        let (model_owned, context) = (model.clone(), context.clone());
        self.lazy(model, async move {
            let mut options = options.unwrap_or_default();
            let (provider, request_model, request) = models.apply_auth(&model_owned, &options.request, &transforms).await?;
            options.request = request;
            Ok(provider.stream(&request_model, &context, Some(options)))
        })
    }

    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
        transforms: ModelsRequestTransforms,
    ) -> Result<AssistantMessage, crate::utils::event_stream::StreamError> {
        self.stream(model, context, options, transforms).result().await
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
        transforms: ModelsRequestTransforms,
    ) -> AssistantMessageEventStream {
        let models = self.clone();
        let (model_owned, context) = (model.clone(), context.clone());
        self.lazy(model, async move {
            let mut options = options.unwrap_or_default();
            let (provider, request_model, request) =
                models.apply_auth(&model_owned, &options.stream.request, &transforms).await?;
            options.stream.request = request;
            Ok(provider.stream_simple(&request_model, &context, Some(options)))
        })
    }

    pub async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
        transforms: ModelsRequestTransforms,
    ) -> Result<AssistantMessage, crate::utils::event_stream::StreamError> {
        self.stream_simple(model, context, options, transforms).result().await
    }

    pub fn stream_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<crate::types::DeferredFetchOptions>,
        transforms: ModelsRequestTransforms,
    ) -> AssistantMessageEventStream {
        let models = self.clone();
        let (model_owned, handle) = (model.clone(), handle.clone());
        self.lazy(model, async move {
            let mut options = options.unwrap_or_default();
            let (provider, request_model, request) = models.apply_auth(&model_owned, &options.request, &transforms).await?;
            options.request = request;
            provider.fetch_deferred(&request_model, &handle, Some(options)).ok_or_else(|| {
                ModelsError::new(
                    ModelsErrorCode::Provider,
                    format!("Provider {} does not support deferred responses", model_owned.provider),
                )
            })
        })
    }
}

pub fn has_api(model: &Model, api: &str) -> bool {
    model.api == api
}

/// Fills `usage.cost` from the model's (tiered) $/million-token rates and returns it.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> UsageCost {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let mut rates: ModelCostRates = model.cost.rates();
    let mut matched_threshold: i128 = -1;
    for tier in model.cost.tiers.iter().flatten() {
        let above = i128::from(tier.input_tokens_above);
        if i128::from(input_tokens) > above && above > matched_threshold {
            rates = ModelCostRates { input: tier.input, output: tier.output, cache_read: tier.cache_read, cache_write: tier.cache_write };
            matched_threshold = above;
        }
    }
    let long_write = usage.cache_write_1h.unwrap_or(0) as f64;
    let short_write = usage.cache_write as f64 - long_write;
    usage.cost.input = (rates.input / 1_000_000.0) * usage.input as f64;
    usage.cost.output = (rates.output / 1_000_000.0) * usage.output as f64;
    usage.cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    usage.cost.cache_write = (rates.cache_write * short_write + rates.input * 2.0 * long_write) / 1_000_000.0;
    usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost
}

const OPENAI_THINKING_APIS: &[&str] = &["openai-completions", "openai-responses", "azure-openai-responses", "openai-codex-responses"];

fn gpt_6_astra_thinking_level_map() -> ThinkingLevelMap {
    use ModelThinkingLevel::{High, Low, Max, Medium, Minimal, Off, Xhigh};
    [
        (Off, None),
        (Minimal, None),
        (Low, Some("low".into())),
        (Medium, Some("medium".into())),
        (High, Some("high".into())),
        (Xhigh, Some("xhigh".into())),
        (Max, Some("max".into())),
    ]
    .into_iter()
    .collect()
}

pub fn infer_openai_thinking_level_map(model: &Model) -> Option<ThinkingLevelMap> {
    if let Some(map) = &model.thinking_level_map {
        return Some(map.clone());
    }
    if OPENAI_THINKING_APIS.contains(&model.api.as_str()) && matches_model_family(&model.id, "gpt-6-astra") {
        return Some(gpt_6_astra_thinking_level_map());
    }
    None
}

/// `map?.[level]`: `None` = key absent, `Some(None)` = explicit null.
fn mapped_level(model: &Model, level: ModelThinkingLevel) -> Option<Option<String>> {
    infer_openai_thinking_level_map(model).and_then(|map| map.get(&level).cloned())
}

pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }
    ModelThinkingLevel::ALL
        .into_iter()
        .filter(|level| {
            if matches!(mapped_level(model, *level), Some(None)) {
                return false;
            }
            match level {
                ModelThinkingLevel::Xhigh => supports_xhigh(model),
                ModelThinkingLevel::Max => supports_max(model),
                _ => true,
            }
        })
        .collect()
}

/// Nearest supported level, preferring the next higher one.
pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available = get_supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }
    let all = ModelThinkingLevel::ALL;
    let requested = all.iter().position(|l| *l == level).unwrap_or(0);
    all[requested..]
        .iter()
        .chain(all[..requested].iter().rev())
        .find(|candidate| available.contains(candidate))
        .copied()
        .unwrap_or_else(|| available.first().copied().unwrap_or(ModelThinkingLevel::Off))
}

const XHIGH_MODEL_IDS: &[&str] = &[
    "gpt-5.2", "gpt-5.3", "gpt-5.4", "gpt-5.5", "gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-6-astra", "gpt-6-sol",
    "gpt-6-luna", "deepseek-v4-pro", "deepseek-v4-flash", "opus-4-6", "opus-4.6", "opus-4-7", "opus-4.7", "opus-4-8",
    "opus-4.8", "opus-5", "sonnet-5", "fable-5",
];

pub fn supports_xhigh(model: &Model) -> bool {
    match mapped_level(model, ModelThinkingLevel::Xhigh) {
        Some(None) => false,
        Some(Some(_)) => true,
        None if model.thinking_level_map.is_some() => false,
        None => model.reasoning && XHIGH_MODEL_IDS.iter().any(|id| matches_model_family(&model.id, id)),
    }
}

/// Family match at a token boundary: `gpt-*` families do not match after a `-` (so `gpt-5.5` does
/// not match `chatgpt-5.5`), and the family must end at a non-alphanumeric character.
fn matches_model_family(model_id: &str, family: &str) -> bool {
    let leading = if family.starts_with("gpt-") { "(?:^|[/.:_])" } else { "(?:^|[/.:_-])" };
    let pattern = format!("{leading}{}(?:$|[^a-z0-9])", regex::escape(&family.to_lowercase()));
    Regex::new(&pattern).is_ok_and(|re| re.is_match(&model_id.to_lowercase()))
}

const OPENAI_MAX_APIS: &[&str] = &["openai-responses", "azure-openai-responses", "openai-codex-responses", "openai-completions"];
const OPENAI_MAX_MODEL_IDS: &[&str] = &["gpt-5.6-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna"];
const MAX_MODEL_IDS: &[&str] = &["opus-4-6", "opus-4.6", "opus-4-7", "opus-4.7", "opus-4-8", "opus-4.8", "opus-5", "sonnet-5", "fable-5"];

pub fn supports_max(model: &Model) -> bool {
    match mapped_level(model, ModelThinkingLevel::Max) {
        Some(None) => false,
        Some(Some(_)) => true,
        None if model.thinking_level_map.is_some() => false,
        None => supports_max_model(model),
    }
}

fn supports_max_model(model: &Model) -> bool {
    if !model.reasoning {
        return false;
    }
    if OPENAI_MAX_APIS.contains(&model.api.as_str()) && OPENAI_MAX_MODEL_IDS.iter().any(|id| matches_model_family(&model.id, id)) {
        return true;
    }
    MAX_MODEL_IDS.iter().any(|id| matches_model_family(&model.id, id))
}

pub fn models_are_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
