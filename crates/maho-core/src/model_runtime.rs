//! Provider catalog composition and authenticated model execution.
use crate::model_config::ModelConfig;
use crate::provider_composer::{AuthStatus, ProviderConfigInput, compose_model_provider};
use indexmap::IndexMap;
use maho_ai::auth::context::DefaultAuthContext;
use maho_ai::auth::pool::slots::{PooledCredential, project_slot};
use maho_ai::auth::types::{
    ApiKeyCredential, AuthCheck, AuthOperationOptions, AuthType, Credential, CredentialInfo,
    CredentialStore, ProviderAuth,
};
use maho_ai::legacy_provider_ids::normalize_provider_id;
use maho_ai::models::{
    AuthResolution, AuthResolutionOverrides, CreateModelsOptions, Models, ModelsAuth, ModelsError,
    ModelsErrorCode, ModelsRefreshOptions, ModelsRefreshResult, ModelsRequestTransforms, Provider,
    create_models,
};
use maho_ai::types::{
    AssistantMessage, AssistantMessageEventStream, BoxFuture, Context, Model, SimpleStreamOptions,
    StreamOptions,
};
use maho_ai::utils::abort::{AbortSignal, operation_signal};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, RwLock},
};

#[derive(Default)]
pub struct CreateModelRuntimeOptions {
    pub models_path: Option<PathBuf>,
    pub auth_path: Option<PathBuf>,
    pub credentials: Option<Arc<crate::auth_storage::AuthStorage>>,
    pub providers: Option<Vec<Arc<dyn Provider>>>,
    /// senpi `agentDir`: where `credential-pool-state.json` lives (else beside `auth_path`).
    pub agent_dir: Option<PathBuf>,
    /// Dynamic catalog store; defaults to `models-store.json` beside `models.json` (in-memory without one).
    pub models_store: Option<Arc<dyn maho_ai::models_store::ModelsStore>>,
    pub models_store_path: Option<PathBuf>,
    pub catalog_base_url: Option<String>,
    /// Allow `create()` (and registration refreshes) to refresh catalogs over the network.
    pub allow_model_network: Option<bool>,
    /// Timeout for the create-time network refresh (default 15 s for registration refreshes).
    pub model_refresh_timeout_ms: Option<u64>,
    /// Skip the initial catalog refresh; static models remain available.
    pub refresh_on_create: Option<bool>,
    pub signal: Option<maho_ai::utils::abort::AbortSignal>,
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
            .and_then(|path| {
                crate::auth_storage::read_stored_credential(provider_id, &path.to_string_lossy())
            })
            .or_else(|| self.credentials.get(provider_id))
    }

    fn decode(value: serde_json::Value) -> anyhow::Result<Credential> {
        serde_json::from_value(value)
            .map_err(|error| anyhow::anyhow!("Invalid auth.json credential: {error}"))
    }
}

fn check_abort(options: &Option<AuthOperationOptions>) -> anyhow::Result<()> {
    if let Some(signal) = options.as_ref().and_then(|options| options.signal.as_ref()) {
        signal
            .throw_if_aborted()
            .map_err(|reason| anyhow::anyhow!("{reason}"))?;
    }
    Ok(())
}

type ModifyFn = Box<
    dyn FnOnce(Option<Credential>) -> BoxFuture<'static, anyhow::Result<Option<Credential>>> + Send,
>;

#[async_trait::async_trait]
impl CredentialStore for AuthStorageCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        check_abort(&options)?;
        if let Some(key) = self.credentials.runtime_api_key(provider_id) {
            return Ok(Some(Credential::ApiKey(ApiKeyCredential {
                key: Some(key),
                env: None,
                ..Default::default()
            })));
        }
        let Some(raw) = self.raw(provider_id) else {
            return Ok(None);
        };
        let credential = Self::decode(raw)?;
        check_abort(&options)?;
        Ok(Some(match credential {
            Credential::ApiKey(ApiKeyCredential {
                key: Some(key),
                env,
                extra,
            }) => {
                let lookup: Option<std::collections::HashMap<String, String>> = env
                    .as_ref()
                    .map(|env| env.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                let key =
                    crate::resolve_config_value::resolve_config_value(&key, lookup.as_ref()).await;
                Credential::ApiKey(ApiKeyCredential { key, env, extra })
            }
            other => other,
        }))
    }

    async fn list(
        &self,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Vec<CredentialInfo>> {
        check_abort(&options)?;
        Ok(self
            .credentials
            .list()
            .into_iter()
            .map(|(provider_id, kind)| CredentialInfo {
                provider_id,
                credential_type: match kind {
                    crate::auth_storage::CredentialKind::ApiKey => {
                        maho_ai::auth::types::CredentialType::ApiKey
                    }
                    crate::auth_storage::CredentialKind::Oauth => {
                        maho_ai::auth::types::CredentialType::OAuth
                    }
                },
            })
            .collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyFn,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        check_abort(&options)?;
        let _mutation = self.credentials.account_mutation.lock().await;
        let stored = self
            .credentials
            .modify_credential(
                provider_id,
                move |current: Option<serde_json::Value>| async move {
                    let current = current
                        .map(Self::decode)
                        .transpose()
                        .map_err(|error| error.to_string())?;
                    let next = f(current).await.map_err(|error| error.to_string())?;
                    next.map(|next| serde_json::to_value(&next).map_err(|error| error.to_string()))
                        .transpose()
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        stored.map(Self::decode).transpose()
    }

    async fn modify_or_delete(
        &self,
        provider_id: &str,
        f: ModifyFn,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        check_abort(&options)?;
        let stored = self.credentials.modify_or_delete_credential(
            provider_id,
            move |current: Option<serde_json::Value>| async move {
                let current = current.map(Self::decode).transpose().map_err(|error| error.to_string())?;
                let next = f(current).await.map_err(|error| error.to_string())?;
                next.map(|next| serde_json::to_value(&next).map_err(|error| error.to_string())).transpose()
            },
        ).await.map_err(|error| anyhow::anyhow!(error))?;
        stored.map(Self::decode).transpose()
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<()> {
        check_abort(&options)?;
        let _mutation = self.credentials.account_mutation.lock().await;
        self.credentials
            .delete(provider_id)
            .map_err(|error| anyhow::anyhow!(error))
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
    let config = config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get_provider(id)
        .cloned();
    let extension = extensions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(id)
        .cloned();
    let base = provider.auth();
    let oauth = extension
        .as_ref()
        .and_then(|e| e.oauth.clone())
        .or_else(|| base.and_then(|auth| auth.oauth.clone()))
        .or_else(|| builtin_oauth(id));
    // Providers registered without declared auth keep the env-var lookup senpi's builtin
    // factories declare through `envApiKeyAuth`.
    let inherited = base.and_then(|auth| auth.api_key.clone()).or_else(|| {
        maho_ai::env_api_keys::get_api_key_env_vars(id).map(|vars| {
            Arc::new(maho_ai::auth::helpers::env_api_key_auth("API key", &vars))
                as Arc<dyn maho_ai::auth::types::ApiKeyAuth>
        })
    }).or_else(|| {
        let extension_config = extension.as_ref().map(|extension| &extension.config);
        let has_key = crate::provider_api_key_auth::configured_api_key(config.as_ref(), extension_config).is_some();
        let has_headers = crate::provider_header_auth::header_auth_resolution_source(
            config.as_ref().and_then(|config| config.headers.as_ref()),
            extension_config.and_then(|config| config.headers.as_ref()),
        ).is_some();
        if has_key || has_headers { None } else {
            extension.as_ref().and_then(|extension| extension.ambient_auth.clone())
        }
    });
    let api_key = crate::provider_api_key_auth::compose_api_key_auth(
        id,
        inherited,
        oauth.as_ref(),
        config.as_ref(),
        extension.as_ref().map(|e| &e.config),
    );
    let oauth = crate::provider_composer::compose_oauth_auth(
        id,
        oauth,
        config.as_ref(),
        extension.as_ref().map(|e| &e.config),
    )
    .map(|auth| Arc::new(auth) as Arc<dyn maho_ai::auth::types::OAuthAuth>);
    ProviderAuth { api_key, oauth }
}

impl RuntimeAuth {
    fn provider_auth(&self, provider: &dyn Provider) -> ProviderAuth {
        compose_provider_auth(&self.config, &self.extensions, provider)
    }

    /// A slot of a stored api-key pool is projected and resolved as that slot's explicit key.
    /// `Some(None)` = the slot is missing; `None` = not an api-key pool request.
    fn api_key_slot(
        &self,
        provider_id: &str,
        overrides: &AuthResolutionOverrides,
    ) -> Option<Option<AuthResolutionOverrides>> {
        let slot_name = overrides.slot_name.as_deref()?;
        let raw = self.credentials.get(provider_id)?;
        if crate::auth_storage::credential_kind(&raw)
            != Some(crate::auth_storage::CredentialKind::ApiKey)
        {
            return None;
        }
        let pooled: Option<PooledCredential> = serde_json::from_value(raw).ok();
        let Some(Credential::ApiKey(slot)) = project_slot(pooled.as_ref(), slot_name) else {
            return Some(None);
        };
        let Some(key) = slot.key else {
            return Some(None);
        };
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
    fn resolve<'a>(
        &'a self,
        provider: &'a dyn Provider,
        overrides: &'a AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
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
                provider.id(),
                &provider_auth,
                &self.store,
                &DefaultAuthContext,
                Some(overrides),
                &signal,
            )
            .await
            .map_err(models_error)?;
            Ok(result.map(|result| AuthResolution {
                ambient: result.ambient,
                auth: crate::provider_api_key_auth::model_auth_to_result(result.auth),
                env: result.env,
            }))
        })
    }
    fn read_credential<'a>(
        &'a self,
        id: &'a str,
        _signal: &'a maho_ai::utils::abort::AbortSignal,
    ) -> BoxFuture<'a, Result<Option<serde_json::Value>, ModelsError>> {
        Box::pin(async move { Ok(self.credentials.get(id)) })
    }
    fn refresh_credential<'a>(
        &'a self,
        provider: &'a dyn Provider,
        stored: Option<&'a serde_json::Value>,
        signal: &'a maho_ai::utils::abort::AbortSignal,
    ) -> BoxFuture<'a, Result<Option<serde_json::Value>, ModelsError>> {
        Box::pin(async move {
            let id = provider.id();
            // The collection hands over the raw stored value; senpi's `readCredential` yields the
            // store's resolved view (api-key config values resolved), so re-read through the store.
            let stored = match stored {
                Some(_) => self
                    .store
                    .read(
                        id,
                        Some(AuthOperationOptions {
                            signal: Some(signal.clone()),
                        }),
                    )
                    .await
                    .map_err(|error| {
                        ModelsError::with_cause(
                            ModelsErrorCode::Auth,
                            format!("Credential store read failed for {id}"),
                            &error.to_string(),
                        )
                    })?,
                None => None,
            };
            let provider_auth = self.provider_auth(provider);
            let credential = maho_ai::auth::refresh_credential::resolve_refresh_credential(
                id,
                &provider_auth,
                &self.store,
                &DefaultAuthContext,
                stored.as_ref(),
                signal,
            )
            .await
            .map_err(models_error)?;
            credential
                .map(|credential| {
                    serde_json::to_value(credential)
                        .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.to_string()))
                })
                .transpose()
        })
    }
    /// senpi `checkProviderAuth`: side-effect-free; never refreshes OAuth or runs `!command` keys.
    fn check<'a>(
        &'a self,
        provider: &'a dyn Provider,
        signal: &'a maho_ai::utils::abort::AbortSignal,
    ) -> BoxFuture<'a, Result<Option<AuthCheck>, ModelsError>> {
        Box::pin(async move {
            let provider_auth = self.provider_auth(provider);
            let id = provider.id();
            // senpi `readCredential`: the store's view, with api-key config values resolved.
            let credential = self
                .store
                .read(
                    id,
                    Some(AuthOperationOptions {
                        signal: Some(signal.clone()),
                    }),
                )
                .await
                .map_err(|error| {
                    ModelsError::with_cause(
                        ModelsErrorCode::Auth,
                        format!("Credential store read failed for {id}"),
                        &error.to_string(),
                    )
                })?;
            let check_error = |kind: &str, error: anyhow::Error| {
                ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("{kind} auth check failed for provider {id}: {error}"),
                )
            };
            if let Some(Credential::OAuth(credential)) = &credential {
                let Some(oauth) = &provider_auth.oauth else {
                    return Ok(None);
                };
                return oauth
                    .check(&DefaultAuthContext, Some(credential), signal)
                    .await
                    .map_err(|error| check_error("OAuth", error));
            }
            if let Some(api_key) = &provider_auth.api_key {
                let stored = credential.as_ref().and_then(Credential::as_api_key);
                if api_key.has_check() {
                    if let Some(check) = api_key
                        .check(&DefaultAuthContext, stored, signal)
                        .await
                        .map_err(|error| check_error("API key", error))?
                    {
                        return Ok(Some(check));
                    }
                } else if let Some(result) = api_key
                    .resolve(&DefaultAuthContext, stored, signal)
                    .await
                    .map_err(|error| check_error("API key", error))?
                {
                    return Ok(Some(AuthCheck {
                        ambient: result.ambient,
                        source: result.source,
                        auth_type: AuthType::ApiKey,
                    }));
                }
            }
            match &provider_auth.oauth {
                Some(oauth) => oauth
                    .check(&DefaultAuthContext, None, signal)
                    .await
                    .map_err(|error| check_error("OAuth", error)),
                None => Ok(None),
            }
        })
    }
}

