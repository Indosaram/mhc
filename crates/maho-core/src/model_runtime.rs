//! Provider catalog composition and authenticated model execution.
use crate::model_config::ModelConfig;
use crate::provider_composer::{AuthStatus, ProviderConfigInput, compose_model_provider};
use maho_ai::legacy_provider_ids::normalize_provider_id;
use maho_ai::models::{AuthResolution, AuthResolutionOverrides, CreateModelsOptions, Models, ModelsAuth, ModelsError, ModelsErrorCode, ModelsRefreshOptions, ModelsRefreshResult, ModelsRequestTransforms, Provider, create_models};
use maho_ai::types::{AssistantMessage, AssistantMessageEventStream, BoxFuture, Context, Model, SimpleStreamOptions, StreamOptions};
use maho_ai::auth::context::DefaultAuthContext;
use maho_ai::auth::pool::slots::{PooledCredential, project_slot};
use maho_ai::auth::types::{ApiKeyCredential, AuthCheck, AuthOperationOptions, AuthType, Credential, CredentialInfo, CredentialStore, ProviderAuth};
use maho_ai::utils::abort::{AbortSignal, operation_signal};
use indexmap::IndexMap;
use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::{Arc, RwLock}};

#[derive(Default)]
pub struct CreateModelRuntimeOptions {
    pub models_path: Option<PathBuf>,
    pub auth_path:Option<PathBuf>,
    pub credentials: Option<Arc<crate::auth_storage::AuthStorage>>,
    pub providers: Option<Vec<Arc<dyn Provider>>>,
}

#[derive(Clone)]
struct RuntimeAuth {
    config: Arc<RwLock<ModelConfig>>,
    extensions: Arc<RwLock<IndexMap<String, ProviderConfigInput>>>,
    credentials: Arc<crate::auth_storage::AuthStorage>,
    /// The `CredentialStore` view of `credentials` that `resolve_provider_auth` and the shared OAuth
    /// refresh run against. One stable `Arc` so concurrent refreshes of a slot join one exchange.
    store: Arc<dyn CredentialStore>,
}

/// senpi `AuthStorage implements CredentialStore`: reads prefer the on-disk `auth.json` (so a token
/// rotated by another process is seen), a runtime API key wins, and an api-key value is resolved
/// through `resolveConfigValue`. `modify` is a short serialized read-modify-write; it never holds
/// the lock across an OAuth exchange (the refresh runs the exchange before calling it).
struct AuthStorageCredentialStore {
    credentials: Arc<crate::auth_storage::AuthStorage>,
    auth_path: Option<PathBuf>,
}

impl AuthStorageCredentialStore {
    fn raw(&self, provider_id: &str) -> Option<serde_json::Value> {
        self.auth_path
            .as_ref()
            .and_then(|path| crate::auth_storage::read_stored_credential(provider_id, &path.to_string_lossy()))
            .or_else(|| self.credentials.get(provider_id))
    }

    fn decode(value: serde_json::Value) -> anyhow::Result<Credential> {
        serde_json::from_value(value).map_err(|error| anyhow::anyhow!("Invalid auth.json credential: {error}"))
    }
}

fn check_abort(options: &Option<AuthOperationOptions>) -> anyhow::Result<()> {
    if let Some(signal) = options.as_ref().and_then(|options| options.signal.as_ref()) {
        signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
    }
    Ok(())
}

type ModifyFn = Box<dyn FnOnce(Option<Credential>) -> BoxFuture<'static, anyhow::Result<Option<Credential>>> + Send>;

#[async_trait::async_trait]
impl CredentialStore for AuthStorageCredentialStore {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> anyhow::Result<Option<Credential>> {
        check_abort(&options)?;
        if let Some(key) = self.credentials.runtime_api_key(provider_id) {
            return Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some(key), env: None })));
        }
        let Some(raw) = self.raw(provider_id) else { return Ok(None) };
        let credential = Self::decode(raw)?;
        check_abort(&options)?;
        Ok(Some(match credential {
            Credential::ApiKey(ApiKeyCredential { key: Some(key), env }) => {
                let lookup: Option<std::collections::HashMap<String, String>> = env.as_ref().map(|env| env.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                let key = crate::resolve_config_value::resolve_config_value(&key, lookup.as_ref()).await;
                Credential::ApiKey(ApiKeyCredential { key, env })
            }
            other => other,
        }))
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> anyhow::Result<Vec<CredentialInfo>> {
        check_abort(&options)?;
        Ok(self.credentials.list().into_iter().map(|(provider_id, kind)| CredentialInfo {
            provider_id,
            credential_type: match kind {
                crate::auth_storage::CredentialKind::ApiKey => maho_ai::auth::types::CredentialType::ApiKey,
                crate::auth_storage::CredentialKind::Oauth => maho_ai::auth::types::CredentialType::OAuth,
            },
        }).collect())
    }

    async fn modify(&self, provider_id: &str, f: ModifyFn, options: Option<AuthOperationOptions>) -> anyhow::Result<Option<Credential>> {
        check_abort(&options)?;
        let _mutation = self.credentials.account_mutation.lock().await;
        let current = self.raw(provider_id).map(Self::decode).transpose()?;
        let Some(next) = f(current.clone()).await? else { return Ok(current) };
        let value = serde_json::to_value(&next)?;
        self.credentials.set(provider_id, Some(value)).map_err(|error| anyhow::anyhow!(error))?;
        Ok(Some(next))
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> anyhow::Result<()> {
        check_abort(&options)?;
        let _mutation = self.credentials.account_mutation.lock().await;
        self.credentials.delete(provider_id).map_err(|error| anyhow::anyhow!(error))
    }
}

fn models_error(error: maho_ai::auth::resolve::ModelsError) -> ModelsError {
    let code = match error.code {
        "oauth" => ModelsErrorCode::OAuth,
        _ => ModelsErrorCode::Auth,
    };
    ModelsError::new(code, error.message)
}

/// senpi composes provider auth once in `composeProvider` (`composeApiKeyAuth` /
/// `composeOAuthAuth`); the models.json / extension overlays live in this runtime, so the
/// composition happens at resolution (and login) time against the provider's own `auth()`.
fn compose_provider_auth(
    config: &RwLock<ModelConfig>,
    extensions: &RwLock<IndexMap<String, ProviderConfigInput>>,
    provider: &dyn Provider,
) -> ProviderAuth {
    let id = provider.id();
    let config = config.read().unwrap_or_else(|p| p.into_inner()).get_provider(id).cloned();
    let extension = extensions.read().unwrap_or_else(|p| p.into_inner()).get(id).cloned();
    let base = provider.auth();
    let oauth = extension.as_ref().and_then(|e| e.oauth.clone())
        .or_else(|| base.and_then(|auth| auth.oauth.clone()))
        .or_else(|| builtin_oauth(id));
    // Providers registered without declared auth keep the env-var lookup senpi's builtin
    // factories declare through `envApiKeyAuth`.
    let inherited = base.and_then(|auth| auth.api_key.clone()).or_else(|| {
        maho_ai::env_api_keys::get_api_key_env_vars(id)
            .map(|vars| Arc::new(maho_ai::auth::helpers::env_api_key_auth("API key", &vars)) as Arc<dyn maho_ai::auth::types::ApiKeyAuth>)
    });
    let api_key = crate::provider_api_key_auth::compose_api_key_auth(id, inherited, oauth.is_some(), config.as_ref(), extension.as_ref().map(|e| &e.config))
        .map(|auth| Arc::new(auth) as Arc<dyn maho_ai::auth::types::ApiKeyAuth>);
    let oauth = crate::provider_composer::compose_oauth_auth(id, oauth, config.as_ref(), extension.as_ref().map(|e| &e.config))
        .map(|auth| Arc::new(auth) as Arc<dyn maho_ai::auth::types::OAuthAuth>);
    ProviderAuth { api_key, oauth }
}

impl RuntimeAuth {
    fn provider_auth(&self, provider: &dyn Provider) -> ProviderAuth {
        compose_provider_auth(&self.config, &self.extensions, provider)
    }

    /// An api-key pool's `accounts` live outside the typed `ApiKeyCredential`, so a slot of a stored
    /// api-key pool is projected here and resolved as that slot's explicit key. `Some(None)` = the
    /// slot is missing; `None` = not an api-key pool request.
    fn api_key_slot(&self, provider_id: &str, overrides: &AuthResolutionOverrides) -> Option<Option<AuthResolutionOverrides>> {
        let slot_name = overrides.slot_name.as_deref()?;
        let raw = self.credentials.get(provider_id)?;
        if crate::auth_storage::credential_kind(&raw) != Some(crate::auth_storage::CredentialKind::ApiKey) {
            return None;
        }
        let pooled: Option<PooledCredential> = serde_json::from_value(raw).ok();
        let Some(Credential::ApiKey(slot)) = project_slot(pooled.as_ref(), slot_name) else { return Some(None) };
        let Some(key) = slot.key else { return Some(None) };
        let mut env = slot.env.unwrap_or_default();
        env.extend(overrides.env.clone().unwrap_or_default());
        Some(Some(AuthResolutionOverrides {
            api_key: Some(key),
            env: (!env.is_empty()).then_some(env),
            min_oauth_validity_ms: overrides.min_oauth_validity_ms,
            slot_name: None,
            signal: overrides.signal.clone(),
        }))
    }
}

impl ModelsAuth for RuntimeAuth {
    fn resolve<'a>(&'a self, provider: &'a dyn Provider, overrides: &'a AuthResolutionOverrides) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
        Box::pin(async move {
            let signal = maho_ai::utils::abort::operation_signal(overrides.signal.clone());
            let provider_auth = self.provider_auth(provider);
            let slot_overrides = match self.api_key_slot(provider.id(), overrides) {
                Some(None) => return Ok(None),
                // A slot-scoped request never falls back to another identity.
                Some(Some(_)) if provider_auth.api_key.is_none() => return Ok(None),
                Some(Some(slot_overrides)) => Some(slot_overrides),
                None => None,
            };
            let overrides = slot_overrides.as_ref().unwrap_or(overrides);
            let result = maho_ai::auth::resolve::resolve_provider_auth(
                provider.id(), &provider_auth, &self.store, &DefaultAuthContext, Some(overrides), &signal,
            ).await.map_err(models_error)?;
            Ok(result.map(|result| AuthResolution {
                auth: crate::provider_api_key_auth::model_auth_to_result(result.auth),
                env: result.env,
            }))
        })
    }
    fn read_credential<'a>(&'a self,id: &'a str,_signal: &'a maho_ai::utils::abort::AbortSignal) -> BoxFuture<'a,Result<Option<serde_json::Value>,ModelsError>> {
        Box::pin(async move { Ok(self.credentials.get(id)) })
    }
    fn refresh_credential<'a>(&'a self,provider: &'a dyn Provider,stored: Option<&'a serde_json::Value>,signal: &'a maho_ai::utils::abort::AbortSignal) -> BoxFuture<'a,Result<Option<serde_json::Value>,ModelsError>> {
        Box::pin(async move {
            let id = provider.id();
            // The collection hands over the raw stored value; senpi's `readCredential` yields the
            // store's resolved view (api-key config values resolved), so re-read through the store.
            let stored = match stored {
                Some(_) => self.store.read(id, Some(AuthOperationOptions { signal: Some(signal.clone()) })).await
                    .map_err(|error| ModelsError::with_cause(ModelsErrorCode::Auth, format!("Credential store read failed for {id}"), &error.to_string()))?,
                None => None,
            };
            let provider_auth = self.provider_auth(provider);
            let credential = maho_ai::auth::refresh_credential::resolve_refresh_credential(
                id, &provider_auth, &self.store, &DefaultAuthContext, stored.as_ref(), signal,
            ).await.map_err(models_error)?;
            credential.map(|credential| serde_json::to_value(credential).map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.to_string()))).transpose()
        })
    }
    /// senpi `checkProviderAuth`: side-effect-free; never refreshes OAuth or runs `!command` keys.
    fn check<'a>(&'a self, provider: &'a dyn Provider, signal: &'a maho_ai::utils::abort::AbortSignal) -> BoxFuture<'a, Result<Option<AuthCheck>, ModelsError>> {
        Box::pin(async move {
            let provider_auth = self.provider_auth(provider);
            let id = provider.id();
            // senpi `readCredential`: the store's view, with api-key config values resolved.
            let credential = self.store.read(id, Some(AuthOperationOptions { signal: Some(signal.clone()) })).await
                .map_err(|error| ModelsError::with_cause(ModelsErrorCode::Auth, format!("Credential store read failed for {id}"), &error.to_string()))?;
            let check_error = |kind: &str, error: anyhow::Error| ModelsError::new(ModelsErrorCode::Auth, format!("{kind} auth check failed for provider {id}: {error}"));
            if let Some(Credential::OAuth(credential)) = &credential {
                let Some(oauth) = &provider_auth.oauth else { return Ok(None) };
                return oauth.check(&DefaultAuthContext, Some(credential), signal).await.map_err(|error| check_error("OAuth", error));
            }
            if let Some(api_key) = &provider_auth.api_key {
                let stored = credential.as_ref().and_then(Credential::as_api_key);
                if let Some(check) = api_key.check(&DefaultAuthContext, stored, signal).await.map_err(|error| check_error("API key", error))? {
                    return Ok(Some(check));
                }
            }
            match &provider_auth.oauth {
                Some(oauth) => oauth.check(&DefaultAuthContext, None, signal).await.map_err(|error| check_error("OAuth", error)),
                None => Ok(None),
            }
        })
    }
}