fn builtin_oauth(id: &str) -> Option<Arc<dyn maho_ai::auth::types::OAuthAuth>> {
    use maho_ai::auth::oauth;
    match id {
        "anthropic" | "anthropic-subscription" => Some(oauth::anthropic::anthropic_oauth()),
        "chatgpt-subscription" => Some(oauth::chatgpt_subscription::chatgpt_subscription_oauth()),
        "github-copilot" => Some(oauth::github_copilot::github_copilot_oauth()),
        "openrouter" => Some(oauth::openrouter::open_router_oauth()),
        "kimi-coding" => Some(oauth::kimi_coding::kimi_coding_oauth()),
        "xai" => Some(oauth::xai::xai_oauth()),
        "cursor" => Some(Arc::new(oauth::cursor::CursorOAuth::new())),
        "devin" => Some(oauth::devin::devin_oauth()),
        "radius" => Some(oauth::radius::radius_oauth(
            "Radius",
            &maho_ai::providers::radius_config::normalize_radius_gateway_url(
                maho_ai::providers::radius_config::DEFAULT_RADIUS_GATEWAY,
            ),
        )),
        _ => None,
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
    error: Option<ModelsError>,
    /// Bumped whenever `snapshot` changes, so readers can recompute derived values (the footer's
    /// provider count) on change instead of on every frame.
    revision: u64,
    busy_retries: u32,
    busy_retry_pending: bool,
}

#[derive(Clone)]
pub struct ModelRuntime {
    models: Models,
    config: Arc<RwLock<ModelConfig>>,
    builtins: IndexMap<String, Arc<dyn Provider>>,
    native: IndexMap<String, Arc<dyn Provider>>,
    extensions: Arc<RwLock<IndexMap<String, ProviderConfigInput>>>,
    errors: Arc<RwLock<IndexMap<String, String>>>,
    models_path: Option<PathBuf>,
    pub credentials: Arc<crate::auth_storage::AuthStorage>,
    availability: Arc<RwLock<AvailabilityState>>,
    /// senpi `credentialOperations`: login/logout/runtime-key changes run one at a time per provider.
    credential_operations:
        Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// senpi `providerSemaphores`, limited by the bound settings' `providers.<id>.maxConcurrency`.
    provider_semaphores: Arc<crate::provider_concurrency::ProviderSemaphores>,
    provider_settings: Arc<RwLock<Option<serde_json::Value>>>,
    provider_settings_subscription:
        Arc<std::sync::Mutex<Option<crate::settings_manager::ProviderSettingsSubscription>>>,
    /// senpi `poolStatePath` repository: the credential-pool sidecar written by rotating requests.
    pool_repository: Arc<crate::credential_pool::state_store::CredentialSlotRepository>,
    outer_slot_state_retired: Arc<tokio::sync::Mutex<HashSet<String>>>,
    model_network_enabled: bool,
    model_refresh_timeout_ms: u64,
}

/// senpi `CredentialSynchronizationError`: the credential change committed but recomposing or
/// refreshing the provider afterwards failed.
pub fn is_credential_synchronization_error(error: &ModelsError) -> bool {
    error.message.starts_with("Credential ")
        && error.message.contains(", but local synchronization failed")
}

impl ModelRuntime {
    pub fn create_sync(options: CreateModelRuntimeOptions) -> Result<Self, ModelsError> {
        // senpi `model-runtime.ts:73`: the product's identity rides outgoing requests.
        maho_ai::wire_identity::set_wire_identity(Some(&crate::config::brand().user_agent));
        let credentials = options.credentials.unwrap_or_else(|| {
            Arc::new(options.auth_path.as_ref().map_or_else(
                || crate::auth_storage::AuthStorage::in_memory(Default::default()),
                |path| crate::auth_storage::AuthStorage::create(&path.to_string_lossy()),
            ))
        });
        let config = Arc::new(RwLock::new(ModelConfig::load_sync_with_migration(
            options.models_path.as_deref(),
            !crate::lockfile_policy::facts_lock_broker_enabled(),
        )));
        let extensions = Arc::new(RwLock::new(IndexMap::new()));
        let pool_state_path = options
            .agent_dir
            .as_ref()
            .map(|dir| dir.join("credential-pool-state.json"))
            .or_else(|| {
                options
                    .auth_path
                    .as_ref()
                    .and_then(|path| path.parent())
                    .map(|dir| dir.join("credential-pool-state.json"))
            });
        let pool_repository = Arc::new(pool_state_path.map_or_else(
            crate::credential_pool::state_store::CredentialSlotRepository::default_path,
            |path| {
                crate::credential_pool::state_store::CredentialSlotRepository::new(
                    &path.to_string_lossy(),
                )
            },
        ));
        let store: Arc<dyn CredentialStore> = Arc::new(AuthStorageCredentialStore {
            credentials: credentials.clone(),
            auth_path: options.auth_path,
        });
        let auth = RuntimeAuth {
            config: config.clone(),
            extensions: extensions.clone(),
            credentials: credentials.clone(),
            store,
        };
        let provider_settings: Arc<RwLock<Option<serde_json::Value>>> = Arc::new(RwLock::new(None));
        let limits = provider_settings.clone();
        let provider_semaphores = Arc::new(crate::provider_concurrency::ProviderSemaphores::new(
            move |id| {
                crate::settings_manager::provider_concurrency_limit(
                    limits.read().unwrap_or_else(|p| p.into_inner()).as_ref(),
                    id,
                )
                .unwrap_or(0)
            },
        ));
        // senpi: `FileModelsStore` beside models.json, else an in-memory store.
        let models_store: Arc<dyn maho_ai::models_store::ModelsStore> =
            match (options.models_store, &options.models_path) {
                (Some(store), _) => store,
                (None, Some(models_path)) => {
                    let path = options.models_store_path.clone().unwrap_or_else(|| {
                        models_path.parent().map_or_else(
                            || PathBuf::from("models-store.json"),
                            |dir| dir.join("models-store.json"),
                        )
                    });
                    Arc::new(crate::models_store::FileModelsStore::new(Some(
                        &path.to_string_lossy(),
                    )))
                }
                (None, None) => {
                    Arc::new(crate::models_store::InMemoryCodingAgentModelsStore::new())
                }
            };
        // Builtins without their own refresh get the persisted remote catalog overlay.
        let generated_at = maho_ai::providers::all::get_builtin_model_data_generated_at();
        let catalog_base_url = options.catalog_base_url.clone();
        let providers = match options.providers {
            Some(providers) => providers,
            None => {
                maho_ai::providers::all::builtin_providers()?
                    .into_iter()
                    .map(|provider| {
                        if provider.supports_refresh()
                            || !crate::remote_catalog_provider::remote_catalog_serves_provider(
                                provider.id(),
                                catalog_base_url.as_deref(),
                            )
                        {
                            provider
                        } else {
                            crate::remote_catalog_provider::with_remote_catalog(
                                provider,
                                catalog_base_url.as_deref(),
                                generated_at,
                            )
                        }
                    })
                    .collect()
            }
        };
        let builtins: IndexMap<String, Arc<dyn Provider>> = providers
            .into_iter()
            .map(|p| (p.id().to_owned(), p))
            .collect();
        let model_network_enabled = options.allow_model_network.unwrap_or(false);
        let model_refresh_timeout_ms = options.model_refresh_timeout_ms.unwrap_or(15_000);
        let runtime = Self {
            models: create_models(Some(CreateModelsOptions {
                auth: Some(Arc::new(auth)),
                models_store: Some(models_store),
            })),
            config,
            builtins,
            native: IndexMap::new(),
            extensions,
            errors: Arc::new(RwLock::new(IndexMap::new())),
            models_path: options.models_path,
            credentials,
            credential_operations: Arc::new(
                std::sync::Mutex::new(std::collections::HashMap::new()),
            ),
            provider_semaphores,
            provider_settings,
            provider_settings_subscription: Arc::new(std::sync::Mutex::new(None)),
            pool_repository,
            outer_slot_state_retired: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            model_network_enabled,
            model_refresh_timeout_ms,
            availability: Arc::new(RwLock::new(AvailabilityState::default())),
        };
        runtime.rebuild_providers();
        Ok(runtime)
    }
    /// senpi `ModelRuntime.create`: construct, then (unless `refresh_on_create: false`) restore the
    /// stored catalogs and, when network is allowed and not `OFFLINE`, refresh them within the
    /// create-time timeout.
    pub async fn create(mut options: CreateModelRuntimeOptions) -> Result<Self, ModelsError> {
        let offline = crate::brand::env_value("OFFLINE", &crate::config::current_env()).is_some();
        let refresh_from_network = !offline && options.allow_model_network == Some(true);
        options.allow_model_network = Some(refresh_from_network);
        let (timeout_ms, signal, refresh_on_create) = (
            options.model_refresh_timeout_ms,
            options.signal.clone(),
            options.refresh_on_create,
        );
        let mut runtime = Self::create_sync(options)?;
        if refresh_on_create != Some(false) {
            let controller = maho_ai::utils::abort::AbortController::new();
            let combined = maho_ai::utils::abort_signals::combine_abort_signals(&[
                signal,
                Some(controller.signal()),
            ]);
            let refresh = runtime.refresh(ModelsRefreshOptions {
                allow_network: Some(refresh_from_network),
                signal: combined.signal.clone(),
                ..Default::default()
            });
            match timeout_ms.filter(|_| refresh_from_network) {
                Some(ms) => {
                    if tokio::time::timeout(std::time::Duration::from_millis(ms), refresh)
                        .await
                        .is_err()
                    {
                        controller.abort(None);
                    }
                }
                None => {
                    refresh.await;
                }
            }
        }
        Ok(runtime)
    }
    /// senpi `setSettingsManager`: provider concurrency limits follow the settings, resizing live
    /// semaphores whenever the merged settings change.
    pub fn set_settings_manager(&self, settings: &crate::settings_manager::SettingsManager) {
        let snapshot = self.provider_settings.clone();
        let semaphores = self.provider_semaphores.clone();
        let resize = move |settings: &crate::settings_manager::SettingsManager| {
            *snapshot.write().unwrap_or_else(|p| p.into_inner()) = settings.get_provider_settings();
            semaphores.resize_all();
        };
        resize(settings);
        let subscription = settings.subscribe_to_provider_settings(Arc::new(resize));
        *self
            .provider_settings_subscription
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(subscription);
    }
    /// The provider/auth registry this runtime resolves against (pinned `models`). Exposed so a
    /// caller (e.g. the harness constructor) can hand the same registry to a `Models` consumer
    /// without rebuilding it through `create_models`.
    pub fn models(&self) -> &Models {
        &self.models
    }
    fn recompose_provider(&self, id: &str) {
        let id = normalize_provider_id(id);
        let config = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        self.errors
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .shift_remove(&id);
        if config.is_provider_disabled(&id) {
            self.models.delete_provider(&id);
            return;
        }
        let base = self
            .native
            .get(&id)
            .or_else(|| self.builtins.get(&id))
            .cloned();
        let extension = self
            .extensions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .cloned();
        if config.get_provider(&id).is_none() && extension.is_none() {
            match base {
                Some(base) => self.models.set_provider(base),
                None => self.models.delete_provider(&id),
            }
            return;
        }
        match compose_model_provider(&id, base.clone(), &config, extension.as_ref()) {
            Ok(provider) => self.models.set_provider(provider),
            Err(error) => {
                self.errors
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone(), error);
                match base {
                    Some(base) => self.models.set_provider(base),
                    None => self.models.delete_provider(&id),
                }
            }
        }
    }
    fn rebuild_providers(&self) {
        self.models.clear_providers();
        self.errors
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        let mut ids: indexmap::IndexSet<String> = self
            .builtins
            .keys()
            .chain(self.native.keys())
            .cloned()
            .collect();
        ids.extend(
            self.config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .get_provider_ids()
                .into_iter()
                .map(str::to_owned),
        );
        ids.extend(
            self.extensions
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .keys()
                .cloned(),
        );
        for id in ids {
            self.recompose_provider(&id);
        }
        self.update_model_snapshot();
    }
    fn availability_state(&self) -> std::sync::RwLockWriteGuard<'_, AvailabilityState> {
        self.availability.write().unwrap_or_else(|p| p.into_inner())
    }
    fn update_model_snapshot(&self) {
        let all = self.models.get_models(None);
        let mut guard = self.availability_state();
        let snapshot = &mut guard.snapshot;
        snapshot.available = all
            .iter()
            .filter(|model| snapshot.configured_providers.contains(&model.provider))
            .cloned()
            .collect();
        snapshot.all = all;
        guard.revision += 1;
    }
    async fn run_availability_refresh(
        &self,
        seq: u64,
        error_seq: u64,
        signal: AbortSignal,
    ) -> Result<(), ModelsError> {
        let providers = self.models.get_providers();
        let available = self
            .models
            .get_available(None, Some(signal.clone()))
            .await?;
        let checks = futures::future::join_all(providers.iter().map(|provider| {
            let id = provider.id().to_owned();
            let options = AuthOperationOptions {
                signal: Some(signal.clone()),
            };
            async move {
                let check = self.check_auth(&id, Some(options)).await;
                (id, check)
            }
        }))
        .await;
        let credentials = self
            .list_credentials(Some(AuthOperationOptions {
                signal: Some(signal.clone()),
            }))
            .await?;
        let mut auth = HashMap::new();
        for (id, check) in checks {
            if let Some(check) = check? {
                auth.insert(id, check);
            }
        }
        let all = self.models.get_models(None);
        let mut guard = self.availability_state();
        if seq != guard.refresh_seq {
            return Ok(());
        }
        guard.snapshot = ModelRuntimeSnapshot {
            all,
            available,
            configured_providers: auth.keys().cloned().collect(),
            stored_providers: credentials
                .into_iter()
                .map(|entry| entry.provider_id)
                .collect(),
            auth,
        };
        guard.initialized = true;
        guard.busy_retries = 0;
        guard.revision += 1;
        if error_seq == guard.error_seq {
            guard.error = None;
        }
        Ok(())
    }
    fn schedule_busy_availability_retry(&self) {
        let delay = {
            let mut state = self.availability_state();
            if state.busy_retry_pending || state.busy_retries >= 5 { return; }
            let delay = 250_u64 << state.busy_retries;
            state.busy_retries += 1;
            state.busy_retry_pending = true;
            delay
        };
        let runtime = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            runtime.availability_state().busy_retry_pending = false;
            // The pass records its error before the retry task returns.
            let _ = runtime.queue_availability_refresh(None).await;
        });
    }
    fn queue_availability_refresh(&self, signal: Option<AbortSignal>) -> BoxFuture<'_, Result<(), ModelsError>> {
        Box::pin(async move {

        let (seq, error_seq) = {
            let mut guard = self.availability_state();
            guard.refresh_seq += 1;
            for provider_seq in guard.provider_seq.values_mut() {
                *provider_seq += 1;
            }
            guard.error_seq += 1;
            (guard.refresh_seq, guard.error_seq)
        };
        let signal = operation_signal(signal);
        let result = self
            .run_availability_refresh(seq, error_seq, signal.clone())
            .await;
        if let Err(error) = &result {
            let mut guard = self.availability_state();
            if error_seq == guard.error_seq && !signal.aborted() {
                guard.error = Some(error.clone());
            }
        }
        if result.as_ref().is_err_and(|error| error.message.to_ascii_lowercase().contains("busy")) {
            self.schedule_busy_availability_retry();
        }
        result
        })
    }
    async fn refresh_provider_availability(
        &self,
        provider_id: &str,
        signal: AbortSignal,
    ) -> Result<(), ModelsError> {
        // Invalidate any full availability pass that started before this credential change.
        let (provider_seq, error_seq) = {
            let mut guard = self.availability_state();
            guard.refresh_seq += 1;
            let provider_seq = guard.provider_seq.get(provider_id).copied().unwrap_or(0) + 1;
            guard
                .provider_seq
                .insert(provider_id.to_owned(), provider_seq);
            guard.error_seq += 1;
            (provider_seq, guard.error_seq)
        };
        let result = async {
            let available = self
                .models
                .get_available(Some(provider_id), Some(signal.clone()))
                .await?;
            let auth = self
                .check_auth(
                    provider_id,
                    Some(AuthOperationOptions {
                        signal: Some(signal.clone()),
                    }),
                )
                .await?;
            signal.throw_if_aborted().map_err(|reason| ModelsError::new(ModelsErrorCode::Auth, reason.message))?;
            let credential = self.credentials.get(provider_id);
            let all = self.models.get_models(None);
            let mut guard = self.availability_state();
            if guard.provider_seq.get(provider_id) != Some(&provider_seq) {
                return Ok(());
            }
            let state = &mut *guard;
            let snapshot = &mut state.snapshot;
            match auth {
                Some(auth) => {
                    snapshot.configured_providers.insert(provider_id.to_owned());
                    snapshot.auth.insert(provider_id.to_owned(), auth);
                }
                None => {
                    snapshot.configured_providers.remove(provider_id);
                    snapshot.auth.remove(provider_id);
                }
            }
            if credential.is_some() {
                snapshot.stored_providers.insert(provider_id.to_owned());
            } else {
                snapshot.stored_providers.remove(provider_id);
            }
            let available_by_id: HashMap<(String, String), Model> = snapshot
                .available
                .iter()
                .filter(|model| model.provider != provider_id)
                .chain(available.iter())
                .map(|model| ((model.provider.clone(), model.id.clone()), model.clone()))
                .collect();
            snapshot.available = all
                .iter()
                .filter_map(|model| {
                    available_by_id
                        .get(&(model.provider.clone(), model.id.clone()))
                        .cloned()
                })
                .collect();
            snapshot.all = all;
            state.revision += 1;
            if error_seq == state.error_seq {
                state.error = None;
            }
            Ok::<(), ModelsError>(())
        }
        .await;
        if let Err(error) = &result {
            let mut guard = self.availability_state();
            if guard.provider_seq.get(provider_id) == Some(&provider_seq)
                && error_seq == guard.error_seq
                && !signal.aborted()
            {
                guard.error = Some(error.clone());
            }
        }
        result
    }
    pub fn get_error(&self) -> Option<String> {
        let mut errors: Vec<String> = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get_error()
            .map(str::to_owned)
            .into_iter()
            .collect();
        errors.extend(
            self.errors
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .map(|(id, error)| format!("Provider \"{id}\": {error}")),
        );
        if let Some(error) = self.availability_state().error.clone() {
            errors.push(format!("Availability refresh: {error}"));
        }
        if errors.is_empty() {
            None
        } else {
            Some(errors.join("\n\n"))
        }
    }
    pub fn get_warnings(&self) -> Vec<String> {
        self.config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get_warnings()
            .to_vec()
    }
    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        self.models.get_models(provider)
    }
    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.models.get_model(&normalize_provider_id(provider), id)
    }
    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.models.get_provider(&normalize_provider_id(id))
    }
    pub fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
        self.models.get_providers()
    }
    /// senpi `getAvailable`: a provider-scoped read goes straight to the registry; a full read
    /// runs one availability pass and returns the refreshed snapshot. A failed pass is recorded
    /// for `get_error` and rethrown without retrying the registry or returning stale models.
    pub async fn get_available(
        &self,
        provider: Option<&str>,
    ) -> Result<Vec<Model>, ModelsError> {
        if let Some(provider) = provider {
            let error_seq = {
                let mut guard = self.availability_state();
                guard.error_seq += 1;
                guard.error_seq
            };
            let result = self.models.get_available(Some(provider), None).await;
            let mut guard = self.availability_state();
            if error_seq == guard.error_seq {
                guard.error = result.as_ref().err().cloned();
            }
            return result;
        }
        self.queue_availability_refresh(None).await?;
        Ok(self.get_available_snapshot())
    }
    pub fn get_available_snapshot(&self) -> Vec<Model> {
        self.availability_state().snapshot.available.clone()
    }
    /// Distinct providers in the availability snapshot, counted under the lock without cloning models.
    pub fn available_provider_count(&self) -> usize {
        self.availability_state()
            .snapshot
            .available
            .iter()
            .map(|model| model.provider.as_str())
            .collect::<HashSet<_>>()
            .len()
    }
    /// True only when the most recent availability refresh completed without error.
    pub fn has_fresh_availability_snapshot(&self) -> bool {
        let guard = self.availability_state();
        guard.initialized && guard.error.is_none()
    }
    pub fn has_availability_snapshot(&self) -> bool {
        self.availability_state().initialized
    }
    /// Changes whenever the availability snapshot changes (refresh, provider (un)registration).
    pub fn availability_revision(&self) -> u64 {
        self.availability
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .revision
    }
    pub fn has_configured_auth(&self, provider_id: &str) -> bool {
        self.availability_state()
            .snapshot
            .configured_providers
            .contains(provider_id)
    }
    pub fn get_provider_auth_status(&self, id: &str) -> AuthStatus {
        // senpi `getProviderAuthStatus`: a non-persistent runtime API key wins, so the
        // credential-policy runtime-key branch can fire; the stored credential is next.
        if self.credentials.has_runtime_api_key(id) {
            return AuthStatus {
                ambient: false,
                configured: true,
                source: Some("runtime".into()),
                label: None,
            };
        }
        if self.credentials.get(id).is_some() {
            return AuthStatus {
                ambient: false,
                configured: true,
                source: Some("stored".into()),
                label: None,
            };
        }
        let config = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get_provider(id)
            .cloned();
        let extension = self
            .extensions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned();
        if let Some(status) = crate::provider_api_key_auth::configured_request_auth_status(
            config.as_ref(),
            extension.as_ref().map(|e| &e.config),
        ) {
            return status;
        }
        let check = self.availability_state().snapshot.auth.get(id).cloned();
        match check {
            Some(check) => AuthStatus {
                ambient: check.ambient,
                configured: true,
                source: Some("environment".into()),
                label: check.source,
            },
            None => AuthStatus {
                ambient: false,
                configured: false,
                source: None,
                label: None,
            },
        }
    }
    /// Consent gate for credential resolution: runs before `Models::get_auth`, which refreshes
    /// expired OAuth credentials over the network.
    fn consent_error(&self, provider: &str, slot_name: Option<&str>) -> Option<ModelsError> {
        crate::provider_consent::consent_failure(&self.credentials, provider, slot_name)
            .map(|message| ModelsError::new(ModelsErrorCode::ConsentRequired, message))
    }
    pub async fn get_auth(&self, id: &str) -> Result<Option<AuthResolution>, ModelsError> {
        if let Some(error) = self.consent_error(id, None) {
            return Err(error);
        }
        self.get_auth_unchecked(id).await
    }
    /// Un-gated `get_auth`, only for CLI auth diagnostics (`mhc auth check`/`print`) so users can
    /// inspect credentials before consenting. Never use on a request path (SD-4).
    pub async fn get_auth_unchecked(&self, id: &str) -> Result<Option<AuthResolution>, ModelsError> {
        self.models
            .get_auth(id, &AuthResolutionOverrides::default())
            .await
    }
    pub async fn get_auth_with_overrides(
        &self,
        id: &str,
        overrides: &AuthResolutionOverrides,
    ) -> Result<Option<AuthResolution>, ModelsError> {
        if let Some(error) = self.consent_error(id, overrides.slot_name.as_deref()) {
            return Err(error);
        }
        self.get_auth_with_overrides_unchecked(id, overrides).await
    }
    /// Un-gated `get_auth_with_overrides`, only for CLI auth diagnostics (SD-4).
    pub async fn get_auth_with_overrides_unchecked(
        &self,
        id: &str,
        overrides: &AuthResolutionOverrides,
    ) -> Result<Option<AuthResolution>, ModelsError> {
        self.models.get_auth(id, overrides).await
    }
    pub async fn list_credentials(
        &self,
        options: Option<maho_ai::auth::types::AuthOperationOptions>,
    ) -> Result<Vec<maho_ai::auth::types::CredentialInfo>, ModelsError> {
        if let Some(signal) = options.and_then(|options| options.signal) {
            signal
                .throw_if_aborted()
                .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.message))?;
        }
        Ok(self
            .credentials
            .list()
            .into_iter()
            .map(|(provider_id, kind)| maho_ai::auth::types::CredentialInfo {
                provider_id,
                credential_type: match kind {
                    crate::auth_storage::CredentialKind::ApiKey => {
                        maho_ai::auth::types::CredentialType::ApiKey
                    }
                    crate::auth_storage::CredentialKind::Oauth => {
                        maho_ai::auth::types::CredentialType::OAuth
                    }
                },
            })
            .collect())
    }
    /// senpi `ModelRuntime.checkAuth` -> `Models.checkAuth`: side-effect-free provider availability.
    pub async fn check_auth(
        &self,
        id: &str,
        options: Option<maho_ai::auth::types::AuthOperationOptions>,
    ) -> Result<Option<maho_ai::auth::types::AuthCheck>, ModelsError> {
        let signal =
            maho_ai::utils::abort::operation_signal(options.and_then(|options| options.signal));
        signal
            .throw_if_aborted()
            .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.message))?;
        self.models
            .check_auth(&normalize_provider_id(id), &signal)
            .await
    }
    /// Pinned `ModelRuntime.login` -> `Models.login` (`models.ts:583-637`): run the provider's
    /// composed OAuth or api-key login flow, append the credential as a login slot
    /// (`appendLoginSlot`) and report the committed account through `on_account_committed`.
    pub async fn login(
        &self,
        provider_id: &str,
        auth_type: maho_ai::auth::types::AuthType,
        interaction: std::sync::Arc<dyn maho_ai::auth::types::AuthInteraction>,
    ) -> Result<maho_ai::auth::types::Credential, ModelsError> {
        let id = normalize_provider_id(provider_id);
        let signal = maho_ai::utils::abort::operation_signal(interaction.signal());
        self.enqueue_credential_operation(&id, &signal, async {
            let credential = self
                .login_unsynchronized(&id, auth_type, interaction.clone())
                .await?;
            self.synchronize_credential_state(&id, "login", &signal)
                .await?;
            Ok(credential)
        })
        .await
    }

    async fn login_unsynchronized(
        &self,
        provider_id: &str,
        auth_type: maho_ai::auth::types::AuthType,
        interaction: std::sync::Arc<dyn maho_ai::auth::types::AuthInteraction>,
    ) -> Result<maho_ai::auth::types::Credential, ModelsError> {
        use maho_ai::auth::types::{AuthType, Credential, ProviderAuthInteraction};
        let id = normalize_provider_id(provider_id);
        let signal = maho_ai::utils::abort::operation_signal(interaction.signal());
        let aborted = |reason: maho_ai::utils::abort::AbortReason| {
            ModelsError::new(ModelsErrorCode::Auth, reason.message)
        };
        signal.throw_if_aborted().map_err(aborted)?;
        let Some(provider) = self.get_provider(&id) else {
            return Err(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {id}"),
            ));
        };
        let auth = compose_provider_auth(&self.config, &self.extensions, provider.as_ref());
        let provider_interaction =
            ProviderAuthInteraction::new(signal.clone(), interaction.clone());
        let unsupported = |kind: &str| {
            ModelsError::new(
                ModelsErrorCode::Auth,
                format!("{} does not support {kind} login", provider.name()),
            )
        };
        let credential =
            match auth_type {
                AuthType::OAuth => {
                    let oauth = auth.oauth.ok_or_else(|| unsupported("oauth"))?;
                    let login = maho_ai::utils::abort::race_with_abort_signal(
                        oauth.login(&provider_interaction),
                        &signal,
                    )
                    .await
                    .map_err(aborted)?;
                    Credential::OAuth(login.map_err(|error| {
                        ModelsError::new(ModelsErrorCode::OAuth, error.to_string())
                    })?)
                }
                AuthType::ApiKey => {
                    let api_key = auth
                        .api_key
                        .filter(|api_key| api_key.has_login())
                        .ok_or_else(|| unsupported("api_key"))?;
                    let login = maho_ai::utils::abort::race_with_abort_signal(
                        api_key.login(&provider_interaction),
                        &signal,
                    )
                    .await
                    .map_err(aborted)?;
                    Credential::ApiKey(login.map_err(|error| {
                        ModelsError::new(ModelsErrorCode::Auth, error.to_string())
                    })?)
                }
            };
        let receipt = self
            .credentials
            .append_login_account(&id, credential.clone(), Some(signal.clone()))
            .await
            .map_err(|error| match signal.throw_if_aborted() {
                Err(reason) => aborted(reason),
                Ok(()) => ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store modify failed for {id}"),
                    &error,
                ),
            })?;
        if let Some(receipt) = receipt {
            maho_ai::auth::types::AuthInteraction::on_account_committed(
                interaction.as_ref(),
                receipt,
            );
        }
        // senpi `synchronizeCredentialState`: the snapshot follows the credential change. A
        // failed pass is recorded as the availability error surfaced by `get_error`.
        let _ = self.refresh_provider_availability(&id, signal).await;
        Ok(credential)
    }

    /// Pinned `ModelRuntime.logout` -> `Models.logout` (`models.ts:639-655`): with `slot_id`,
    /// remove only that account slot (deleting the entry when it was the last); otherwise remove
    /// the provider's stored credential.
    pub async fn logout(
        &self,
        provider_id: &str,
        slot_id: Option<&str>,
        options: maho_ai::auth::types::AuthOperationOptions,
    ) -> Result<(), ModelsError> {
        let id = normalize_provider_id(provider_id);
        let signal = maho_ai::utils::abort::operation_signal(options.signal);
        self.enqueue_credential_operation(&id, &signal, async {
            self.logout_unsynchronized(
                &id,
                slot_id,
                maho_ai::auth::types::AuthOperationOptions {
                    signal: Some(signal.clone()),
                },
            )
            .await?;
            self.synchronize_credential_state(&id, "logout", &signal)
                .await
        })
        .await
    }

    /// senpi `setRuntimeApiKey`: a non-persistent key for this process (`--api-key`), then recompose
    /// and refresh the provider so availability reflects it.
    pub async fn set_runtime_api_key(
        &self,
        provider_id: &str,
        api_key: &str,
        options: maho_ai::auth::types::AuthOperationOptions,
    ) -> Result<(), ModelsError> {
        let signal = maho_ai::utils::abort::operation_signal(options.signal);
        self.enqueue_credential_operation(provider_id, &signal, async {
            self.credentials.set_runtime_api_key(provider_id, api_key);
            self.synchronize_credential_state(provider_id, "setRuntimeApiKey", &signal)
                .await
        })
        .await
    }

    /// senpi `removeRuntimeApiKey`.
    pub async fn remove_runtime_api_key(
        &self,
        provider_id: &str,
        options: maho_ai::auth::types::AuthOperationOptions,
    ) -> Result<(), ModelsError> {
        let signal = maho_ai::utils::abort::operation_signal(options.signal);
        self.enqueue_credential_operation(provider_id, &signal, async {
            self.credentials.remove_runtime_api_key(provider_id);
            self.synchronize_credential_state(provider_id, "removeRuntimeApiKey", &signal)
                .await
        })
        .await
    }

    /// senpi `enqueueCredentialOperation`: wait (abortably) for the provider's previous credential
    /// operation, then run this one to completion; once started it is not raced with the signal.
    async fn enqueue_credential_operation<T>(
        &self,
        provider_id: &str,
        signal: &maho_ai::utils::abort::AbortSignal,
        task: impl std::future::Future<Output = Result<T, ModelsError>>,
    ) -> Result<T, ModelsError> {
        let aborted = |reason: maho_ai::utils::abort::AbortReason| {
            ModelsError::new(ModelsErrorCode::Auth, reason.message)
        };
        let queue = self
            .credential_operations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(provider_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _turn = maho_ai::utils::abort::race_with_abort_signal(queue.lock_owned(), signal)
            .await
            .map_err(aborted)?;
        signal.throw_if_aborted().map_err(aborted)?;
        task.await
    }

    /// senpi `synchronizeCredentialState`: recompose the provider and refresh its catalog offline;
    /// any failure is reported as a committed-but-unsynchronized credential change.
    async fn synchronize_credential_state(
        &self,
        provider_id: &str,
        operation: &str,
        signal: &maho_ai::utils::abort::AbortSignal,
    ) -> Result<(), ModelsError> {
        let id = normalize_provider_id(provider_id);
        let synchronize = async {
            signal.throw_if_aborted().map_err(|reason| reason.message)?;
            self.recompose_provider(&id);
            let composition_error = self
                .errors
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .get(&id)
                .cloned();
            if let Some(error) = composition_error {
                return Err(error);
            }
            let result = self
                .models
                .refresh(ModelsRefreshOptions {
                    allow_network: Some(false),
                    providers: Some(vec![id.clone()]),
                    force: None,
                    signal: Some(signal.clone()),
                })
                .await;
            if result.aborted {
                signal.throw_if_aborted().map_err(|reason| reason.message)?;
            }
            if let Some(error) = result.errors.get(&id) {
                return Err(error.message.clone());
            }
            Ok(())
        };
        synchronize.await.map_err(|cause| ModelsError::with_cause(ModelsErrorCode::Auth, format!("Credential {operation} committed for {provider_id}, but local synchronization failed"), &cause))
    }

    async fn logout_unsynchronized(
        &self,
        provider_id: &str,
        slot_id: Option<&str>,
        options: maho_ai::auth::types::AuthOperationOptions,
    ) -> Result<(), ModelsError> {
        use maho_ai::auth::pool::slots::{PooledCredentialType, remove_slot};
        let id = normalize_provider_id(provider_id);
        let signal = maho_ai::utils::abort::operation_signal(options.signal);
        signal
            .throw_if_aborted()
            .map_err(|reason| ModelsError::new(ModelsErrorCode::Auth, reason.message))?;
        let result = 'mutation: {
            let failed = |error: String| {
                ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store delete failed for {id}"),
                    &error,
                )
            };
            let _mutation = self.credentials.account_mutation.lock().await;
            let Some(slot_id) = slot_id else {
                break 'mutation self.credentials.delete(&id).map_err(failed);
            };
            let current = self
                .credentials
                .get(&id)
                .map(serde_json::from_value::<PooledCredential>)
                .transpose()
                .map_err(|error| failed(error.to_string()))?;
            let Some(next) = remove_slot(current, slot_id) else {
                break 'mutation self.credentials.delete(&id).map_err(failed);
            };
            let value = match next.credential_type {
                PooledCredentialType::ApiKey => serde_json::to_value(&next),
                PooledCredentialType::OAuth => serde_json::to_value(next.to_stored_credential()),
            }
            .map_err(|error| failed(error.to_string()))?;
            self.credentials.set(&id, Some(value)).map_err(failed)
        };
        // senpi `synchronizeCredentialState`; a failed pass is recorded for `get_error`.
        let _ = self.refresh_provider_availability(&id, signal).await;
        result
    }

    pub async fn get_auth_for_model(
        &self,
        model: &Model,
        overrides: &AuthResolutionOverrides,
    ) -> Result<Option<AuthResolution>, ModelsError> {
        if let Some(error) = self.consent_error(&model.provider, overrides.slot_name.as_deref()) {
            return Err(error);
        }
        self.get_auth_for_model_unchecked(model, overrides).await
    }
    /// Un-gated `get_auth_for_model`, only for CLI auth diagnostics (SD-4).
    pub async fn get_auth_for_model_unchecked(
        &self,
        model: &Model,
        overrides: &AuthResolutionOverrides,
    ) -> Result<Option<AuthResolution>, ModelsError> {
        let signal = maho_ai::utils::abort::operation_signal(overrides.signal.clone());
        let resolve = async {
            let Some(mut resolution) = self.models.get_auth_for_model(model, overrides).await?
            else {
                return Ok(None);
            };
            let mut env = resolution.env.clone().unwrap_or_default();
            env.extend(overrides.env.clone().unwrap_or_default());
            let env: std::collections::HashMap<String, String> = env.into_iter().collect();
            let headers = self
                .get_compatibility_request_headers(model, Some(&env))
                .await
                .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error))?;
            if let Some(headers) = headers {
                resolution
                    .auth
                    .headers
                    .get_or_insert_with(Default::default)
                    .extend(headers);
            }
            Ok(Some(resolution))
        };
        maho_ai::utils::abort::race_with_abort_signal(resolve, &signal)
            .await
            .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.message))?
    }
    pub fn get_compatibility_request_config(
        &self,
        model: &Model,
    ) -> crate::provider_composer::CompatibilityRequestConfig {
        let config = self.config.read().unwrap_or_else(|p| p.into_inner());
        let extension = self.extensions.read().unwrap_or_else(|p| p.into_inner());
        crate::provider_composer::resolve_compatibility_request_config(
            model,
            config.get_provider(&model.provider),
            extension.get(&model.provider),
        )
    }
    pub async fn get_compatibility_request_headers(
        &self,
        model: &Model,
        env: Option<&std::collections::HashMap<String, String>>,
    ) -> Result<Option<maho_ai::types::ProviderHeaders>, String> {
        let config = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get_provider(&model.provider)
            .cloned();
        let extension = self
            .extensions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&model.provider)
            .cloned();
        crate::provider_composer::resolve_compatibility_request_headers(
            model,
            config.as_ref(),
            extension.as_ref(),
            env,
        )
        .await
    }
    pub async fn refresh(&mut self, mut options: ModelsRefreshOptions) -> ModelsRefreshResult {
        let config = ModelConfig::load_with_migration(
            self.models_path.as_deref(),
            !crate::lockfile_policy::facts_lock_broker_enabled(),
        )
        .await;
        *self.config.write().unwrap_or_else(|p| p.into_inner()) = config;
        // senpi: `allowNetwork: options.allowNetwork ?? this.modelNetworkEnabled`.
        options.allow_network = Some(options.allow_network.unwrap_or(self.model_network_enabled));
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
                async move {
                    (
                        provider_id.clone(),
                        this.refresh_provider_availability(provider_id, provider_signal)
                            .await,
                    )
                }
            }))
            .await;
            for (provider_id, outcome) in outcomes {
                if let Err(error) = outcome
                    && !signal.as_ref().is_some_and(AbortSignal::aborted)
                {
                    result
                        .errors
                        .insert(provider_id, error);
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
    /// senpi `reloadConfig`.
    pub async fn reload_config(&mut self) -> ModelsRefreshResult {
        let allow_network = self.model_network_enabled;
        self.refresh(ModelsRefreshOptions {
            allow_network: Some(allow_network),
            ..Default::default()
        })
        .await
    }
    pub fn model_refresh_timeout_ms(&self) -> u64 {
        self.model_refresh_timeout_ms
    }
    pub fn register_provider(
        &mut self,
        id: &str,
        config: ProviderConfigInput,
    ) -> Result<(), String> {
        let id = normalize_provider_id(id);
        compose_model_provider(
            &id,
            self.native
                .get(&id)
                .or_else(|| self.builtins.get(&id))
                .cloned(),
            &self.config.read().unwrap_or_else(|p| p.into_inner()),
            Some(&config),
        )?;
        self.extensions
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.clone(), config.clone());
        self.recompose_provider(&id);
        self.sync_registered_oauth(&id);
        self.update_model_snapshot();
        let stored = self
            .availability_state()
            .snapshot
            .stored_providers
            .contains(&id);
        let configured = stored
            || crate::provider_api_key_auth::configured_request_auth_status(
                self.config
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .get_provider(&id),
                Some(&config.config),
            )
            .is_some_and(|status| status.configured);
        if configured {
            let mut guard = self.availability_state();
            let snapshot = &mut guard.snapshot;
            snapshot.configured_providers.insert(id.clone());
            // Provisional entry until the async refresh lands; never clobber a real check result.
            snapshot
                .auth
                .entry(id.clone())
                .or_insert_with(|| AuthCheck {
                    ambient: false,
                    source: Some("configured provider".into()),
                    auth_type: if config.oauth.is_some() && config.api_key.is_none() {
                        AuthType::OAuth
                    } else {
                        AuthType::ApiKey
                    },
                });
            snapshot.available = snapshot
                .all
                .iter()
                .filter(|model| snapshot.configured_providers.contains(&model.provider))
                .cloned()
                .collect();
            guard.revision += 1;
        }
        Ok(())
    }
    pub fn register_native_provider(&mut self, provider: Arc<dyn Provider>) {
        let id = normalize_provider_id(provider.id());
        self.native.insert(id.clone(), provider);
        self.recompose_provider(&id);
        self.sync_registered_oauth(&id);
        self.update_model_snapshot();
    }
    pub fn unregister_provider(&mut self, id: &str) {
        let id = normalize_provider_id(id);
        self.native.shift_remove(&id);
        self.extensions
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .shift_remove(&id);
        self.recompose_provider(&id);
        self.credentials.unregister_oauth_provider(&id);
        self.update_model_snapshot();
    }
    /// senpi `registerProvider`/`registerNativeProvider`: the composed OAuth method joins the
    /// credential store's OAuth provider list (`/login`, RPC), or leaves it when the provider has none.
    fn sync_registered_oauth(&self, id: &str) {
        let oauth = self.get_provider(id).and_then(|provider| {
            compose_provider_auth(&self.config, &self.extensions, provider.as_ref()).oauth
        });
        match oauth {
            Some(oauth) => self.credentials.register_oauth_provider(id, oauth),
            None => self.credentials.unregister_oauth_provider(id),
        }
    }
    pub fn get_registered_provider_ids(&self) -> Vec<String> {
        self.extensions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .chain(self.native.keys())
            .cloned()
            .collect()
    }
    pub fn get_registered_provider_config(&self, id: &str) -> Option<ProviderConfigInput> {
        self.extensions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned()
    }
    pub fn get_registered_native_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.native.get(id).cloned()
    }
    pub fn is_using_oauth(&self, id: &str) -> bool {
        self.credentials
            .get(id)
            .as_ref()
            .and_then(crate::auth_storage::credential_kind)
            == Some(crate::auth_storage::CredentialKind::Oauth)
    }
    /// senpi `isUsingSubscription`: a stored OAuth credential whose provider OAuth is subscription-backed.
    pub fn is_using_subscription(&self, id: &str) -> bool {
        self.is_using_oauth(id)
            && self
                .get_provider_auth(id)
                .and_then(|auth| auth.oauth)
                .is_some_and(|oauth| oauth.is_subscription())
    }
    /// senpi `provider.auth`: the provider's composed api-key and OAuth methods.
    pub fn get_provider_auth(&self, id: &str) -> Option<ProviderAuth> {
        self.get_provider(id).map(|provider| {
            compose_provider_auth(&self.config, &self.extensions, provider.as_ref())
        })
    }
    pub fn is_fallback_eligible(&self, id: &str) -> bool {
        self.extensions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .and_then(|e| e.fallback_eligible.as_ref())
            .is_none_or(|hook| hook())
    }
    fn prepare_request(
        &self,
        model: &Model,
        options: &mut StreamOptions,
    ) -> (
        Model,
        Option<crate::model_config_schema::ModelsJsonProvider>,
        Option<ProviderConfigInput>,
    ) {
        let config = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get_provider(&model.provider)
            .cloned();
        let extension = self
            .extensions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&model.provider)
            .cloned();
        let compatibility = crate::provider_composer::resolve_compatibility_request_config(
            model,
            config.as_ref(),
            extension.as_ref(),
        );
        if let Some(mut body) = compatibility.extra_body {
            body.extend(options.extra_body.take().unwrap_or_default());
            options.extra_body = Some(body);
        }
        let mut request_model = model.clone();
        if let Some(id) = compatibility.upstream_model_id {
            request_model.id = id;
        }
        if let Some(tier) = compatibility.service_tier {
            request_model.service_tier = Some(tier);
        }
        (request_model, config, extension)
    }
    /// senpi `couldRotateCredentials`: rotation engages only when nothing pins the request to one
    /// credential (an explicit or runtime key, `credentials.rotation: false`) and the provider can
    /// hold more than one slot.
    fn owns_account_selection(&self, provider_id: &str) -> bool {
        self.extensions.read().unwrap_or_else(|p| p.into_inner()).get(provider_id).is_some_and(|provider| provider.owns_account_selection)
    }
    async fn retire_outer_slot_state(&self, provider_id: &str) -> Result<(), String> {
        if !self.owns_account_selection(provider_id) { return Ok(()); }
        let mut retired = self.outer_slot_state_retired.lock().await;
        if retired.contains(provider_id) { return Ok(()); }
        match self.pool_repository.clear_provider(provider_id).await {
            Ok(_) => { retired.insert(provider_id.into()); Ok(()) },
            Err(error) if error.to_ascii_lowercase().contains("busy") => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn could_rotate_credentials(
        &self,
        model: &Model,
        request: &maho_ai::types::ProviderRequestOptions,
    ) -> Option<RotationContext> {
        if self.owns_account_selection(&model.provider) || request.api_key.is_some() || self.credentials.has_runtime_api_key(&model.provider) {
            return None;
        }
        let policy = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get_provider(&model.provider)
            .and_then(|provider| provider.credentials.clone());
        if policy.as_ref().and_then(|policy| policy.rotation) == Some(false) {
            return None;
        }
        let context = RotationContext {
            credentials: self.credentials.clone(),
            repository: self.pool_repository.clone(),
            policy,
            env: request.env.clone(),
        };
        if self.credentials.get(&model.provider).is_some() {
            return Some(context);
        }
        let env = context.env_lookup();
        let slots = crate::credential_pool::env_slots::discover_env_slots(&model.provider, &*env)
            .len()
            + context.policy_slot_count();
        (slots > 1).then_some(context)
    }
    fn rotating_stream(
        model: &Model,
        rotation: Option<RotationContext>,
        affinity_key: Option<String>,
        models: Models,
        semaphores: Arc<crate::provider_concurrency::ProviderSemaphores>,
        request: maho_ai::types::ProviderRequestOptions,
        call: ProviderCall,
    ) -> impl std::future::Future<
        Output = Result<
            (
                AssistantMessageEventStream,
                Option<crate::provider_concurrency::ProviderPermit>,
            ),
            String,
        >,
    > + Send
    + 'static {
        let model = model.clone();
        async move {
            let sources = match rotation {
                Some(rotation) => rotation.sources(&model.provider).await,
                None => None,
            };
            if let Some(sources) = sources {
                let attempt_model = model.clone();
                let run:crate::credential_pool::rotation_stream::RunRotationAttempt=Arc::new(move |slot:crate::credential_pool::rotation_stream::RotationSlot| -> crate::credential_pool::rotation_stream::RotationAttemptFuture {
                    let (models,semaphores,model,request,call)=(models.clone(),semaphores.clone(),attempt_model.clone(),request.clone(),call.clone());
                    Box::pin(async move {
                        let (provider,request_model,request)=prepare_slot_request(&models,&model,&slot,request).await.map_err(|message|serde_json::json!({"message":message}))?;
                        let signal=request.signal.clone();
                        let (stream,permit)=semaphores.bracket(&request_model.provider,signal.as_ref(),|| call(CallTarget::Provider(provider.clone()),&request_model,request)).await
                            .map_err(|reason|serde_json::json!({"name":reason.name,"message":reason.message}))?;
                        Ok(hold_permit(stream,permit))
                    })
                });
                return Ok((
                    crate::credential_pool::rotation_stream::stream_with_credential_rotation(
                        &model,
                        sources,
                        affinity_key,
                        run,
                    ),
                    None,
                ));
            }
            let signal = request.signal.clone();
            semaphores
                .bracket(&model.provider, signal.as_ref(), || {
                    call(CallTarget::Models(models.clone()), &model, request)
                })
                .await
                .map_err(|reason| reason.message)
        }
    }
    /// Consent gate for every request path (`stream`, `stream_simple`, `complete`): returns a
    /// terminal error stream before any rotation, refresh, header or network work starts.
    fn consent_error_stream(&self, model: &Model) -> Option<AssistantMessageEventStream> {
        let message = crate::provider_consent::consent_failure(&self.credentials, &model.provider, None)?;
        let mut error = maho_ai::utils::lazy::setup_error_message(model, &message);
        error.provider_diagnostic = Some(maho_ai::types::ProviderDiagnostic {
            category: maho_ai::types::ProviderDiagnosticCategory::Unknown,
            http_status: None,
            code: Some(crate::provider_consent::CONSENT_REQUIRED_CODE.to_owned()),
            evidence: maho_ai::types::ProviderDiagnosticEvidence::StructuredCode,
            resets_at: None,
        });
        let stream = AssistantMessageEventStream::assistant();
        stream.push(maho_ai::types::AssistantMessageEvent::Error {
            reason: maho_ai::types::ErrorReason::Error,
            error: error.clone(),
        });
        stream.end(Some(error));
        Some(stream)
    }
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        if let Some(failed) = self.consent_error_stream(model) {
            return failed;
        }
        let mut options = options.unwrap_or_default();
        let (request_model, config, extension) = self.prepare_request(model, &mut options);
        let models = self.models.clone();
        let context = context.clone();
        let original = model.clone();
        let tools = context.tools.clone().unwrap_or_default();
        let semaphores = self.provider_semaphores.clone();
        let rotation = self.could_rotate_credentials(model, &options.request);
        let runtime = self.clone();
        let inner = Self::lazy_request(model, async move {
            runtime.retire_outer_slot_state(&original.provider).await?;
            Self::apply_configured_headers(
                &original,
                config.as_ref(),
                extension.as_ref(),
                &mut options,
            )
            .await?;
            let request = options.request.clone();
            let affinity = options.session_id.clone();
            let call: ProviderCall = Arc::new(
                move |target: CallTarget,
                      model: &Model,
                      request: maho_ai::types::ProviderRequestOptions| {
                    let mut options = options.clone();
                    options.request = request;
                    match target {
                        CallTarget::Provider(provider) => {
                            provider.stream(model, &context, Some(options))
                        }
                        CallTarget::Models(models) => {
                            models.stream(model, &context, Some(options), Default::default())
                        }
                    }
                },
            );
            Self::rotating_stream(
                &request_model,
                rotation,
                affinity,
                models,
                semaphores,
                request,
                call,
            )
            .await
        });
        maho_ai::wrap_stream_with_model_recovery(inner, model, &tools)
    }
    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_simple_with_transforms(
            model,
            context,
            options,
            ModelsRequestTransforms::default(),
        )
    }
    /// `streamSimple` with the `ModelsRequestTransforms` senpi passes inside the options object
    /// (`transformHeaders` runs on the assembled request headers after auth resolution).
    pub fn stream_simple_with_transforms(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
        transforms: ModelsRequestTransforms,
    ) -> AssistantMessageEventStream {
        if let Some(failed) = self.consent_error_stream(model) {
            return failed;
        }
        let mut options = options.unwrap_or_default();
        let (request_model, config, extension) = self.prepare_request(model, &mut options.stream);
        let models = self.models.clone();
        let context = context.clone();
        let original = model.clone();
        let tools = context.tools.clone().unwrap_or_default();
        let semaphores = self.provider_semaphores.clone();
        let rotation = self.could_rotate_credentials(model, &options.stream.request);
        let runtime = self.clone();
        let inner = Self::lazy_request(model, async move {
            runtime.retire_outer_slot_state(&original.provider).await?;
            Self::apply_configured_headers(
                &original,
                config.as_ref(),
                extension.as_ref(),
                &mut options.stream,
            )
            .await?;
            let request = options.stream.request.clone();
            let affinity = options.stream.session_id.clone();
            let call: ProviderCall = Arc::new(
                move |target: CallTarget,
                      model: &Model,
                      request: maho_ai::types::ProviderRequestOptions| {
                    let transforms = transforms.clone();
                    let mut options = options.clone();
                    options.stream.request = request;
                    match target {
                        CallTarget::Provider(provider) => {
                            provider.stream_simple(model, &context, Some(options))
                        }
                        CallTarget::Models(models) => {
                            models.stream_simple(model, &context, Some(options), transforms)
                        }
                    }
                },
            );
            Self::rotating_stream(
                &request_model,
                rotation,
                affinity,
                models,
                semaphores,
                request,
                call,
            )
            .await
        });
        maho_ai::wrap_stream_with_model_recovery(inner, model, &tools)
    }
    async fn apply_configured_headers(
        model: &Model,
        config: Option<&crate::model_config_schema::ModelsJsonProvider>,
        extension: Option<&ProviderConfigInput>,
        options: &mut StreamOptions,
    ) -> Result<(), String> {
        let env = options
            .request
            .env
            .as_ref()
            .map(|v| v.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let signal = maho_ai::utils::abort::operation_signal(options.request.signal.clone());
        let headers = maho_ai::utils::abort::race_with_abort_signal(
            crate::provider_composer::resolve_compatibility_request_headers(
                model,
                config,
                extension,
                env.as_ref(),
            ),
            &signal,
        )
        .await
        .map_err(|error| error.message)??;
        if let Some(mut headers) = headers {
            headers.extend(options.request.headers.take().unwrap_or_default());
            options.request.headers = Some(headers);
        }
        Ok(())
    }
    /// The provider permit (if any) is held until the forwarded stream settles.
    fn lazy_request(
        model: &Model,
        setup: impl std::future::Future<
            Output = Result<
                (
                    AssistantMessageEventStream,
                    Option<crate::provider_concurrency::ProviderPermit>,
                ),
                String,
            >,
        > + Send
        + 'static,
    ) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::assistant();
        let (source_tx, source_rx) = tokio::sync::oneshot::channel::<AssistantMessageEventStream>();
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
        stream.set_cancellation_handler(Box::new(move || Box::pin(async move {
            let _ = cancel_tx.send(());
            if let Ok(source) = source_rx.await {
                source.cancel().await?;
            }
            Ok(())
        })));
        let target = stream.clone();
        let model = model.clone();
        let task = tokio::spawn(async move {
            let prepared = tokio::select! {
                biased;
                _ = &mut cancel_rx => return,
                prepared = setup => prepared,
            };
            let (source, _permit) = match prepared {
                Ok(setup) => setup,
                Err(error) => (maho_ai::utils::lazy::error_stream(&model, &error), None),
            };
            let _ = source_tx.send(source.clone());
            loop {
                match source.next().await {
                    Ok(Some(event)) => target.push(event),
                    Ok(None) => {
                        match source.result_if_ready() {
                            Some(Ok(message)) => target.end(Some(message)),
                            Some(Err(error)) => target.fail(error),
                            None => target.end(None),
                        }
                        break;
                    }
                    Err(error) => {
                        target.fail(error);
                        break;
                    }
                }
            }
        });
        stream.set_cancellation_task(task);
        stream
    }
    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> Result<AssistantMessage, maho_ai::utils::event_stream::StreamError> {
        let stream = self.stream(model, context, options);
        // Pop events so their partial-message clones are freed instead of queued until drop.
        while let Ok(Some(_)) = stream.next().await {}
        stream.result().await
    }
}

/// Where a prepared request goes: straight to the provider (rotation already resolved the slot's
/// auth) or through the collection, which resolves auth itself.
enum CallTarget {
    Provider(Arc<dyn Provider>),
    Models(Models),
}
type ProviderCall = Arc<
    dyn Fn(
            CallTarget,
            &Model,
            maho_ai::types::ProviderRequestOptions,
        ) -> AssistantMessageEventStream
        + Send
        + Sync,
>;

/// The inputs `credentialRotationSources` needs, captured synchronously at admission.
struct RotationContext {
    credentials: Arc<crate::auth_storage::AuthStorage>,
    repository: Arc<crate::credential_pool::state_store::CredentialSlotRepository>,
    policy: Option<crate::model_config_schema::ModelsJsonCredentialPolicy>,
    env: Option<maho_ai::types::ProviderEnv>,
}

impl RotationContext {
    /// senpi `(name) => options?.env?.[name] ?? process.env[name]`.
    fn env_lookup(&self) -> crate::credential_pool::rotation_stream::EnvLookup {
        let env = self.env.clone();
        Arc::new(move |name| {
            env.as_ref()
                .and_then(|env| env.get(name).cloned())
                .or_else(|| std::env::var(name).ok())
        })
    }
    fn policy_slot_count(&self) -> usize {
        self.policy
            .as_ref()
            .and_then(|policy| policy.slots.as_ref())
            .map_or(0, IndexMap::len)
    }
    fn rotation_policy(&self) -> crate::credential_pool::rotation_stream::RotationPolicy {
        use crate::credential_pool::rotation_stream::{PolicySlotRef, RotationPolicy};
        let Some(policy) = &self.policy else {
            return RotationPolicy::default();
        };
        RotationPolicy {
            affinity: policy.affinity,
            cooldown_base_ms: policy.cooldown_base_ms.map(|ms| ms as u64),
            cooldown_cap_ms: policy.cooldown_cap_ms.map(|ms| ms as u64),
            slots: policy
                .slots
                .iter()
                .flatten()
                .map(|(name, slot)| {
                    (
                        name.clone(),
                        PolicySlotRef {
                            env: slot.env.clone(),
                            value: slot.value.clone(),
                        },
                    )
                })
                .collect(),
        }
    }
    /// senpi `credentialRotationSources`: `Some` only when the provider really holds more than one
    /// rotation slot (`mightHoldCredentialPool`, then the leaseless slot listing).
    async fn sources(
        &self,
        provider_id: &str,
    ) -> Option<crate::credential_pool::rotation_stream::OwnedRotationSources> {
        let raw = self.credentials.get(provider_id);
        let env = self.env_lookup();
        let might_hold_pool = match &raw {
            Some(raw) => {
                raw.get("accounts")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|accounts| accounts.len() > 1)
                    || self.policy_slot_count() > 0
            }
            None => {
                crate::credential_pool::env_slots::discover_env_slots(provider_id, &*env).len()
                    + self.policy_slot_count()
                    > 1
            }
        };
        if !might_hold_pool {
            return None;
        }
        let credential = match raw {
            Some(mut raw) => {
                // The pooled shape keeps an integer flat expiry; a fractional one only affects the
                // flat fallback slot's metadata, never its material.
                if let Some(object) = raw.as_object_mut()
                    && object
                        .get("expires")
                        .is_some_and(|expires| !expires.is_i64())
                {
                    object.remove("expires");
                }
                Some(
                    serde_json::from_value::<crate::credential_pool::slots::PooledCredential>(raw)
                        .ok()?,
                )
            }
            None => None,
        };
        let sources = crate::credential_pool::rotation_stream::OwnedRotationSources {
            provider_id: provider_id.to_owned(),
            credential,
            env,
            repository: self.repository.clone(),
            policy: self.rotation_policy(),
        };
        let slots = sources.list_without_leases().await.ok()?;
        (slots.len() > 1).then_some(sources)
    }
}