fn builtin_oauth(id:&str)->Option<Arc<dyn maho_ai::auth::types::OAuthAuth>> {
    use maho_ai::auth::oauth;
    match id {
        "anthropic"|"anthropic-subscription"=>Some(oauth::anthropic::anthropic_oauth()),
        "chatgpt-subscription"=>Some(oauth::chatgpt_subscription::chatgpt_subscription_oauth()),
        "github-copilot"=>Some(oauth::github_copilot::github_copilot_oauth()),
        "openrouter"=>Some(oauth::openrouter::open_router_oauth()),
        "kimi-coding"=>Some(oauth::kimi_coding::kimi_coding_oauth()),
        "xai"=>Some(oauth::xai::xai_oauth()),"cursor"=>Some(Arc::new(oauth::cursor::CursorOAuth::new())),
        "devin"=>Some(oauth::devin::devin_oauth()),
        "radius"=>Some(oauth::radius::radius_oauth("Radius",&maho_ai::providers::radius_config::normalize_radius_gateway_url(maho_ai::providers::radius_config::DEFAULT_RADIUS_GATEWAY))),
        _=>None,
    }
}

/// senpi `ModelRuntimeSnapshot`: the last availability view, readable synchronously.
#[derive(Default)]
struct ModelRuntimeSnapshot {
    all: Vec<Model>,
    available: Vec<Model>,
    configured_providers: HashSet<String>,
    stored_providers: HashSet<String>,
    auth: HashMap<String, AuthCheck>,
}

/// Snapshot plus the availability sequence counters and last availability error. Shared by
/// clones so a refresh on one handle is visible to every holder, like the single TS object.
#[derive(Default)]
struct AvailabilityState {
    snapshot: ModelRuntimeSnapshot,
    initialized: bool,
    refresh_seq: u64,
    error_seq: u64,
    provider_seq: HashMap<String, u64>,
    error: Option<String>,
}

#[derive(Clone)]
pub struct ModelRuntime {
    models: Models,
    config: Arc<RwLock<ModelConfig>>,
    builtins: IndexMap<String,Arc<dyn Provider>>,
    native: IndexMap<String,Arc<dyn Provider>>,
    extensions: Arc<RwLock<IndexMap<String,ProviderConfigInput>>>,
    errors: IndexMap<String,String>,
    models_path: Option<PathBuf>,
    pub credentials: Arc<crate::auth_storage::AuthStorage>,
    availability: Arc<RwLock<AvailabilityState>>,
    settings_manager: SharedSettingsSlot,
    provider_semaphores: Arc<crate::provider_concurrency::ProviderSemaphores>,
}

type SharedSettingsSlot = Arc<RwLock<Option<Arc<std::sync::Mutex<crate::settings_manager::SettingsManager>>>>>;

/// `settingsManager.getProviderConcurrencyLimit`: `providers.<id>.maxConcurrency`, canonical key first and
/// then the legacy spellings; zero (unlimited) when unset, non-integer or not positive.
fn provider_concurrency_limit(settings: &SharedSettingsSlot, provider_id: &str) -> i64 {
    let Some(manager) = settings.read().unwrap_or_else(|p| p.into_inner()).clone() else { return 0; };
    let manager = manager.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(providers) = manager.get_value("providers") else { return 0; };
    let entry = providers.get(provider_id).or_else(|| maho_ai::legacy_provider_ids::legacy_provider_ids_for(provider_id)
        .into_iter().find_map(|id| providers.get(id)));
    entry.and_then(|entry| entry.get("maxConcurrency")).and_then(serde_json::Value::as_f64)
        .filter(|value| value.fract() == 0.0 && *value > 0.0).map_or(0, |value| value as i64)
}