/// senpi `prepareRequest(model, options, slotAuth)`: resolve one rotation slot's auth (env lane:
/// its key; stored lane: its slot) and apply it to the request like the collection's `applyAuth`.
async fn prepare_slot_request(
    models: &Models,
    model: &Model,
    slot: &crate::credential_pool::rotation_stream::RotationSlot,
    mut request: maho_ai::types::ProviderRequestOptions,
) -> Result<
    (
        Arc<dyn Provider>,
        Model,
        maho_ai::types::ProviderRequestOptions,
    ),
    String,
> {
    use crate::credential_pool::rotation_stream::RotationLane;
    let provider = models
        .get_provider(&model.provider)
        .ok_or_else(|| format!("Unknown provider: {}", model.provider))?;
    let overrides = AuthResolutionOverrides {
        api_key: (slot.lane == RotationLane::Env)
            .then(|| slot.env_key.clone())
            .flatten(),
        slot_name: (slot.lane == RotationLane::Stored).then(|| slot.slot.name.clone()),
        env: request.env.clone(),
        signal: request.signal.clone(),
        ..Default::default()
    };
    let resolution = models
        .get_auth_for_model(model, &overrides)
        .await
        .map_err(|error| error.message)?
        .ok_or_else(|| maho_ai::models::provider_not_configured_message(&model.provider))?;
    let mut headers = resolution.auth.headers.clone();
    if let Some(overrides) = request.headers.take() {
        let merged = headers.get_or_insert_with(Default::default);
        for (name, value) in overrides {
            merged.retain(|existing, _| !existing.eq_ignore_ascii_case(&name));
            merged.insert(name, value);
        }
    }
    let env = match (&resolution.env, &request.env) {
        (None, None) => None,
        (base, extra) => {
            let mut env = base.clone().unwrap_or_default();
            env.extend(extra.clone().unwrap_or_default());
            Some(env)
        }
    };
    let mut request_model = model.clone();
    if let Some(base_url) = resolution.auth.base_url {
        request_model.base_url = base_url;
    }
    request.api_key = overrides.api_key.or(resolution.auth.api_key);
    request.headers = headers;
    request.env = env;
    Ok((provider, request_model, request))
}

/// Keeps a provider permit until the attempt's stream settles.
fn hold_permit(
    source: AssistantMessageEventStream,
    permit: Option<crate::provider_concurrency::ProviderPermit>,
) -> AssistantMessageEventStream {
    let Some(permit) = permit else { return source };
    let target = AssistantMessageEventStream::assistant();
    let forwarded = target.clone();
    target.forward_cancellation_from(source.clone());
    let task = tokio::spawn(async move {
        let _permit = permit;
        loop {
            match source.next().await {
                Ok(Some(event)) => target.push(event),
                Ok(None) => {
                    match source.result_if_ready() {
                        Some(Ok(message)) => target.end(Some(message)),
                        Some(Err(error)) => target.fail(error),
                        None => target.end(None),
                    }
                    break;
                }
                Err(error) => {
                    target.fail(error);
                    break;
                }
            }
        }
    });
    forwarded.set_cancellation_task(task);
    forwarded
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn credential_store_conditional_delete_serializes_with_login() {
        // Given deletion suspended inside the shared account mutation transaction.
        let credentials = Arc::new(crate::auth_storage::AuthStorage::in_memory(serde_json::Map::new()));
        credentials.set("p", Some(serde_json::json!({"type":"api_key","key":"old"}))).expect("seed");
        let store = AuthStorageCredentialStore { credentials, auth_path: None };
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let deletion = store.modify_or_delete("p", Box::new(move |current| Box::pin(async move {
            assert_eq!(current.expect("old credential").as_api_key().and_then(|key| key.key.as_deref()), Some("old"));
            entered_tx.send(()).expect("entry subscriber");
            release_rx.await.expect("release");
            Ok(None)
        })), None);
        tokio::pin!(deletion);
        assert!(futures::poll!(deletion.as_mut()).is_pending());
        tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx).await.expect("entry deadline").expect("entered");
        let login = store.modify("p", Box::new(|current| Box::pin(async move {
            assert!(current.is_none());
            Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("new".into()), ..Default::default() })))
        })), None);
        tokio::pin!(login);
        assert!(futures::poll!(login.as_mut()).is_pending());
        // When deletion commits before the queued login.
        release_tx.send(()).expect("release subscriber");
        assert!(tokio::time::timeout(std::time::Duration::from_secs(5), deletion).await.expect("delete deadline").expect("delete").is_none());
        tokio::time::timeout(std::time::Duration::from_secs(5), login).await.expect("login deadline").expect("login");
        // Then no trailing deletion removes the newly committed login.
        assert_eq!(store.read("p", None).await.expect("read").expect("new credential").as_api_key().and_then(|key| key.key.as_deref()), Some("new"));
    }

    #[tokio::test]
    async fn cancellation_drops_pending_request_setup_before_acknowledging() {
        let model = maho_ai::providers::faux::faux_provider(Default::default())
            .expect("faux provider")
            .get_model(None).expect("model");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel::<()>();
        let stream = ModelRuntime::lazy_request(&model, async move {
            let _drop_signal = dropped_tx;
            started_tx.send(()).expect("setup subscriber");
            std::future::pending().await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), started_rx).await.expect("bounded setup").expect("setup started");
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.cancel()).await.expect("bounded cancellation").expect("cancel setup");
        assert!(tokio::time::timeout(std::time::Duration::from_secs(5), dropped_rx).await.expect("bounded destruction").is_err());
    }

    #[tokio::test]
    async fn terminal_cancellation_releases_forwarded_provider_permit() {
        let semaphores = crate::provider_concurrency::ProviderSemaphores::new(|_| 1);
        let (_, permit) = semaphores.bracket("p", None, || ()).await.expect("permit");
        let source = AssistantMessageEventStream::assistant();
        let (released_tx, released_rx) = tokio::sync::oneshot::channel();
        source.set_cancellation_handler(Box::new(move || Box::pin(async move {
            released_tx.send(()).expect("cleanup subscriber");
            Ok(())
        })));
        let message = maho_ai::providers::faux::faux_assistant_message("done", Default::default());
        source.end(Some(message));
        let stream = hold_permit(source, permit);
        while let Ok(Some(_)) = stream.next().await {}
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.cancel()).await.expect("bounded cancellation").expect("terminal cleanup");
        tokio::time::timeout(std::time::Duration::from_secs(5), released_rx).await.expect("bounded cleanup").expect("source cleanup acknowledged");
        let (_, next) = tokio::time::timeout(std::time::Duration::from_secs(5), semaphores.bracket("p", None, || ()))
            .await.expect("permit released").expect("next permit");
        assert!(next.is_some());
    }

    #[tokio::test]
    async fn credential_store_preserves_pool_siblings_after_key_resolution() {
        let raw = serde_json::json!({"type":"api_key","key":"literal","accounts":[{"name":"a","key":"slot"}],"pinned":"a","providerMetadata":{"region":"r"}});
        let credentials = Arc::new(crate::auth_storage::AuthStorage::in_memory(
            serde_json::Map::from_iter([("p".to_owned(), raw.clone())]),
        ));
        let store = AuthStorageCredentialStore { credentials, auth_path: None };
        let credential = store.read("p", None).await.expect("read").expect("credential");
        let stored = serde_json::to_value(credential).expect("stored credential");
        assert_eq!(stored["accounts"], raw["accounts"]);
        assert_eq!(stored["pinned"], raw["pinned"]);
        assert_eq!(stored["providerMetadata"], raw["providerMetadata"]);
        assert!(stored.get("env").is_none_or(serde_json::Value::is_null));
    }

    fn configured_runtime(text: &str) -> (tempfile::TempDir, ModelRuntime) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.json");
        std::fs::write(&path, text).expect("config");
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            models_path: Some(path),
            providers: Some(Vec::new()),
            ..Default::default()
        }).expect("model runtime");
        (dir, runtime)
    }
    #[test]
    fn configured_catalog_has_identical_api_and_base_url() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"kiro-lb":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"claude-sonnet-5"}]}}}"#,
        );
        assert!(runtime.get_error().is_none());
        let model = runtime
            .get_model("kiro-lb", "claude-sonnet-5")
            .expect("model");
        assert_eq!(model.api, "openai-completions");
        assert_eq!(model.base_url, "http://localhost/v1");
    }
    #[test]
    fn unknown_invalid_provider_reports_error_and_preserves_valid_catalog() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"unknown-task-17":{"models":[{"id":"missing-api"}]},"valid":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        assert!(runtime.get_model("valid", "sol").is_some());
        assert!(runtime.get_provider("unknown-task-17").is_none());
        assert!(runtime.get_error().is_some());
    }
    #[tokio::test]
    async fn runtime_auth_resolves_key_and_provider_headers() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","apiKey":"fixture-key","authHeader":true,"models":[{"id":"sol"}]}}}"#,
        );
        let auth = runtime.get_auth("p").await.expect("resolve").expect("auth");
        assert_eq!(auth.auth.api_key.as_deref(), Some("fixture-key"));
        assert_eq!(
            auth.auth.headers.expect("headers")["Authorization"].as_deref(),
            Some("Bearer fixture-key")
        );
    }
    #[tokio::test]
    async fn auth_overrides_metadata_and_preaborted_checks_use_shared_store() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol","headers":{"x-model":"native"}}]}}}"#,
        );
        runtime
            .credentials
            .set(
                "p",
                Some(serde_json::json!({"type":"api_key","key":"stored-fixture"})),
            )
            .expect("seed");
        let model = runtime.get_model("p", "sol").expect("model");
        let auth = runtime
            .get_auth_for_model(
                &model,
                &AuthResolutionOverrides {
                    api_key: Some("override-fixture".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("resolve")
            .expect("auth");
        assert_eq!(auth.auth.api_key.as_deref(), Some("override-fixture"));
        assert_eq!(
            auth.auth.headers.expect("model headers")["x-model"].as_deref(),
            Some("native")
        );
        let metadata = runtime.list_credentials(None).await.expect("metadata");
        assert!(metadata.iter().any(|entry| entry.provider_id == "p"
            && entry.credential_type == maho_ai::auth::types::CredentialType::ApiKey));
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        let error = runtime
            .get_auth_for_model(
                &model,
                &AuthResolutionOverrides {
                    signal: Some(controller.signal()),
                    api_key: Some("override-fixture".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("preaborted model auth must not resolve");
        assert_eq!(
            error.to_string(),
            maho_ai::utils::abort::AbortReason::dom_default().message
        );
        assert!(
            runtime
                .check_auth(
                    "p",
                    Some(maho_ai::auth::types::AuthOperationOptions {
                        signal: Some(controller.signal())
                    })
                )
                .await
                .is_err()
        );
        assert!(
            runtime
                .list_credentials(Some(maho_ai::auth::types::AuthOperationOptions {
                    signal: Some(controller.signal())
                }))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn runtime_stream_recovers_leaked_text_tool_calls_for_claude_models() {
        use maho_ai::providers::faux::{
            FauxAssistantMessageOptions, FauxModelDefinition, RegisterFauxProviderOptions,
            faux_assistant_message, faux_provider,
        };
        let (_dir, mut runtime) = configured_runtime(r#"{"providers":{}}"#);
        let faux = faux_provider(RegisterFauxProviderOptions {
            provider: Some("faux-recovery".into()),
            models: Some(vec![FauxModelDefinition {
                id: "claude-fixture".into(),
                ..Default::default()
            }]),
            ..Default::default()
        }).expect("faux provider");
        let invoke =
            r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#;
        faux.core.set_responses(vec![
            faux_assistant_message(invoke, FauxAssistantMessageOptions::default()).into(),
        ]);
        runtime.register_native_provider(faux.provider.clone());
        let model = runtime
            .get_model("faux-recovery", "claude-fixture")
            .expect("model");
        let tool = maho_ai::types::Tool {
            name: "get_weather".into(),
            description: "d".into(),
            parameters: serde_json::json!({"type":"object"}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        };
        let context = Context {
            active_tool_names: None,
            system_prompt: None,
            messages: Vec::new(),
            tools: Some(vec![tool]),
        };
        let mut options = StreamOptions::default();
        options.request.api_key = Some("fixture-key".into());
        let message = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.stream(&model, &context, Some(options)).result(),
        )
        .await
        .expect("bounded stream")
        .expect("result");
        assert_eq!(message.stop_reason, maho_ai::types::StopReason::ToolUse);
        assert!(message.content.iter().any(|block| matches!(block, maho_ai::types::ContentBlock::ToolCall(call) if call.name == "get_weather")));
    }
    #[tokio::test]
    async fn preaborted_configured_stream_preserves_headers_and_stops_setup() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        let model = runtime.get_model("p", "sol").expect("model");
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        let headers = std::collections::BTreeMap::from([(
            "x-caller".to_owned(),
            Some("retained".to_owned()),
        )]);
        let mut options = StreamOptions {
            request: maho_ai::types::ProviderRequestOptions {
                signal: Some(controller.signal()),
                headers: Some(headers.clone()),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = ModelRuntime::apply_configured_headers(&model, None, None, &mut options).await;
        assert_eq!(
            result,
            Err(maho_ai::utils::abort::AbortReason::dom_default().message)
        );
        assert_eq!(options.request.headers, Some(headers));
    }

    fn builtin_runtime(id: &str) -> ModelRuntime {
        let provider = maho_ai::providers::all::builtin_providers()
            .expect("builtin providers")
            .into_iter()
            .find(|p| p.id() == id)
            .expect("builtin provider");
        ModelRuntime::create_sync(CreateModelRuntimeOptions {
            providers: Some(vec![provider]),
            ..Default::default()
        }).expect("model runtime")
    }

    #[tokio::test]
    async fn stored_cloudflare_credential_env_reaches_the_resolution() {
        let runtime = builtin_runtime("cloudflare-ai-gateway");
        runtime
            .credentials
            .set(
                "cloudflare-ai-gateway",
                Some(serde_json::json!({"type":"api_key","key":"cf-key",
            "env":{"CLOUDFLARE_ACCOUNT_ID":"acct","CLOUDFLARE_GATEWAY_ID":"gw"}})),
            )
            .expect("seed");
        let auth = runtime
            .get_auth("cloudflare-ai-gateway")
            .await
            .expect("resolve")
            .expect("auth");
        let env = auth.env.expect("credential env");
        assert_eq!(
            env.get("CLOUDFLARE_ACCOUNT_ID").map(String::as_str),
            Some("acct")
        );
        assert_eq!(
            env.get("CLOUDFLARE_GATEWAY_ID").map(String::as_str),
            Some("gw")
        );
        assert_eq!(
            auth.auth.headers.expect("gateway headers")["cf-aig-authorization"].as_deref(),
            Some("Bearer cf-key")
        );
    }

    #[tokio::test]
    async fn stored_kimi_region_routes_the_base_url() {
        // kimi-coding impersonates the Kimi CLI, so resolving it needs recorded risk consent;
        // keep that consent inside a temp agent dir so the test never reads the real settings.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"providerRiskConsent":{"kimi-coding":"2026-01-01T00:00:00.000Z"}}"#,
        )
        .expect("settings");
        let provider = maho_ai::providers::all::builtin_providers()
            .expect("builtin providers")
            .into_iter()
            .find(|p| p.id() == "kimi-coding")
            .expect("builtin provider");
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            providers: Some(vec![provider]),
            // The consent gate derives its agent dir from the credential store's file path, so the
            // runtime needs a file-backed store in this temp dir; an in-memory store falls back to
            // the process-global agent dir and would read the developer's real settings.
            auth_path: Some(dir.path().join("auth.json")),
            agent_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        })
        .expect("model runtime");
        runtime
            .credentials
            .set(
                "kimi-coding",
                Some(serde_json::json!({"type":"api_key","key":"kimi-key",
            "env":{"KIMI_CODE_REGION":"global"}})),
            )
            .expect("seed");
        let auth = runtime
            .get_auth("kimi-coding")
            .await
            .expect("resolve")
            .expect("auth");
        assert_eq!(
            auth.auth.base_url.as_deref(),
            Some("https://api.kimi.ai/coding")
        );
    }

    #[tokio::test]
    async fn missing_slot_does_not_fall_back_to_the_flat_credential() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime.credentials.set("p", Some(serde_json::json!({"type":"api_key","key":"flat","accounts":[{"name":"default","key":"flat"}]}))).expect("seed");
        let overrides = AuthResolutionOverrides {
            slot_name: Some("work".into()),
            ..Default::default()
        };
        assert!(
            runtime
                .get_auth_with_overrides("p", &overrides)
                .await
                .expect("resolve")
                .is_none()
        );
        let overrides = AuthResolutionOverrides {
            slot_name: Some("default".into()),
            ..Default::default()
        };
        let auth = runtime
            .get_auth_with_overrides("p", &overrides)
            .await
            .expect("resolve")
            .expect("slot auth");
        assert_eq!(auth.auth.api_key.as_deref(), Some("flat"));
    }

    struct CountingOAuth(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl maho_ai::auth::types::OAuthAuth for CountingOAuth {
        fn name(&self) -> &str {
            "counting-oauth"
        }
        async fn login(
            &self,
            _: &maho_ai::auth::types::ProviderAuthInteraction,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            anyhow::bail!("login is not exercised")
        }
        async fn refresh(
            &self,
            _: &maho_ai::auth::types::OAuthCredential,
            _: &maho_ai::utils::abort::AbortSignal,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(maho_ai::auth::types::OAuthCredential::new(
                "fresh",
                "next",
                f64::MAX,
            ))
        }
        async fn to_auth(
            &self,
            credential: &maho_ai::auth::types::OAuthCredential,
        ) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(maho_ai::auth::types::ModelAuth {
                api_key: Some(credential.access.clone()),
                ..Default::default()
            })
        }
    }

    fn counting_oauth_runtime(
        expires_in_ms: f64,
    ) -> (
        tempfile::TempDir,
        ModelRuntime,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let (dir, mut runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        runtime
            .register_provider(
                "p",
                crate::provider_composer::ProviderConfigInput {
                    oauth: Some(std::sync::Arc::new(CountingOAuth(calls.clone()))),
                    ..Default::default()
                },
            )
            .expect("register oauth");
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
        assert_eq!(
            runtime.credentials.get("p").expect("stored")["access"],
            "fresh"
        );
    }

    #[tokio::test]
    async fn available_models_check_never_refreshes_oauth() {
        let (_dir, runtime, calls) = counting_oauth_runtime(-1.0);
        assert!(
            runtime
                .get_available(Some("p"))
                .await
                .expect("available models")
                .iter()
                .any(|model| model.id == "sol")
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    struct TestAuthInteraction;
    #[async_trait::async_trait]
    impl maho_ai::auth::types::AuthInteraction for TestAuthInteraction {
        fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> {
            None
        }
        async fn prompt(&self, _: maho_ai::auth::types::AuthPrompt) -> anyhow::Result<String> {
            Ok("entered-key".into())
        }
        fn notify(&self, _: maho_ai::auth::types::AuthEvent) {}
    }
    struct TestOAuth;
    #[async_trait::async_trait]
    impl maho_ai::auth::types::OAuthAuth for TestOAuth {
        fn name(&self) -> &str {
            "test-oauth"
        }
        async fn login(
            &self,
            _: &maho_ai::auth::types::ProviderAuthInteraction,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            Ok(maho_ai::auth::types::OAuthCredential::new(
                "access", "refresh", 42.0,
            ))
        }
        async fn refresh(
            &self,
            credential: &maho_ai::auth::types::OAuthCredential,
            _: &maho_ai::utils::abort::AbortSignal,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            Ok(credential.clone())
        }
        async fn to_auth(
            &self,
            _: &maho_ai::auth::types::OAuthCredential,
        ) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(maho_ai::auth::types::ModelAuth::default())
        }
    }

    struct SubscriptionOAuth;
    #[async_trait::async_trait]
    impl maho_ai::auth::types::OAuthAuth for SubscriptionOAuth {
        fn name(&self) -> &str {
            "subscription-oauth"
        }
        fn is_subscription(&self) -> bool {
            true
        }
        async fn login(
            &self,
            _: &maho_ai::auth::types::ProviderAuthInteraction,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            Ok(maho_ai::auth::types::OAuthCredential::new(
                "access", "refresh", 42.0,
            ))
        }
        async fn refresh(
            &self,
            credential: &maho_ai::auth::types::OAuthCredential,
            _: &maho_ai::utils::abort::AbortSignal,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            Ok(credential.clone())
        }
        async fn to_auth(
            &self,
            _: &maho_ai::auth::types::OAuthCredential,
        ) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(maho_ai::auth::types::ModelAuth::default())
        }
    }

    #[test]
    fn subscription_requires_a_stored_oauth_credential_on_a_subscription_provider() {
        let (_dir, mut runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime
            .register_provider(
                "p",
                crate::provider_composer::ProviderConfigInput {
                    oauth: Some(std::sync::Arc::new(SubscriptionOAuth)),
                    ..Default::default()
                },
            )
            .expect("register oauth");
        assert_eq!(
            runtime
                .get_provider_auth("p")
                .and_then(|auth| auth.oauth)
                .map(|oauth| oauth.name().to_owned())
                .as_deref(),
            Some("subscription-oauth")
        );
        runtime
            .credentials
            .set("p", Some(serde_json::json!({"type":"api_key","key":"k"})))
            .expect("seed key");
        assert!(!runtime.is_using_subscription("p"));
        runtime
            .credentials
            .set(
                "p",
                Some(serde_json::json!({"type":"oauth","access":"a","refresh":"r","expires":1.0})),
            )
            .expect("seed oauth");
        assert!(runtime.is_using_subscription("p"));
    }

    #[tokio::test]
    async fn login_persists_an_oauth_credential_and_logout_removes_it() {
        let (_dir, mut runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime
            .register_provider(
                "p",
                crate::provider_composer::ProviderConfigInput {
                    oauth: Some(std::sync::Arc::new(TestOAuth)),
                    ..Default::default()
                },
            )
            .expect("register oauth");
        let credential = runtime
            .login(
                "p",
                maho_ai::auth::types::AuthType::OAuth,
                std::sync::Arc::new(TestAuthInteraction),
            )
            .await
            .expect("login");
        assert!(matches!(
            credential,
            maho_ai::auth::types::Credential::OAuth(_)
        ));
        assert!(runtime.is_using_oauth("p"));
        assert!(runtime.credentials.get("p").is_some());
        runtime
            .logout("p", None, Default::default())
            .await
            .expect("logout");
        assert!(runtime.credentials.get("p").is_none());
    }

    #[test]
    fn registered_oauth_provider_joins_and_leaves_the_oauth_provider_list() {
        let (_dir, mut runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime
            .register_provider(
                "p",
                crate::provider_composer::ProviderConfigInput {
                    oauth: Some(std::sync::Arc::new(TestOAuth)),
                    ..Default::default()
                },
            )
            .expect("register oauth");
        assert!(
            runtime
                .credentials
                .get_oauth_providers()
                .expect("builtin providers")
                .iter()
                .any(|p| p.id == "p" && p.name == "test-oauth")
        );
        runtime.unregister_provider("p");
        assert!(
            !runtime
                .credentials
                .get_oauth_providers()
                .expect("builtin providers")
                .iter()
                .any(|p| p.id == "p")
        );
    }

    #[tokio::test]
    async fn runtime_api_key_resolves_and_reports_runtime_status_until_removed() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime
            .set_runtime_api_key("p", "runtime-fixture", Default::default())
            .await
            .expect("set runtime key");
        assert_eq!(
            runtime
                .get_auth("p")
                .await
                .expect("resolve")
                .expect("auth")
                .auth
                .api_key
                .as_deref(),
            Some("runtime-fixture")
        );
        assert_eq!(
            runtime.get_provider_auth_status("p").source.as_deref(),
            Some("runtime")
        );
        runtime
            .remove_runtime_api_key("p", Default::default())
            .await
            .expect("remove runtime key");
        assert!(runtime.get_auth("p").await.expect("resolve").is_none());
    }

    #[tokio::test]
    async fn preaborted_credential_operation_does_not_run() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        let options = maho_ai::auth::types::AuthOperationOptions {
            signal: Some(controller.signal()),
        };
        assert!(
            runtime
                .set_runtime_api_key("p", "k", options)
                .await
                .is_err()
        );
        assert!(!runtime.credentials.has_runtime_api_key("p"));
    }

    /// Fails with a 401 for the key "bad" and completes for any other key, recording the keys used.
    struct KeyedStreams(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
    impl maho_ai::types::ProviderStreams for KeyedStreams {
        fn stream(
            &self,
            model: &Model,
            _: &Context,
            options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            let key = options
                .and_then(|options| options.request.api_key)
                .unwrap_or_default();
            self.0.lock().expect("keys").push(key.clone());
            if key == "bad" {
                return maho_ai::utils::lazy::error_stream(model, "401 invalid api key");
            }
            let mut message = maho_ai::utils::lazy::setup_error_message(model, "");
            message.error_message = None;
            message.stop_reason = maho_ai::types::StopReason::Stop;
            let stream = AssistantMessageEventStream::assistant();
            stream.push(maho_ai::types::AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop,
                message: message.clone(),
            });
            stream.end(Some(message));
            stream
        }
        fn stream_simple(
            &self,
            model: &Model,
            context: &Context,
            options: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream {
            self.stream(model, context, options.map(|options| options.stream))
        }
    }

    #[tokio::test]
    async fn a_stored_api_key_pool_rotates_past_a_rejected_slot_and_records_the_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let model = Model {
            id: "sol".into(),
            provider: "pool".into(),
            api: "openai-completions".into(),
            base_url: "http://localhost/v1".into(),
            ..maho_ai::providers::faux::faux_provider(Default::default())
                .expect("faux provider")
                .get_model(None)
                .expect("model")
        };
        let provider = maho_ai::models::create_provider(maho_ai::models::CreateProviderOptions {
            id: "pool".into(),
            name: None,
            base_url: None,
            headers: None,
            auth: None,
            models: vec![model.clone()],
            fetch_models: None,
            restore_models: None,
            filter_models: None,
            api: maho_ai::models::ProviderApi::Single(std::sync::Arc::new(KeyedStreams(
                keys.clone(),
            ))),
            images: IndexMap::new(),
        }).expect("valid pool provider");
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            providers: Some(vec![provider]),
            agent_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        }).expect("model runtime");
        // Automatic rotation across pooled accounts is gated behind recorded consent.
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"providerRiskConsent":{"pool:rotation":"2026-01-01T00:00:00.000Z"}}"#,
        )
        .expect("settings");
        runtime
            .credentials
            .set(
                "pool",
                Some(serde_json::json!({"type":"api_key","key":"bad","accounts":[
            {"name":"a","key":"bad"},{"name":"b","key":"good"}],"pinned":"a"})),
            )
            .expect("seed");
        let result = runtime
            .complete(&model, &Context::default(), None)
            .await
            .expect("rotated completion");
        assert_eq!(result.stop_reason, maho_ai::types::StopReason::Stop);
        assert_eq!(
            *keys.lock().expect("keys"),
            vec!["bad".to_owned(), "good".to_owned()]
        );
        let sidecar = std::fs::read_to_string(dir.path().join("credential-pool-state.json"))
            .expect("sidecar written");
        assert!(sidecar.contains("auth_error"));
    }

    /// Counts provider calls so a gated request can prove nothing was contacted.
    struct CountingStreams(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl maho_ai::types::ProviderStreams for CountingStreams {
        fn stream(&self, model: &Model, _: &Context, _: Option<StreamOptions>) -> AssistantMessageEventStream {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            maho_ai::utils::lazy::error_stream(model, "provider was contacted")
        }
        fn stream_simple(&self, model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
            self.stream(model, context, options.map(|options| options.stream))
        }
    }

    #[tokio::test]
    async fn consentless_impersonating_provider_fails_on_every_runtime_path_without_a_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let model = Model {
            id: "fixture".into(),
            provider: "github-copilot".into(),
            api: "openai-completions".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            ..maho_ai::providers::faux::faux_provider(Default::default())
                .expect("faux provider")
                .get_model(None)
                .expect("model")
        };
        let provider = maho_ai::models::create_provider(maho_ai::models::CreateProviderOptions {
            id: "github-copilot".into(),
            name: None,
            base_url: None,
            headers: None,
            auth: None,
            models: vec![model.clone()],
            fetch_models: None,
            restore_models: None,
            filter_models: None,
            api: maho_ai::models::ProviderApi::Single(std::sync::Arc::new(CountingStreams(calls.clone()))),
            images: IndexMap::new(),
        }).expect("provider");
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            providers: Some(vec![provider]),
            auth_path: Some(dir.path().join("auth.json")),
            agent_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        }).expect("model runtime");
        runtime
            .credentials
            .set("github-copilot", Some(serde_json::json!({"type":"api_key","key":"k"})))
            .expect("seed");
        let context = Context::default();
        let check = |message: maho_ai::types::AssistantMessage| {
            assert_eq!(message.stop_reason, maho_ai::types::StopReason::Error);
            assert!(message.error_message.expect("message").contains("`mhc auth consent github-copilot`"));
            assert_eq!(
                message.provider_diagnostic.and_then(|d| d.code).as_deref(),
                Some("provider_consent_required")
            );
        };
        check(runtime.complete(&model, &context, None).await.expect("stream result"));
        let stream = runtime.stream_simple(&model, &context, None);
        while let Ok(Some(_)) = stream.next().await {}
        check(stream.result().await.expect("simple result"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"providerRiskConsent":{"github-copilot":"2026-01-01T00:00:00.000Z"}}"#,
        ).expect("settings");
        let _ = runtime.complete(&model, &context, None).await;
        assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn consentless_expired_oauth_credential_fails_get_auth_before_any_refresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            auth_path: Some(dir.path().join("auth.json")),
            agent_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        }).expect("model runtime");
        runtime
            .credentials
            .set(
                "github-copilot",
                Some(serde_json::json!({"type":"oauth","access":"tid=fake","refresh":"ghu_fake","expires":1})),
            )
            .expect("seed expired oauth");
        // Resolving would refresh over the network; the gate must fail first with the consent code.
        let error = runtime.get_auth("github-copilot").await.expect_err("consent required");
        assert_eq!(error.code, ModelsErrorCode::ConsentRequired);
        assert_eq!(error.code.as_str(), "provider_consent_required");
        assert!(error.message.contains("`mhc auth consent github-copilot`"), "{}", error.message);
        let overrides = AuthResolutionOverrides::default();
        let error = runtime
            .get_auth_with_overrides("github-copilot", &overrides)
            .await
            .expect_err("consent required");
        assert_eq!(error.code, ModelsErrorCode::ConsentRequired);
        let model = maho_ai::providers::faux::faux_provider(Default::default())
            .expect("faux provider")
            .get_model(None)
            .expect("model");
        let model = Model { provider: "github-copilot".into(), ..model };
        let error = runtime
            .get_auth_for_model(&model, &overrides)
            .await
            .expect_err("consent required");
        assert_eq!(error.code, ModelsErrorCode::ConsentRequired);
    }

    struct AmbientOAuth;
    struct CloudAmbientAuth;
    #[async_trait::async_trait]
    impl maho_ai::auth::types::ApiKeyAuth for CloudAmbientAuth {
        fn name(&self) -> &str { "cloud ambient" }
        async fn resolve(
            &self,
            _: &dyn maho_ai::auth::types::AuthContext,
            _: Option<&ApiKeyCredential>,
            _: &AbortSignal,
        ) -> anyhow::Result<Option<maho_ai::auth::types::AuthResult>> {
            Ok(Some(maho_ai::auth::types::AuthResult {
                ambient: true,
                auth: Default::default(),
                env: None,
                source: Some("cloud chain".into()),
            }))
        }
    }

    #[tokio::test]
    async fn ambient_auth_admits_models_without_synthetic_api_keys() {
        let (_dir, mut runtime) = configured_runtime(
            r#"{"providers":{"cloud-fixture":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime.register_provider("cloud-fixture", ProviderConfigInput {
            ambient_auth: Some(Arc::new(CloudAmbientAuth)),
            ..Default::default()
        }).expect("register ambient provider");
        let auth = runtime.get_auth("cloud-fixture").await.expect("resolve").expect("ambient auth");
        assert!(auth.ambient);
        assert_eq!(auth.auth.api_key, None);
        let available = runtime.get_available(Some("cloud-fixture")).await.expect("available models");
        assert_eq!(available.len(), 1);
        // A provider-scoped read intentionally bypasses the availability snapshot. Synchronous
        // status reads observe the full refresh, including ambient auth without a stored key.
        runtime.get_available(None).await.expect("refresh auth snapshot");
        let status = runtime.get_provider_auth_status("cloud-fixture");
        assert!(status.configured);
        assert!(status.ambient);
    }

    #[async_trait::async_trait]
    impl maho_ai::auth::types::OAuthAuth for AmbientOAuth {
        fn name(&self) -> &str {
            "ambient-oauth"
        }
        async fn login(
            &self,
            _: &maho_ai::auth::types::ProviderAuthInteraction,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            anyhow::bail!("login is not exercised")
        }
        async fn refresh(
            &self,
            credential: &maho_ai::auth::types::OAuthCredential,
            _: &maho_ai::utils::abort::AbortSignal,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            Ok(credential.clone())
        }
        async fn to_auth(
            &self,
            _: &maho_ai::auth::types::OAuthCredential,
        ) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(Default::default())
        }
        fn has_resolve_ambient(&self) -> bool {
            true
        }
        async fn resolve_ambient(
            &self,
            _: &dyn maho_ai::auth::types::AuthContext,
            _: Option<&maho_ai::types::ProviderEnv>,
            _: &maho_ai::utils::abort::AbortSignal,
        ) -> anyhow::Result<Option<maho_ai::auth::types::AuthResult>> {
            Ok(Some(maho_ai::auth::types::AuthResult {
                ambient: false,
                auth: maho_ai::auth::types::ModelAuth {
                    api_key: Some("ambient-token".into()),
                    ..Default::default()
                },
                env: None,
                source: Some("ambient CLI".into()),
            }))
        }
    }

    #[tokio::test]
    async fn an_ambient_oauth_provider_resolves_without_a_stored_credential() {
        let (_dir, mut runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime
            .register_provider(
                "p",
                crate::provider_composer::ProviderConfigInput {
                    oauth: Some(std::sync::Arc::new(AmbientOAuth)),
                    ..Default::default()
                },
            )
            .expect("register oauth");
        assert_eq!(
            runtime
                .get_auth("p")
                .await
                .expect("resolve")
                .expect("ambient auth")
                .auth
                .api_key
                .as_deref(),
            Some("ambient-token")
        );
        assert!(
            runtime
                .get_available(Some("p"))
                .await
                .expect("available models")
                .iter()
                .any(|model| model.id == "sol")
        );
    }

    #[tokio::test]
    async fn create_defaults_the_models_store_beside_models_json_and_restores_offline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models_path = dir.path().join("models.json");
        std::fs::write(&models_path, r#"{"providers":{}}"#).expect("config");
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            models_path: Some(models_path),
            providers: Some(Vec::new()),
            ..Default::default()
        })
        .await.expect("model runtime");
        assert_eq!(runtime.get_error(), None);
        assert!(!runtime.model_network_enabled);
        assert_eq!(runtime.model_refresh_timeout_ms(), 15_000);
    }

    #[tokio::test]
    async fn login_rejects_unknown_provider_and_unsupported_oauth() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        let unknown = runtime
            .login(
                "missing",
                maho_ai::auth::types::AuthType::ApiKey,
                std::sync::Arc::new(TestAuthInteraction),
            )
            .await
            .expect_err("unknown provider");
        assert_eq!(unknown.code, ModelsErrorCode::Provider);
        let unsupported = runtime
            .login(
                "p",
                maho_ai::auth::types::AuthType::OAuth,
                std::sync::Arc::new(TestAuthInteraction),
            )
            .await
            .expect_err("no oauth method");
        assert_eq!(unsupported.code, ModelsErrorCode::Auth);
    }

    #[tokio::test]
    async fn api_key_login_prompts_and_persists() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        let credential = runtime
            .login(
                "p",
                maho_ai::auth::types::AuthType::ApiKey,
                std::sync::Arc::new(TestAuthInteraction),
            )
            .await
            .expect("api key login");
        assert_eq!(
            credential,
            maho_ai::auth::types::Credential::ApiKey(maho_ai::auth::types::ApiKeyCredential {
                key: Some("entered-key".into()),
                env: None,
                ..Default::default()
            })
        );
        assert_eq!(
            runtime.credentials.get("p").and_then(|value| value
                .get("key")
                .and_then(|key| key.as_str().map(str::to_owned))),
            Some("entered-key".into())
        );
    }

    /// Hides every model but `keep`, like a provider entitlement filter.
    struct FilteringProvider;
    impl Provider for FilteringProvider {
        fn id(&self) -> &str {
            "filtering"
        }
        fn name(&self) -> &str {
            "filtering"
        }
        fn get_models(&self) -> Vec<Model> {
            ["keep","hide"].into_iter().map(|id| serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "api": "openai-completions", "provider": "filtering", "baseUrl": "http://localhost/v1",
                "reasoning": false, "input": [], "contextWindow": 128000, "maxTokens": 4096,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
            })).expect("model")).collect()
        }
        fn filter_models(
            &self,
            models: Vec<Model>,
            credential: Option<&maho_ai::models::Credential>,
        ) -> Vec<Model> {
            assert!(
                credential.is_some(),
                "filter receives the stored credential"
            );
            models
                .into_iter()
                .filter(|model| model.id == "keep")
                .collect()
        }
        fn stream(
            &self,
            _model: &Model,
            _context: &Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            maho_ai::utils::event_stream::create_assistant_message_event_stream()
        }
        fn stream_simple(
            &self,
            _model: &Model,
            _context: &Context,
            _options: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream {
            maho_ai::utils::event_stream::create_assistant_message_event_stream()
        }
    }
    fn filtering_registry() -> crate::model_registry::ModelRegistry {
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            providers: Some(vec![Arc::new(FilteringProvider) as Arc<dyn Provider>]),
            ..Default::default()
        }).expect("model runtime");
        runtime
            .credentials
            .set(
                "filtering",
                Some(serde_json::json!({"type":"api_key","key":"fixture"})),
            )
            .expect("seed");
        crate::model_registry::ModelRegistry::new(runtime)
    }
    #[test]
    fn registry_available_applies_provider_filter_before_snapshot() {
        let registry = filtering_registry();
        assert!(!registry.model_runtime.has_availability_snapshot());
        let ids: Vec<_> = registry
            .get_available()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, ["keep"]);
    }
    #[tokio::test]
    async fn registry_available_reads_runtime_snapshot_after_refresh() {
        let registry = filtering_registry();
        let async_ids: Vec<_> = registry
            .model_runtime
            .get_available(None)
            .await
            .expect("available models")
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(registry.model_runtime.has_fresh_availability_snapshot());
        assert!(registry.model_runtime.has_configured_auth("filtering"));
        let sync_ids: Vec<_> = registry
            .get_available()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            (async_ids, sync_ids),
            (vec!["keep".to_owned()], vec!["keep".to_owned()])
        );
    }
    #[test]
    fn register_provider_provisionally_marks_configured_provider() {
        let (_dir, mut runtime) = configured_runtime(r#"{"providers":{}}"#);
        runtime
            .register_provider(
                "ext",
                crate::model_config_schema::ModelsJsonProvider {
                    api: Some("openai-completions".into()),
                    base_url: Some("http://localhost/v1".into()),
                    api_key: Some("fixture-key".into()),
                    models: Some(vec![crate::model_config_schema::ModelsJsonModel {
                        id: "sol".into(),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }
                .into(),
            )
            .expect("register");
        assert!(runtime.has_configured_auth("ext"));
        assert!(
            runtime
                .get_available_snapshot()
                .iter()
                .any(|model| model.provider == "ext" && model.id == "sol")
        );
    }
    struct FailingAvailabilityAuth {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        fail_at: usize,
    }
    impl ModelsAuth for FailingAvailabilityAuth {
        fn resolve<'a>(
            &'a self,
            _: &'a dyn Provider,
            _: &'a AuthResolutionOverrides,
        ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
            Box::pin(async move {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                if call == self.fail_at {
                    return Err(ModelsError::new(ModelsErrorCode::OAuth, "availability-fixture"));
                }
                Ok(Some(AuthResolution::default()))
            })
        }
        fn read_credential<'a>(
            &'a self,
            _: &'a str,
            _: &'a AbortSignal,
        ) -> BoxFuture<'a, Result<Option<serde_json::Value>, ModelsError>> {
            Box::pin(async { Ok(Some(serde_json::json!({"type":"api_key","key":"fixture"}))) })
        }
        fn refresh_credential<'a>(
            &'a self,
            _: &'a dyn Provider,
            stored: Option<&'a serde_json::Value>,
            _: &'a AbortSignal,
        ) -> BoxFuture<'a, Result<Option<serde_json::Value>, ModelsError>> {
            Box::pin(async move { Ok(stored.cloned()) })
        }
    }
    async fn failing_availability_runtime(fail_at: usize) -> (ModelRuntime, Arc<std::sync::atomic::AtomicUsize>) {
        let mut runtime = filtering_registry().model_runtime;
        runtime.get_available(None).await.expect("seed available snapshot");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        runtime.models = create_models(Some(CreateModelsOptions {
            auth: Some(Arc::new(FailingAvailabilityAuth { calls: calls.clone(), fail_at })),
            ..Default::default()
        }));
        runtime.models.set_provider(Arc::new(FilteringProvider));
        (runtime, calls)
    }
    #[tokio::test]
    async fn available_propagates_provider_errors_without_returning_stale_models() {
        let (runtime, calls) = failing_availability_runtime(1).await;
        let error = runtime.get_available(Some("filtering")).await.expect_err("provider error");
        assert_eq!(error.code, ModelsErrorCode::OAuth);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(runtime.availability_state().error.as_ref(), Some(&error));
        assert!(runtime.get_error().is_some_and(|message| message.contains(&error.message)));
        assert!(!runtime.has_fresh_availability_snapshot());
    }
    #[tokio::test]
    async fn available_propagates_full_pass_errors_without_retrying_registry() {
        let (runtime, calls) = failing_availability_runtime(1).await;
        let error = runtime.get_available(None).await.expect_err("full availability error");
        assert_eq!(error.code, ModelsErrorCode::OAuth);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(runtime.availability_state().error.as_ref(), Some(&error));
    }
    #[tokio::test]
    async fn available_propagates_auth_check_errors_after_registry_success() {
        let (runtime, calls) = failing_availability_runtime(2).await;
        let error = runtime.get_available(None).await.expect_err("auth check error");
        assert_eq!(error.code, ModelsErrorCode::OAuth);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(runtime.availability_state().error.as_ref(), Some(&error));
    }
    #[tokio::test]
    async fn session_restore_propagates_availability_failure() {
        let (runtime, _) = failing_availability_runtime(1).await;
        let result = crate::model_resolver::restore_model_from_session("missing", "missing", None, &runtime).await;
        let error = result.err().expect("restore error");
        assert_eq!(error.code, ModelsErrorCode::OAuth);
    }
    #[tokio::test]
    async fn initial_model_propagates_availability_failure() {
        let (runtime, _) = failing_availability_runtime(1).await;
        let result = crate::model_resolver::find_initial_model(crate::model_resolver::InitialModelOptions {
            cli_provider: None, cli_model: None, scoped_models: &[], is_continuing: false,
            default_provider: None, default_model_id: None, model_thinking_levels: None,
        }, &runtime).await;
        assert!(result.is_err());
        assert_eq!(runtime.availability_state().error.as_ref().map(|error| error.code), Some(ModelsErrorCode::OAuth));
    }
    /// Answers selects with the first option id and every text/secret prompt with its own message,
    /// recording committed account receipts.
    #[derive(Default)]
    struct ScriptedInteraction {
        prompts: std::sync::Mutex<Vec<String>>,
        receipts: std::sync::Mutex<Vec<maho_ai::auth::types::AccountLoginReceipt>>,
    }
    #[async_trait::async_trait]
    impl maho_ai::auth::types::AuthInteraction for ScriptedInteraction {
        fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> {
            None
        }
        fn on_account_committed(&self, receipt: maho_ai::auth::types::AccountLoginReceipt) {
            self.receipts.lock().expect("receipts").push(receipt);
        }
        async fn prompt(&self, prompt: maho_ai::auth::types::AuthPrompt) -> anyhow::Result<String> {
            use maho_ai::auth::types::AuthPromptKind;
            let (message, answer) = match prompt.kind {
                AuthPromptKind::Select { message, options } => (
                    message,
                    options
                        .first()
                        .map(|option| option.id.clone())
                        .unwrap_or_default(),
                ),
                AuthPromptKind::Text { message, .. }
                | AuthPromptKind::Secret { message, .. }
                | AuthPromptKind::ManualCode { message, .. } => (message.clone(), message),
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
        let credential = runtime
            .login(
                "cloudflare-ai-gateway",
                maho_ai::auth::types::AuthType::ApiKey,
                interaction.clone(),
            )
            .await
            .expect("login");
        let maho_ai::auth::types::Credential::ApiKey(credential) = credential else {
            panic!("api key credential")
        };
        let env = credential.env.expect("provider login env");
        assert!(
            env.contains_key("CLOUDFLARE_ACCOUNT_ID") && env.contains_key("CLOUDFLARE_GATEWAY_ID")
        );
        assert!(interaction.prompts.lock().expect("prompts").len() > 1);
        assert!(
            runtime
                .credentials
                .get("cloudflare-ai-gateway")
                .expect("stored")["env"]["CLOUDFLARE_ACCOUNT_ID"]
                .is_string()
        );
    }

    #[tokio::test]
    async fn bedrock_login_asks_for_the_method_first() {
        let runtime = builtin_runtime("amazon-bedrock");
        let interaction = std::sync::Arc::new(ScriptedInteraction::default());
        runtime
            .login(
                "amazon-bedrock",
                maho_ai::auth::types::AuthType::ApiKey,
                interaction.clone(),
            )
            .await
            .expect("login");
        assert_eq!(
            interaction
                .prompts
                .lock()
                .expect("prompts")
                .first()
                .map(String::as_str),
            Some("Select Amazon Bedrock authentication method:")
        );
    }

    #[tokio::test]
    async fn second_oauth_login_appends_a_slot_and_commits_its_receipt() {
        let (_dir, mut runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime
            .register_provider(
                "p",
                crate::provider_composer::ProviderConfigInput {
                    oauth: Some(std::sync::Arc::new(TestOAuth)),
                    ..Default::default()
                },
            )
            .expect("register oauth");
        runtime.credentials.set("p", Some(serde_json::json!({"type":"oauth","access":"first","refresh":"first-r","expires":1.0}))).expect("seed");
        let interaction = std::sync::Arc::new(ScriptedInteraction::default());
        runtime
            .login(
                "p",
                maho_ai::auth::types::AuthType::OAuth,
                interaction.clone(),
            )
            .await
            .expect("login");
        let stored = runtime.credentials.get("p").expect("stored");
        let names: Vec<_> = stored["accounts"]
            .as_array()
            .expect("accounts")
            .iter()
            .filter_map(|slot| slot["name"].as_str())
            .collect();
        assert_eq!(names, ["default", "login-2"]);
        let receipts = interaction.receipts.lock().expect("receipts");
        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.name.as_str())
                .collect::<Vec<_>>(),
            ["login-2"]
        );
    }

    #[tokio::test]
    async fn logout_with_slot_id_removes_only_that_slot() {
        let (_dir, runtime) = configured_runtime(
            r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
        );
        runtime.credentials.set("p", Some(serde_json::json!({"type":"api_key","key":"a","accounts":[{"name":"default","key":"a"},{"name":"work","key":"b"}]}))).expect("seed");
        runtime
            .logout("p", Some("work"), Default::default())
            .await
            .expect("slot logout");
        let stored = runtime.credentials.get("p").expect("remaining pool");
        assert_eq!(stored["accounts"].as_array().expect("accounts").len(), 1);
        assert_eq!(stored["key"], "a");
        runtime
            .logout("p", Some("default"), Default::default())
            .await
            .expect("last slot logout");
        assert!(runtime.credentials.get("p").is_none());
    }

    #[test]
    fn real_models_copy_qa_when_requested() {
        let config = match std::env::var("TASK17_MODELS_COPY") {
            Ok(path) => ModelConfig::load_sync(Some(std::path::Path::new(&path))),
            Err(_) => ModelConfig::parse(
                r#"{"providers":{"kiro-lb":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"claude-sonnet-5"},{"id":"claude-opus-5.5"}]}}}"#,
                "fixture/models.json",
            ),
        };
        assert!(config.get_error().is_none(), "{:?}", config.get_error());
        let models = crate::provider_composer::compose_models(
            "kiro-lb",
            &[],
            config.get_provider("kiro-lb"),
            None,
        )
        .expect("compose configured catalog");
        assert!(!models.is_empty());
        for model in models {
            println!(
                "RESOLVE {} api={} baseUrl={}",
                model.id, model.api, model.base_url
            );
        }
        let (_dir, unknown) = configured_runtime(
            r#"{"providers":{"unknown-task-17":{"models":[{"id":"missing-api"}]}}}"#,
        );
        println!("UNKNOWN {}", unknown.get_error().expect("error"));
        assert!(
            !unknown
                .get_models(None)
                .iter()
                .any(|m| m.provider == "unknown-task-17")
        );
    }
}