impl ModelRuntime {
    pub fn create_sync(options: CreateModelRuntimeOptions) -> Self {
        let credentials = options.credentials.unwrap_or_else(|| Arc::new(options.auth_path.as_ref().map_or_else(||crate::auth_storage::AuthStorage::in_memory(Default::default()),|path|crate::auth_storage::AuthStorage::create(&path.to_string_lossy()))));
        let config = Arc::new(RwLock::new(ModelConfig::load_sync_with_migration(options.models_path.as_deref(),!crate::lockfile_policy::facts_lock_broker_enabled())));
        let extensions = Arc::new(RwLock::new(IndexMap::new()));
        let store: Arc<dyn CredentialStore> = Arc::new(AuthStorageCredentialStore {credentials:credentials.clone(),auth_path:options.auth_path});
        let auth = RuntimeAuth {config:config.clone(),extensions:extensions.clone(),credentials:credentials.clone(),store};
        let settings_manager: SharedSettingsSlot = Arc::default();
        let limit_source = Arc::clone(&settings_manager);
        let provider_semaphores = Arc::new(crate::provider_concurrency::ProviderSemaphores::new(move |provider_id| provider_concurrency_limit(&limit_source, provider_id)));
        let mut runtime = Self {
            models:create_models(Some(CreateModelsOptions {auth:Some(Arc::new(auth)),..Default::default()})),
            config, builtins: options.providers.unwrap_or_else(maho_ai::providers::all::builtin_providers).into_iter().map(|p| (p.id().to_owned(),p)).collect(),
            native:IndexMap::new(), extensions, errors:IndexMap::new(),models_path:options.models_path,credentials,
            availability:Arc::new(RwLock::new(AvailabilityState::default())),
            settings_manager, provider_semaphores,
        };
        runtime.rebuild_providers(); runtime
    }
    pub async fn create(options: CreateModelRuntimeOptions) -> Self { Self::create_sync(options) }
    /// `setSettingsManager`: provider request concurrency follows the session's live settings.
    pub fn set_settings_manager(&self, settings_manager: Arc<std::sync::Mutex<crate::settings_manager::SettingsManager>>) {
        *self.settings_manager.write().unwrap_or_else(|p| p.into_inner()) = Some(settings_manager);
    }
    /// `reloadConfig`: re-read models.json and rebuild providers without network catalog fetches.
    pub async fn reload_config(&mut self) -> ModelsRefreshResult {
        self.refresh(ModelsRefreshOptions { allow_network: Some(false), ..Default::default() }).await
    }
    /// The provider/auth registry this runtime resolves against (pinned `models`). Exposed so a
    /// caller (e.g. the harness constructor) can hand the same registry to a `Models` consumer
    /// without rebuilding it through `create_models`.
    pub fn models(&self) -> &Models { &self.models }
    fn recompose_provider(&mut self, id: &str) {
        let id = normalize_provider_id(id);
        let config = self.config.read().unwrap_or_else(|p| p.into_inner()).clone();
        self.errors.shift_remove(&id);
        if config.is_provider_disabled(&id) { self.models.delete_provider(&id); return; }
        let base = self.native.get(&id).or_else(|| self.builtins.get(&id)).cloned();
        let extension = self.extensions.read().unwrap_or_else(|p| p.into_inner()).get(&id).cloned();
        if config.get_provider(&id).is_none() && extension.is_none() {
            match base {Some(base) => self.models.set_provider(base),None => self.models.delete_provider(&id)}
            return;
        }
        match compose_model_provider(&id,base.clone(),&config,extension.as_ref()) {
            Ok(provider) => self.models.set_provider(provider),
            Err(error) => {self.errors.insert(id.clone(),error);match base {Some(base) => self.models.set_provider(base),None => self.models.delete_provider(&id)}}
        }
    }
    fn rebuild_providers(&mut self) {
        self.models.clear_providers(); self.errors.clear();
        let mut ids: indexmap::IndexSet<String> = self.builtins.keys().chain(self.native.keys()).cloned().collect();
        ids.extend(self.config.read().unwrap_or_else(|p|p.into_inner()).get_provider_ids().into_iter().map(str::to_owned));
        ids.extend(self.extensions.read().unwrap_or_else(|p|p.into_inner()).keys().cloned());
        for id in ids { self.recompose_provider(&id); }
        self.update_model_snapshot();
    }
    fn availability_state(&self) -> std::sync::RwLockWriteGuard<'_, AvailabilityState> {
        self.availability.write().unwrap_or_else(|p| p.into_inner())
    }
    fn update_model_snapshot(&self) {
        let all = self.models.get_models(None);
        let mut guard = self.availability_state();
        let snapshot = &mut guard.snapshot;
        snapshot.available = all.iter().filter(|model| snapshot.configured_providers.contains(&model.provider)).cloned().collect();
        snapshot.all = all;
    }
    async fn run_availability_refresh(&self, seq: u64, error_seq: u64, signal: AbortSignal) -> Result<(), String> {
        let providers = self.models.get_providers();
        let available = self.models.get_available(None).await;
        let checks = futures::future::join_all(providers.iter().map(|provider| {
            let id = provider.id().to_owned();
            let options = AuthOperationOptions { signal: Some(signal.clone()) };
            async move { let check = self.check_auth(&id, Some(options)).await; (id, check) }
        })).await;
        let credentials = self.list_credentials(Some(AuthOperationOptions { signal: Some(signal.clone()) })).await.map_err(|error| error.message)?;
        let mut auth = HashMap::new();
        for (id, check) in checks {
            if let Some(check) = check.map_err(|error| error.message)? { auth.insert(id, check); }
        }
        let all = self.models.get_models(None);
        let mut guard = self.availability_state();
        if seq != guard.refresh_seq { return Ok(()); }
        guard.snapshot = ModelRuntimeSnapshot {
            all, available, configured_providers: auth.keys().cloned().collect(),
            stored_providers: credentials.into_iter().map(|entry| entry.provider_id).collect(), auth,
        };
        guard.initialized = true;
        if error_seq == guard.error_seq { guard.error = None; }
        Ok(())
    }
    async fn queue_availability_refresh(&self, signal: Option<AbortSignal>) -> Result<(), String> {
        let (seq, error_seq) = {
            let mut guard = self.availability_state();
            guard.refresh_seq += 1;
            for provider_seq in guard.provider_seq.values_mut() { *provider_seq += 1; }
            guard.error_seq += 1;
            (guard.refresh_seq, guard.error_seq)
        };
        let signal = operation_signal(signal);
        let result = self.run_availability_refresh(seq, error_seq, signal.clone()).await;
        if let Err(error) = &result {
            let mut guard = self.availability_state();
            if error_seq == guard.error_seq && !signal.aborted() { guard.error = Some(error.clone()); }
        }
        result
    }
    async fn refresh_provider_availability(&self, provider_id: &str, signal: AbortSignal) -> Result<(), String> {
        // Invalidate any full availability pass that started before this credential change.
        let (provider_seq, error_seq) = {
            let mut guard = self.availability_state();
            guard.refresh_seq += 1;
            let provider_seq = guard.provider_seq.get(provider_id).copied().unwrap_or(0) + 1;
            guard.provider_seq.insert(provider_id.to_owned(), provider_seq);
            guard.error_seq += 1;
            (provider_seq, guard.error_seq)
        };
        let result = async {
            let available = self.models.get_available(Some(provider_id)).await;
            let auth = self.check_auth(provider_id, Some(AuthOperationOptions { signal: Some(signal.clone()) })).await.map_err(|error| error.message)?;
            signal.throw_if_aborted().map_err(|reason| reason.message)?;
            let credential = self.credentials.get(provider_id);
            let all = self.models.get_models(None);
            let mut guard = self.availability_state();
            if guard.provider_seq.get(provider_id) != Some(&provider_seq) { return Ok(()); }
            let state = &mut *guard;
            let snapshot = &mut state.snapshot;
            match auth {
                Some(auth) => { snapshot.configured_providers.insert(provider_id.to_owned()); snapshot.auth.insert(provider_id.to_owned(), auth); }
                None => { snapshot.configured_providers.remove(provider_id); snapshot.auth.remove(provider_id); }
            }
            if credential.is_some() { snapshot.stored_providers.insert(provider_id.to_owned()); } else { snapshot.stored_providers.remove(provider_id); }
            let available_by_id: HashMap<(String, String), Model> = snapshot.available.iter().filter(|model| model.provider != provider_id).chain(available.iter())
                .map(|model| ((model.provider.clone(), model.id.clone()), model.clone())).collect();
            snapshot.available = all.iter().filter_map(|model| available_by_id.get(&(model.provider.clone(), model.id.clone())).cloned()).collect();
            snapshot.all = all;
            if error_seq == state.error_seq { state.error = None; }
            Ok::<(), String>(())
        }.await;
        if let Err(error) = &result {
            let mut guard = self.availability_state();
            if guard.provider_seq.get(provider_id) == Some(&provider_seq) && error_seq == guard.error_seq && !signal.aborted() {
                guard.error = Some(error.clone());
            }
        }
        result
    }
    pub fn get_error(&self) -> Option<String> {
        let mut errors:Vec<String> = self.config.read().unwrap_or_else(|p|p.into_inner()).get_error().map(str::to_owned).into_iter().collect();
        errors.extend(self.errors.iter().map(|(id,error)|format!("Provider \"{id}\": {error}")));
        if let Some(error) = self.availability_state().error.clone() { errors.push(format!("Availability refresh: {error}")); }
        if errors.is_empty() {None} else {Some(errors.join("\n\n"))}
    }
    pub fn get_warnings(&self) -> Vec<String> {self.config.read().unwrap_or_else(|p|p.into_inner()).get_warnings().to_vec()}
    pub fn get_models(&self,provider:Option<&str>) -> Vec<Model> {self.models.get_models(provider)}
    pub fn get_model(&self,provider:&str,id:&str) -> Option<Model> {self.models.get_model(&normalize_provider_id(provider),id)}
    pub fn get_provider(&self,id:&str) -> Option<Arc<dyn Provider>> {self.models.get_provider(&normalize_provider_id(id))}
    pub fn get_providers(&self) -> Vec<Arc<dyn Provider>> {self.models.get_providers()}
    /// senpi `getAvailable`: a provider-scoped read goes straight to the registry; a full read
    /// runs one availability pass and returns the refreshed snapshot. When that pass fails the
    /// error is recorded for `get_error` and the registry's own filtered list is returned, since
    /// this signature (unlike the TS promise) cannot carry the failure.
    pub async fn get_available(&self,provider:Option<&str>) -> Vec<Model> {
        if let Some(provider) = provider {
            let error_seq = { let mut guard = self.availability_state(); guard.error_seq += 1; guard.error_seq };
            let available = self.models.get_available(Some(provider)).await;
            let mut guard = self.availability_state();
            if error_seq == guard.error_seq { guard.error = None; }
            return available;
        }
        match self.queue_availability_refresh(None).await {
            Ok(()) => self.get_available_snapshot(),
            Err(_) => self.models.get_available(None).await,
        }
    }
    pub fn get_available_snapshot(&self) -> Vec<Model> { self.availability_state().snapshot.available.clone() }
    /// True only when the most recent availability refresh completed without error.
    pub fn has_fresh_availability_snapshot(&self) -> bool { let guard = self.availability_state(); guard.initialized && guard.error.is_none() }
    pub fn has_availability_snapshot(&self) -> bool { self.availability_state().initialized }
    pub fn has_configured_auth(&self, provider_id: &str) -> bool { self.availability_state().snapshot.configured_providers.contains(provider_id) }
    pub fn get_provider_auth_status(&self,id:&str) -> AuthStatus {
        // senpi `getProviderAuthStatus`: a non-persistent runtime API key wins, so the
        // credential-policy runtime-key branch can fire; the stored credential is next.
        if self.credentials.has_runtime_api_key(id) {return AuthStatus {configured:true,source:Some("runtime".into()),label:None};}
        if self.credentials.get(id).is_some() {return AuthStatus {configured:true,source:Some("stored".into()),label:None};}
        let config = self.config.read().unwrap_or_else(|p|p.into_inner()).get_provider(id).cloned();
        let extension = self.extensions.read().unwrap_or_else(|p|p.into_inner()).get(id).cloned();
        if let Some(status)=crate::provider_api_key_auth::configured_request_auth_status(config.as_ref(),extension.as_ref().map(|e|&e.config)){return status;}
        // `ANTHROPIC_AUTH_TOKEN` alone configures Anthropic (as a bearer header) even though it is
        // never returned as an API key.
        let configured=maho_ai::env_api_keys::get_env_api_key(id,None).is_some()||maho_ai::env_api_keys::find_env_keys(id,None).is_some();
        AuthStatus {configured,source:Some("environment".into()),label:None}
    }
    pub async fn get_auth(&self,id:&str) -> Result<Option<AuthResolution>,ModelsError> {self.models.get_auth(id,&AuthResolutionOverrides::default()).await}
    pub async fn get_auth_with_overrides(&self, id: &str, overrides: &AuthResolutionOverrides) -> Result<Option<AuthResolution>, ModelsError> {
        self.models.get_auth(id, overrides).await
    }
    pub async fn list_credentials(&self, options: Option<maho_ai::auth::types::AuthOperationOptions>) -> Result<Vec<maho_ai::auth::types::CredentialInfo>, ModelsError> {
        if let Some(signal) = options.and_then(|options| options.signal) {
            signal.throw_if_aborted().map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.message))?;
        }
        Ok(self.credentials.list().into_iter().map(|(provider_id, kind)| maho_ai::auth::types::CredentialInfo {
            provider_id, credential_type: match kind {
                crate::auth_storage::CredentialKind::ApiKey => maho_ai::auth::types::CredentialType::ApiKey,
                crate::auth_storage::CredentialKind::Oauth => maho_ai::auth::types::CredentialType::OAuth,
            }
        }).collect())
    }
    /// senpi `ModelRuntime.checkAuth` -> `Models.checkAuth`: side-effect-free provider availability.
    pub async fn check_auth(&self, id: &str, options: Option<maho_ai::auth::types::AuthOperationOptions>) -> Result<Option<maho_ai::auth::types::AuthCheck>, ModelsError> {
        let signal = maho_ai::utils::abort::operation_signal(options.and_then(|options| options.signal));
        signal.throw_if_aborted().map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.message))?;
        self.models.check_auth(&normalize_provider_id(id), &signal).await
    }
    /// Pinned `ModelRuntime.login` -> `Models.login` (`models.ts:583-637`): run the provider's
    /// composed OAuth or api-key login flow, append the credential as a login slot
    /// (`appendLoginSlot`) and report the committed account through `on_account_committed`.
    pub async fn login(&self, provider_id: &str, auth_type: maho_ai::auth::types::AuthType, interaction: std::sync::Arc<dyn maho_ai::auth::types::AuthInteraction>) -> Result<maho_ai::auth::types::Credential, ModelsError> {
        use maho_ai::auth::types::{AuthType, Credential, ProviderAuthInteraction};
        let id = normalize_provider_id(provider_id);
        let signal = maho_ai::utils::abort::operation_signal(interaction.signal());
        let aborted = |reason: maho_ai::utils::abort::AbortReason| ModelsError::new(ModelsErrorCode::Auth, reason.message);
        signal.throw_if_aborted().map_err(aborted)?;
        let Some(provider) = self.get_provider(&id) else { return Err(ModelsError::new(ModelsErrorCode::Provider, format!("Unknown provider: {id}"))) };
        let auth = compose_provider_auth(&self.config, &self.extensions, provider.as_ref());
        let provider_interaction = ProviderAuthInteraction::new(signal.clone(), interaction.clone());
        let unsupported = |kind: &str| ModelsError::new(ModelsErrorCode::Auth, format!("{} does not support {kind} login", provider.name()));
        let credential = match auth_type {
            AuthType::OAuth => {
                let oauth = auth.oauth.ok_or_else(|| unsupported("oauth"))?;
                let login = maho_ai::utils::abort::race_with_abort_signal(oauth.login(&provider_interaction), &signal).await.map_err(aborted)?;
                Credential::OAuth(login.map_err(|error| ModelsError::new(ModelsErrorCode::OAuth, error.to_string()))?)
            }
            AuthType::ApiKey => {
                let api_key = auth.api_key.filter(|api_key| api_key.has_login()).ok_or_else(|| unsupported("api_key"))?;
                let login = maho_ai::utils::abort::race_with_abort_signal(api_key.login(&provider_interaction), &signal).await.map_err(aborted)?;
                Credential::ApiKey(login.map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.to_string()))?)
            }
        };
        let receipt = self.credentials.append_login_account(&id, credential.clone(), Some(signal.clone())).await.map_err(|error| {
            match signal.throw_if_aborted() {
                Err(reason) => aborted(reason),
                Ok(()) => ModelsError::with_cause(ModelsErrorCode::Auth, format!("Credential store modify failed for {id}"), &error),
            }
        })?;
        if let Some(receipt) = receipt { maho_ai::auth::types::AuthInteraction::on_account_committed(interaction.as_ref(), receipt); }
        // senpi `synchronizeCredentialState`: the snapshot follows the credential change. A
        // failed pass is recorded as the availability error surfaced by `get_error`.
        let _ = self.refresh_provider_availability(&id, signal).await;
        Ok(credential)
    }

    /// Pinned `ModelRuntime.logout` -> `Models.logout` (`models.ts:639-655`): with `slot_id`,
    /// remove only that account slot (deleting the entry when it was the last); otherwise remove
    /// the provider's stored credential.
    pub async fn logout(&self, provider_id: &str, slot_id: Option<&str>, options: maho_ai::auth::types::AuthOperationOptions) -> Result<(), ModelsError> {
        use maho_ai::auth::pool::slots::{PooledCredentialType, remove_slot};
        let id = normalize_provider_id(provider_id);
        let signal = maho_ai::utils::abort::operation_signal(options.signal);
        signal.throw_if_aborted().map_err(|reason| ModelsError::new(ModelsErrorCode::Auth, reason.message))?;
        let result = 'mutation: {
            let failed = |error: String| ModelsError::with_cause(ModelsErrorCode::Auth, format!("Credential store delete failed for {id}"), &error);
            let _mutation = self.credentials.account_mutation.lock().await;
            let Some(slot_id) = slot_id else { break 'mutation self.credentials.delete(&id).map_err(failed) };
            let current = self.credentials.get(&id)
                .map(serde_json::from_value::<PooledCredential>).transpose().map_err(|error| failed(error.to_string()))?;
            let Some(next) = remove_slot(current, slot_id) else { break 'mutation self.credentials.delete(&id).map_err(failed) };
            let value = match next.credential_type {
                PooledCredentialType::ApiKey => serde_json::to_value(&next),
                PooledCredentialType::OAuth => serde_json::to_value(next.to_stored_credential()),
            }.map_err(|error| failed(error.to_string()))?;
            self.credentials.set(&id, Some(value)).map_err(failed)
        };
        // senpi `synchronizeCredentialState`; a failed pass is recorded for `get_error`.
        let _ = self.refresh_provider_availability(&id, signal).await;
        result
    }

    pub async fn get_auth_for_model(&self, model: &Model, overrides: &AuthResolutionOverrides) -> Result<Option<AuthResolution>, ModelsError> {
        let signal = maho_ai::utils::abort::operation_signal(overrides.signal.clone());
        let resolve = async {
            let Some(mut resolution) = self.models.get_auth_for_model(model, overrides).await? else { return Ok(None); };
            let mut env = resolution.env.clone().unwrap_or_default();
            env.extend(overrides.env.clone().unwrap_or_default());
            let env: std::collections::HashMap<String, String> = env.into_iter().collect();
            let headers = self.get_compatibility_request_headers(model, Some(&env)).await
                .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error))?;
            if let Some(headers) = headers { resolution.auth.headers.get_or_insert_with(Default::default).extend(headers); }
            Ok(Some(resolution))
        };
        maho_ai::utils::abort::race_with_abort_signal(resolve, &signal).await
            .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.message))?
    }
    pub fn get_compatibility_request_config(&self,model:&Model)->crate::provider_composer::CompatibilityRequestConfig {
        let config=self.config.read().unwrap_or_else(|p|p.into_inner());let extension=self.extensions.read().unwrap_or_else(|p|p.into_inner());
        crate::provider_composer::resolve_compatibility_request_config(model,config.get_provider(&model.provider),extension.get(&model.provider))
    }
    pub async fn get_compatibility_request_headers(&self,model:&Model,env:Option<&std::collections::HashMap<String,String>>)->Result<Option<maho_ai::types::ProviderHeaders>,String> {
        let config=self.config.read().unwrap_or_else(|p|p.into_inner()).get_provider(&model.provider).cloned();
        let extension=self.extensions.read().unwrap_or_else(|p|p.into_inner()).get(&model.provider).cloned();
        crate::provider_composer::resolve_compatibility_request_headers(model,config.as_ref(),extension.as_ref(),env).await
    }
    pub async fn refresh(&mut self,options:ModelsRefreshOptions) -> ModelsRefreshResult {
        let config = ModelConfig::load_with_migration(self.models_path.as_deref(),!crate::lockfile_policy::facts_lock_broker_enabled()).await;
        *self.config.write().unwrap_or_else(|p|p.into_inner()) = config;
        self.rebuild_providers();
        let providers = options.providers.clone();
        let signal = options.signal.clone();
        let mut result = self.models.refresh(options).await;
        self.update_model_snapshot();
        let this: &Self = self;
        if let Some(providers) = providers {
            let providers: indexmap::IndexSet<String> = providers.into_iter().collect();
            let outcomes = futures::future::join_all(providers.iter().map(|provider_id| {
                let provider_signal = operation_signal(signal.clone());
                async move { (provider_id.clone(), this.refresh_provider_availability(provider_id, provider_signal).await) }
            })).await;
            for (provider_id, outcome) in outcomes {
                if let Err(error) = outcome && !signal.as_ref().is_some_and(AbortSignal::aborted) {
                    result.errors.insert(provider_id, ModelsError::new(ModelsErrorCode::Auth, error));
                }
            }
        } else {
            // Availability errors are recorded by the latest pass (see `get_error`); refreshed
            // models remain usable.
            let _ = this.queue_availability_refresh(signal.clone()).await;
        }
        result.aborted = result.aborted || signal.as_ref().is_some_and(AbortSignal::aborted);
        result
    }
    pub fn register_provider(&mut self,id:&str,config:ProviderConfigInput) -> Result<(),String> {
        let id = normalize_provider_id(id);
        compose_model_provider(&id,self.native.get(&id).or_else(||self.builtins.get(&id)).cloned(),&self.config.read().unwrap_or_else(|p|p.into_inner()),Some(&config))?;
        self.extensions.write().unwrap_or_else(|p|p.into_inner()).insert(id.clone(),config.clone());self.recompose_provider(&id);
        self.update_model_snapshot();
        let stored = self.availability_state().snapshot.stored_providers.contains(&id);
        let configured = stored || crate::provider_api_key_auth::configured_request_auth_status(
            self.config.read().unwrap_or_else(|p|p.into_inner()).get_provider(&id), Some(&config.config)).is_some_and(|status| status.configured);
        if configured {
            let mut guard = self.availability_state();
            let snapshot = &mut guard.snapshot;
            snapshot.configured_providers.insert(id.clone());
            // Provisional entry until the async refresh lands; never clobber a real check result.
            snapshot.auth.entry(id.clone()).or_insert_with(|| AuthCheck {
                source: Some("configured provider".into()),
                auth_type: if config.oauth.is_some() && config.api_key.is_none() { AuthType::OAuth } else { AuthType::ApiKey },
            });
            snapshot.available = snapshot.all.iter().filter(|model| snapshot.configured_providers.contains(&model.provider)).cloned().collect();
        }
        Ok(())
    }
    pub fn register_native_provider(&mut self,provider:Arc<dyn Provider>) {let id=normalize_provider_id(provider.id());self.native.insert(id.clone(),provider);self.recompose_provider(&id);self.update_model_snapshot();}
    pub fn unregister_provider(&mut self,id:&str) {let id=normalize_provider_id(id);self.native.shift_remove(&id);self.extensions.write().unwrap_or_else(|p|p.into_inner()).shift_remove(&id);self.recompose_provider(&id);self.update_model_snapshot();}
    pub fn get_registered_provider_ids(&self) -> Vec<String> {self.extensions.read().unwrap_or_else(|p|p.into_inner()).keys().chain(self.native.keys()).cloned().collect()}
    pub fn get_registered_provider_config(&self,id:&str) -> Option<ProviderConfigInput> {self.extensions.read().unwrap_or_else(|p|p.into_inner()).get(id).cloned()}
    pub fn get_registered_native_provider(&self,id:&str) -> Option<Arc<dyn Provider>> {self.native.get(id).cloned()}
    pub fn is_using_oauth(&self,id:&str) -> bool {self.credentials.get(id).as_ref().and_then(crate::auth_storage::credential_kind)==Some(crate::auth_storage::CredentialKind::Oauth)}
    pub fn is_fallback_eligible(&self,id:&str) -> bool {self.extensions.read().unwrap_or_else(|p|p.into_inner()).get(id).and_then(|e|e.fallback_eligible.as_ref()).is_none_or(|hook|hook())}
    fn prepare_request(&self,model:&Model,options:&mut StreamOptions)->(Model,Option<crate::model_config_schema::ModelsJsonProvider>,Option<ProviderConfigInput>) {
        let config=self.config.read().unwrap_or_else(|p|p.into_inner()).get_provider(&model.provider).cloned();
        let extension=self.extensions.read().unwrap_or_else(|p|p.into_inner()).get(&model.provider).cloned();
        let compatibility=crate::provider_composer::resolve_compatibility_request_config(model,config.as_ref(),extension.as_ref());
        if let Some(mut body)=compatibility.extra_body {body.extend(options.extra_body.take().unwrap_or_default());options.extra_body=Some(body);}
        let mut request_model=model.clone();if let Some(id)=compatibility.upstream_model_id {request_model.id=id;}
        if let Some(tier)=compatibility.service_tier {request_model.service_tier=Some(tier);}
        (request_model,config,extension)
    }
    pub fn stream(&self,model:&Model,context:&Context,options:Option<StreamOptions>) -> AssistantMessageEventStream {
        let mut options=options.unwrap_or_default();let (request_model,config,extension)=self.prepare_request(model,&mut options);
        let models=self.models.clone();let context=context.clone();let original=model.clone();let tools=context.tools.clone().unwrap_or_default();let signal=options.request.signal.clone();
        let inner=self.lazy_request(model,signal,async move {
            Self::apply_configured_headers(&original,config.as_ref(),extension.as_ref(),&mut options).await?;
            Ok(models.stream(&request_model,&context,Some(options),Default::default()))
        });
        maho_ai::wrap_stream_with_model_recovery(inner,model,&tools)
    }
    pub fn stream_simple(&self,model:&Model,context:&Context,options:Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
        self.stream_simple_with_transforms(model,context,options,ModelsRequestTransforms::default())
    }
    /// `streamSimple` with the `ModelsRequestTransforms` senpi passes inside the options object
    /// (`transformHeaders` runs on the assembled request headers after auth resolution).
    pub fn stream_simple_with_transforms(&self,model:&Model,context:&Context,options:Option<SimpleStreamOptions>,transforms:ModelsRequestTransforms) -> AssistantMessageEventStream {
        let mut options=options.unwrap_or_default();let (request_model,config,extension)=self.prepare_request(model,&mut options.stream);
        let models=self.models.clone();let context=context.clone();let original=model.clone();let tools=context.tools.clone().unwrap_or_default();let signal=options.stream.request.signal.clone();
        let inner=self.lazy_request(model,signal,async move {
            Self::apply_configured_headers(&original,config.as_ref(),extension.as_ref(),&mut options.stream).await?;
            Ok(models.stream_simple(&request_model,&context,Some(options),transforms))
        });
        maho_ai::wrap_stream_with_model_recovery(inner,model,&tools)
    }
    async fn apply_configured_headers(model:&Model,config:Option<&crate::model_config_schema::ModelsJsonProvider>,extension:Option<&ProviderConfigInput>,options:&mut StreamOptions)->Result<(),String> {
        let env=options.request.env.as_ref().map(|v|v.iter().map(|(k,v)|(k.clone(),v.clone())).collect());
        let signal = maho_ai::utils::abort::operation_signal(options.request.signal.clone());
        let headers = maho_ai::utils::abort::race_with_abort_signal(
            crate::provider_composer::resolve_compatibility_request_headers(model,config,extension,env.as_ref()), &signal)
            .await.map_err(|error| error.message)??;
        if let Some(mut headers)=headers{headers.extend(options.request.headers.take().unwrap_or_default());options.request.headers=Some(headers);}
        Ok(())
    }
    fn lazy_request(&self,model:&Model,signal:Option<maho_ai::utils::abort::AbortSignal>,setup:impl std::future::Future<Output=Result<AssistantMessageEventStream,String>>+Send+'static)->AssistantMessageEventStream {
        let stream=AssistantMessageEventStream::assistant();let target=stream.clone();let model=model.clone();
        let semaphores=Arc::clone(&self.provider_semaphores);
        tokio::spawn(async move {
            // `providerSemaphores.bracket`: hold the provider slot until the request stream settles;
            // an abort while queued rejects without ever starting the request.
            let signal=maho_ai::utils::abort::operation_signal(signal);
            let (_slot,setup)=match maho_ai::utils::abort::race_with_abort_signal(semaphores.acquire(&model.provider),&signal).await {
                Ok(slot)=>(slot,setup.await),Err(reason)=>(None,Err(reason.message)),
            };
            let source=match setup {Ok(stream)=>stream,Err(error)=>maho_ai::utils::lazy::error_stream(&model,&error)};
            loop {match source.next().await {Ok(Some(event))=>target.push(event),Ok(None)=>{target.end(None);break;},Err(error)=>{target.fail(error);break;}}}
        });stream
    }
    pub async fn complete(&self,model:&Model,context:&Context,options:Option<StreamOptions>) -> Result<AssistantMessage,maho_ai::utils::event_stream::StreamError> {self.stream(model,context,options).result().await}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn configured_runtime(text:&str)->(tempfile::TempDir,ModelRuntime) {
        let dir=tempfile::tempdir().expect("tempdir");let path=dir.path().join("models.json");std::fs::write(&path,text).expect("config");
        let runtime=ModelRuntime::create_sync(CreateModelRuntimeOptions{models_path:Some(path),providers:Some(Vec::new()),..Default::default()});(dir,runtime)
    }
    #[test]
    fn configured_catalog_has_identical_api_and_base_url() {
        let (_dir,runtime)=configured_runtime(r#"{"providers":{"kiro-lb":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"claude-sonnet-5"}]}}}"#);
        assert!(runtime.get_error().is_none());let model=runtime.get_model("kiro-lb","claude-sonnet-5").expect("model");assert_eq!(model.api,"openai-completions");assert_eq!(model.base_url,"http://localhost/v1");
    }
    #[test]
    fn unknown_invalid_provider_reports_error_and_preserves_valid_catalog() {
        let (_dir,runtime)=configured_runtime(r#"{"providers":{"unknown-task-17":{"models":[{"id":"missing-api"}]},"valid":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        assert!(runtime.get_model("valid","sol").is_some());assert!(runtime.get_provider("unknown-task-17").is_none());assert!(runtime.get_error().is_some());
    }
    #[tokio::test]
    async fn runtime_auth_resolves_key_and_provider_headers() {
        let (_dir,runtime)=configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","apiKey":"fixture-key","authHeader":true,"models":[{"id":"sol"}]}}}"#);
        let auth=runtime.get_auth("p").await.expect("resolve").expect("auth");assert_eq!(auth.auth.api_key.as_deref(),Some("fixture-key"));assert_eq!(auth.auth.headers.expect("headers")["Authorization"].as_deref(),Some("Bearer fixture-key"));
    }
    #[tokio::test]
    async fn auth_overrides_metadata_and_preaborted_checks_use_shared_store() {
        let (_dir, runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol","headers":{"x-model":"native"}}]}}}"#);
        runtime.credentials.set("p", Some(serde_json::json!({"type":"api_key","key":"stored-fixture"}))).expect("seed");
        let model = runtime.get_model("p", "sol").expect("model");
        let auth = runtime.get_auth_for_model(&model, &AuthResolutionOverrides { api_key: Some("override-fixture".into()), ..Default::default() })
            .await.expect("resolve").expect("auth");
        assert_eq!(auth.auth.api_key.as_deref(), Some("override-fixture"));
        assert_eq!(auth.auth.headers.expect("model headers")["x-model"].as_deref(), Some("native"));
        let metadata = runtime.list_credentials(None).await.expect("metadata");
        assert!(metadata.iter().any(|entry| entry.provider_id == "p" && entry.credential_type == maho_ai::auth::types::CredentialType::ApiKey));
        let controller = maho_ai::utils::abort::AbortController::new(); controller.abort(None);
        let error = runtime.get_auth_for_model(&model, &AuthResolutionOverrides {
            signal: Some(controller.signal()), api_key: Some("override-fixture".into()), ..Default::default()
        }).await.expect_err("preaborted model auth must not resolve");
        assert_eq!(error.to_string(), maho_ai::utils::abort::AbortReason::dom_default().message);
        assert!(runtime.check_auth("p", Some(maho_ai::auth::types::AuthOperationOptions { signal: Some(controller.signal()) })).await.is_err());
        assert!(runtime.list_credentials(Some(maho_ai::auth::types::AuthOperationOptions { signal: Some(controller.signal()) })).await.is_err());
    }
    #[tokio::test]
    async fn runtime_stream_recovers_leaked_text_tool_calls_for_claude_models() {
        use maho_ai::providers::faux::{FauxAssistantMessageOptions, FauxModelDefinition, RegisterFauxProviderOptions, faux_assistant_message, faux_provider};
        let (_dir, mut runtime) = configured_runtime(r#"{"providers":{}}"#);
        let faux = faux_provider(RegisterFauxProviderOptions {
            provider: Some("faux-recovery".into()),
            models: Some(vec![FauxModelDefinition { id: "claude-fixture".into(), ..Default::default() }]),
            ..Default::default()
        });
        let invoke = r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#;
        faux.core.set_responses(vec![faux_assistant_message(invoke, FauxAssistantMessageOptions::default()).into()]);
        runtime.register_native_provider(faux.provider.clone());
        let model = runtime.get_model("faux-recovery", "claude-fixture").expect("model");
        let tool = maho_ai::types::Tool { name: "get_weather".into(), description: "d".into(), parameters: serde_json::json!({"type":"object"}), freeform: None, constrained_sampling: None };
        let context = Context { system_prompt: None, messages: Vec::new(), tools: Some(vec![tool]) };
        let mut options = StreamOptions::default();
        options.request.api_key = Some("fixture-key".into());
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), runtime.stream(&model, &context, Some(options)).result())
            .await.expect("bounded stream").expect("result");
        assert_eq!(message.stop_reason, maho_ai::types::StopReason::ToolUse);
        assert!(message.content.iter().any(|block| matches!(block, maho_ai::types::ContentBlock::ToolCall(call) if call.name == "get_weather")));
    }
    #[tokio::test]
    async fn preaborted_configured_stream_preserves_headers_and_stops_setup() {
        let (_dir, runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        let model = runtime.get_model("p", "sol").expect("model");
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        let headers = std::collections::BTreeMap::from([("x-caller".to_owned(), Some("retained".to_owned()))]);
        let mut options = StreamOptions { request: maho_ai::types::ProviderRequestOptions {
            signal: Some(controller.signal()), headers: Some(headers.clone()), ..Default::default()
        }, ..Default::default() };
        let result = ModelRuntime::apply_configured_headers(&model, None, None, &mut options).await;
        assert_eq!(result, Err(maho_ai::utils::abort::AbortReason::dom_default().message));
        assert_eq!(options.request.headers, Some(headers));
    }

    fn builtin_runtime(id: &str) -> ModelRuntime {
        let provider = maho_ai::providers::all::builtin_providers().into_iter().find(|p| p.id() == id).expect("builtin provider");
        ModelRuntime::create_sync(CreateModelRuntimeOptions { providers: Some(vec![provider]), ..Default::default() })
    }

    #[tokio::test]
    async fn stored_cloudflare_credential_env_reaches_the_resolution() {
        let runtime = builtin_runtime("cloudflare-ai-gateway");
        runtime.credentials.set("cloudflare-ai-gateway", Some(serde_json::json!({"type":"api_key","key":"cf-key",
            "env":{"CLOUDFLARE_ACCOUNT_ID":"acct","CLOUDFLARE_GATEWAY_ID":"gw"}}))).expect("seed");
        let auth = runtime.get_auth("cloudflare-ai-gateway").await.expect("resolve").expect("auth");
        let env = auth.env.expect("credential env");
        assert_eq!(env.get("CLOUDFLARE_ACCOUNT_ID").map(String::as_str), Some("acct"));
        assert_eq!(env.get("CLOUDFLARE_GATEWAY_ID").map(String::as_str), Some("gw"));
        assert_eq!(auth.auth.headers.expect("gateway headers")["cf-aig-authorization"].as_deref(), Some("Bearer cf-key"));
    }

    #[tokio::test]
    async fn stored_kimi_region_routes_the_base_url() {
        let runtime = builtin_runtime("kimi-coding");
        runtime.credentials.set("kimi-coding", Some(serde_json::json!({"type":"api_key","key":"kimi-key",
            "env":{"KIMI_CODE_REGION":"global"}}))).expect("seed");
        let auth = runtime.get_auth("kimi-coding").await.expect("resolve").expect("auth");
        assert_eq!(auth.auth.base_url.as_deref(), Some("https://api.kimi.ai/coding"));
    }

    #[tokio::test]
    async fn missing_slot_does_not_fall_back_to_the_flat_credential() {
        let (_dir, runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        runtime.credentials.set("p", Some(serde_json::json!({"type":"api_key","key":"flat","accounts":[{"name":"default","key":"flat"}]}))).expect("seed");
        let overrides = AuthResolutionOverrides { slot_name: Some("work".into()), ..Default::default() };
        assert!(runtime.get_auth_with_overrides("p", &overrides).await.expect("resolve").is_none());
        let overrides = AuthResolutionOverrides { slot_name: Some("default".into()), ..Default::default() };
        let auth = runtime.get_auth_with_overrides("p", &overrides).await.expect("resolve").expect("slot auth");
        assert_eq!(auth.auth.api_key.as_deref(), Some("flat"));
    }

    struct CountingOAuth(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl maho_ai::auth::types::OAuthAuth for CountingOAuth {
        fn name(&self) -> &str { "counting-oauth" }
        async fn login(&self, _: &maho_ai::auth::types::ProviderAuthInteraction) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> { anyhow::bail!("login is not exercised") }
        async fn refresh(&self, _: &maho_ai::auth::types::OAuthCredential, _: &maho_ai::utils::abort::AbortSignal) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(maho_ai::auth::types::OAuthCredential::new("fresh", "next", f64::MAX))
        }
        async fn to_auth(&self, credential: &maho_ai::auth::types::OAuthCredential) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(maho_ai::auth::types::ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    fn counting_oauth_runtime(expires_in_ms: f64) -> (tempfile::TempDir, ModelRuntime, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let (dir, mut runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        runtime.register_provider("p", crate::provider_composer::ProviderConfigInput { oauth: Some(std::sync::Arc::new(CountingOAuth(calls.clone()))), ..Default::default() }).expect("register oauth");
        let expires = maho_ai::utils::diagnostics::now_ms() as f64 + expires_in_ms;
        runtime.credentials.set("p", Some(serde_json::json!({"type":"oauth","access":"old","refresh":"r1","expires":expires}))).expect("seed");
        (dir, runtime, calls)
    }

    #[tokio::test]
    async fn oauth_refreshes_inside_the_five_minute_window() {
        let (_dir, runtime, calls) = counting_oauth_runtime(120_000.0);
        let auth = runtime.get_auth("p").await.expect("resolve").expect("auth");
        assert_eq!(auth.auth.api_key.as_deref(), Some("fresh"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(runtime.credentials.get("p").expect("stored")["access"], "fresh");
    }

    #[tokio::test]
    async fn available_models_check_never_refreshes_oauth() {
        let (_dir, runtime, calls) = counting_oauth_runtime(-1.0);
        assert!(runtime.get_available(Some("p")).await.iter().any(|model| model.id == "sol"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    struct TestAuthInteraction;
    #[async_trait::async_trait]
    impl maho_ai::auth::types::AuthInteraction for TestAuthInteraction {
        fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> { None }
        async fn prompt(&self, _: maho_ai::auth::types::AuthPrompt) -> anyhow::Result<String> { Ok("entered-key".into()) }
        fn notify(&self, _: maho_ai::auth::types::AuthEvent) {}
    }
    struct TestOAuth;
    #[async_trait::async_trait]
    impl maho_ai::auth::types::OAuthAuth for TestOAuth {
        fn name(&self) -> &str { "test-oauth" }
        async fn login(&self, _: &maho_ai::auth::types::ProviderAuthInteraction) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> { Ok(maho_ai::auth::types::OAuthCredential::new("access", "refresh", 42.0)) }
        async fn refresh(&self, credential: &maho_ai::auth::types::OAuthCredential, _: &maho_ai::utils::abort::AbortSignal) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> { Ok(credential.clone()) }
        async fn to_auth(&self, _: &maho_ai::auth::types::OAuthCredential) -> anyhow::Result<maho_ai::auth::types::ModelAuth> { Ok(maho_ai::auth::types::ModelAuth::default()) }
    }

    #[tokio::test]
    async fn login_persists_an_oauth_credential_and_logout_removes_it() {
        let (_dir, mut runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        runtime.register_provider("p", crate::provider_composer::ProviderConfigInput { oauth: Some(std::sync::Arc::new(TestOAuth)), ..Default::default() }).expect("register oauth");
        let credential = runtime.login("p", maho_ai::auth::types::AuthType::OAuth, std::sync::Arc::new(TestAuthInteraction)).await.expect("login");
        assert!(matches!(credential, maho_ai::auth::types::Credential::OAuth(_)));
        assert!(runtime.is_using_oauth("p"));
        assert!(runtime.credentials.get("p").is_some());
        runtime.logout("p", None, Default::default()).await.expect("logout");
        assert!(runtime.credentials.get("p").is_none());
    }

    #[tokio::test]
    async fn login_rejects_unknown_provider_and_unsupported_oauth() {
        let (_dir, runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        let unknown = runtime.login("missing", maho_ai::auth::types::AuthType::ApiKey, std::sync::Arc::new(TestAuthInteraction)).await.expect_err("unknown provider");
        assert_eq!(unknown.code, ModelsErrorCode::Provider);
        let unsupported = runtime.login("p", maho_ai::auth::types::AuthType::OAuth, std::sync::Arc::new(TestAuthInteraction)).await.expect_err("no oauth method");
        assert_eq!(unsupported.code, ModelsErrorCode::Auth);
    }

    #[tokio::test]
    async fn api_key_login_prompts_and_persists() {
        let (_dir, runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        let credential = runtime.login("p", maho_ai::auth::types::AuthType::ApiKey, std::sync::Arc::new(TestAuthInteraction)).await.expect("api key login");
        assert_eq!(credential, maho_ai::auth::types::Credential::ApiKey(maho_ai::auth::types::ApiKeyCredential { key: Some("entered-key".into()), env: None }));
        assert_eq!(runtime.credentials.get("p").and_then(|value| value.get("key").and_then(|key| key.as_str().map(str::to_owned))), Some("entered-key".into()));
    }

    /// Hides every model but `keep`, like a provider entitlement filter.
    struct FilteringProvider;
    impl Provider for FilteringProvider {
        fn id(&self) -> &str { "filtering" }
        fn name(&self) -> &str { "filtering" }
        fn get_models(&self) -> Vec<Model> {
            ["keep","hide"].into_iter().map(|id| serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "api": "openai-completions", "provider": "filtering", "baseUrl": "http://localhost/v1",
                "reasoning": false, "input": [], "contextWindow": 128000, "maxTokens": 4096,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
            })).expect("model")).collect()
        }
        fn filter_models(&self, models: Vec<Model>, credential: Option<&maho_ai::models::Credential>) -> Vec<Model> {
            assert!(credential.is_some(), "filter receives the stored credential");
            models.into_iter().filter(|model| model.id == "keep").collect()
        }
        fn stream(&self, _model: &Model, _context: &Context, _options: Option<StreamOptions>) -> AssistantMessageEventStream { maho_ai::utils::event_stream::create_assistant_message_event_stream() }
        fn stream_simple(&self, _model: &Model, _context: &Context, _options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream { maho_ai::utils::event_stream::create_assistant_message_event_stream() }
    }
    fn filtering_registry() -> crate::model_registry::ModelRegistry {
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions { providers: Some(vec![Arc::new(FilteringProvider) as Arc<dyn Provider>]), ..Default::default() });
        runtime.credentials.set("filtering", Some(serde_json::json!({"type":"api_key","key":"fixture"}))).expect("seed");
        crate::model_registry::ModelRegistry::new(runtime)
    }
    #[test]
    fn registry_available_applies_provider_filter_before_snapshot() {
        let registry = filtering_registry();
        assert!(!registry.model_runtime.has_availability_snapshot());
        let ids: Vec<_> = registry.get_available().into_iter().map(|model| model.id).collect();
        assert_eq!(ids, ["keep"]);
    }
    #[tokio::test]
    async fn registry_available_reads_runtime_snapshot_after_refresh() {
        let registry = filtering_registry();
        let async_ids: Vec<_> = registry.model_runtime.get_available(None).await.into_iter().map(|model| model.id).collect();
        assert!(registry.model_runtime.has_fresh_availability_snapshot());
        assert!(registry.model_runtime.has_configured_auth("filtering"));
        let sync_ids: Vec<_> = registry.get_available().into_iter().map(|model| model.id).collect();
        assert_eq!((async_ids, sync_ids), (vec!["keep".to_owned()], vec!["keep".to_owned()]));
    }
    #[test]
    fn register_provider_provisionally_marks_configured_provider() {
        let (_dir, mut runtime) = configured_runtime(r#"{"providers":{}}"#);
        runtime.register_provider("ext", crate::model_config_schema::ModelsJsonProvider {
            api: Some("openai-completions".into()), base_url: Some("http://localhost/v1".into()), api_key: Some("fixture-key".into()),
            models: Some(vec![crate::model_config_schema::ModelsJsonModel { id: "sol".into(), ..Default::default() }]), ..Default::default()
        }.into()).expect("register");
        assert!(runtime.has_configured_auth("ext"));
        assert!(runtime.get_available_snapshot().iter().any(|model| model.provider == "ext" && model.id == "sol"));
    }
    /// Answers selects with the first option id and every text/secret prompt with its own message,
    /// recording committed account receipts.
    #[derive(Default)]
    struct ScriptedInteraction { prompts: std::sync::Mutex<Vec<String>>, receipts: std::sync::Mutex<Vec<maho_ai::auth::types::AccountLoginReceipt>> }
    #[async_trait::async_trait]
    impl maho_ai::auth::types::AuthInteraction for ScriptedInteraction {
        fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> { None }
        fn on_account_committed(&self, receipt: maho_ai::auth::types::AccountLoginReceipt) { self.receipts.lock().expect("receipts").push(receipt); }
        async fn prompt(&self, prompt: maho_ai::auth::types::AuthPrompt) -> anyhow::Result<String> {
            use maho_ai::auth::types::AuthPromptKind;
            let (message, answer) = match prompt.kind {
                AuthPromptKind::Select { message, options } => (message, options.first().map(|option| option.id.clone()).unwrap_or_default()),
                AuthPromptKind::Text { message, .. } | AuthPromptKind::Secret { message, .. } | AuthPromptKind::ManualCode { message, .. } => (message.clone(), message),
            };
            self.prompts.lock().expect("prompts").push(message);
            Ok(answer)
        }
        fn notify(&self, _: maho_ai::auth::types::AuthEvent) {}
    }

    #[tokio::test]
    async fn api_key_login_runs_the_provider_flow_and_stores_its_env() {
        let runtime = builtin_runtime("cloudflare-ai-gateway");
        let interaction = std::sync::Arc::new(ScriptedInteraction::default());
        let credential = runtime.login("cloudflare-ai-gateway", maho_ai::auth::types::AuthType::ApiKey, interaction.clone()).await.expect("login");
        let maho_ai::auth::types::Credential::ApiKey(credential) = credential else { panic!("api key credential") };
        let env = credential.env.expect("provider login env");
        assert!(env.contains_key("CLOUDFLARE_ACCOUNT_ID") && env.contains_key("CLOUDFLARE_GATEWAY_ID"));
        assert!(interaction.prompts.lock().expect("prompts").len() > 1);
        assert!(runtime.credentials.get("cloudflare-ai-gateway").expect("stored")["env"]["CLOUDFLARE_ACCOUNT_ID"].is_string());
    }

    #[tokio::test]
    async fn bedrock_login_asks_for_the_method_first() {
        let runtime = builtin_runtime("amazon-bedrock");
        let interaction = std::sync::Arc::new(ScriptedInteraction::default());
        runtime.login("amazon-bedrock", maho_ai::auth::types::AuthType::ApiKey, interaction.clone()).await.expect("login");
        assert_eq!(interaction.prompts.lock().expect("prompts").first().map(String::as_str), Some("Select Amazon Bedrock authentication method:"));
    }

    #[tokio::test]
    async fn second_oauth_login_appends_a_slot_and_commits_its_receipt() {
        let (_dir, mut runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        runtime.register_provider("p", crate::provider_composer::ProviderConfigInput { oauth: Some(std::sync::Arc::new(TestOAuth)), ..Default::default() }).expect("register oauth");
        runtime.credentials.set("p", Some(serde_json::json!({"type":"oauth","access":"first","refresh":"first-r","expires":1.0}))).expect("seed");
        let interaction = std::sync::Arc::new(ScriptedInteraction::default());
        runtime.login("p", maho_ai::auth::types::AuthType::OAuth, interaction.clone()).await.expect("login");
        let stored = runtime.credentials.get("p").expect("stored");
        let names: Vec<_> = stored["accounts"].as_array().expect("accounts").iter().filter_map(|slot| slot["name"].as_str()).collect();
        assert_eq!(names, ["default", "login-2"]);
        let receipts = interaction.receipts.lock().expect("receipts");
        assert_eq!(receipts.iter().map(|receipt| receipt.name.as_str()).collect::<Vec<_>>(), ["login-2"]);
    }

    #[tokio::test]
    async fn logout_with_slot_id_removes_only_that_slot() {
        let (_dir, runtime) = configured_runtime(r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#);
        runtime.credentials.set("p", Some(serde_json::json!({"type":"api_key","key":"a","accounts":[{"name":"default","key":"a"},{"name":"work","key":"b"}]}))).expect("seed");
        runtime.logout("p", Some("work"), Default::default()).await.expect("slot logout");
        let stored = runtime.credentials.get("p").expect("remaining pool");
        assert_eq!(stored["accounts"].as_array().expect("accounts").len(), 1);
        assert_eq!(stored["key"], "a");
        runtime.logout("p", Some("default"), Default::default()).await.expect("last slot logout");
        assert!(runtime.credentials.get("p").is_none());
    }

    #[test]
    fn real_models_copy_qa_when_requested() {
        let config=match std::env::var("TASK17_MODELS_COPY") {
            Ok(path)=>ModelConfig::load_sync(Some(std::path::Path::new(&path))),
            Err(_)=>ModelConfig::parse(r#"{"providers":{"kiro-lb":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"claude-sonnet-5"},{"id":"claude-opus-5.5"}]}}}"#,"fixture/models.json"),
        };
        assert!(config.get_error().is_none(),"{:?}",config.get_error());
        let models=crate::provider_composer::compose_models("kiro-lb",&[],config.get_provider("kiro-lb"),None).expect("compose configured catalog");assert!(!models.is_empty());
        for model in models {println!("RESOLVE {} api={} baseUrl={}",model.id,model.api,model.base_url);}
        let (_dir,unknown)=configured_runtime(r#"{"providers":{"unknown-task-17":{"models":[{"id":"missing-api"}]}}}"#);
        println!("UNKNOWN {}",unknown.get_error().expect("error"));
        assert!(!unknown.get_models(None).iter().any(|m|m.provider=="unknown-task-17"));
    }
}
