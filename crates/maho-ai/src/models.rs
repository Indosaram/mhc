//! Port of senpi packages/ai/src/models.ts.
//!
//! Auth resolution, credential storage and OAuth login live in `auth/` (todo 13); the collection
//! reaches them through the `ModelsAuth` seam. `EnvModelsAuth` is the default and resolves API keys
//! from provider env vars only.

use crate::env_api_keys::get_env_api_key;
use crate::models_store::{
    InMemoryModelsStore, ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions,
};
use crate::types::{
    AssistantMessage, AssistantMessageEventStream, BoxFuture, Context, DeferredHandle, Model,
    ModelCostRates, ModelThinkingLevel, ProviderEnv, ProviderHeaders, ProviderStreams,
    SimpleStreamOptions, StreamOptions, ThinkingLevelMap, Usage, UsageCost,
};
use crate::utils::abort::{
    AbortController, AbortReason, AbortSignal, operation_signal, race_with_abort_signal,
};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::diagnostics::now_ms;
use crate::utils::lazy::error_stream;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

#[path = "api/cloudflare_workers_ai_system_one.rs"]
pub mod cloudflare_workers_ai_system_one;
#[path = "api/llama_cpp_classify.rs"]
pub mod llama_cpp_classify;
#[path = "api/system_one_shared.rs"]
pub mod system_one_shared;
#[path = "api/typesafe_system_one.rs"]
pub mod typesafe_system_one;

use crate::types::{
    AnyModel, ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult,
    ProviderClassifier,
};

pub struct BuiltinClassifier;

#[cfg(test)]
#[path = "api/classifier_tests.rs"]
mod classifier_tests;

impl ProviderClassifier for BuiltinClassifier {
    fn classify<'a>(
        &'a self,
        model: &'a ClassifierModel,
        context: &'a ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> BoxFuture<'a, ClassifierResult> {
        Box::pin(async move {
            match model.api.as_str() {
                "typesafe-system-one" => {
                    typesafe_system_one::classify(model, context, options).await
                }
                "cloudflare-workers-ai-system-one" => {
                    let mut resolved = model.clone();
                    let env = options
                        .as_ref()
                        .and_then(|options| options.request.env.as_ref());
                    resolved.base_url =
                        crate::providers::cloudflare_stream::resolve_cloudflare_model(
                            &system_one_shared::request_model(model),
                            env,
                        )
                        .base_url;
                    cloudflare_workers_ai_system_one::classify(&resolved, context, options).await
                }
                "llama-cpp-classify" => llama_cpp_classify::classify(model, context, options).await,
                _ => {
                    let mut result = system_one_shared::result(model);
                    system_one_shared::fail(
                        &mut result,
                        &options.unwrap_or_default(),
                        format!("Unsupported classifier API: {}", model.api),
                    );
                    result
                }
            }
        })
    }
}

#[derive(Clone, Default)]
pub struct ModelsClassifierOptions {
    pub options: ClassifierOptions,
    pub transforms: ModelsRequestTransforms,
}

pub fn typesafe_provider() -> Result<Arc<dyn Provider>, ModelsError> {
    create_provider(CreateProviderOptions {
        id: "typesafe".into(),
        name: Some("TypeSafe".into()),
        base_url: None,
        headers: None,
        auth: Some(crate::auth::types::ProviderAuth {
            api_key: Some(Arc::new(crate::auth::helpers::env_api_key_auth(
                "TypeSafe API key",
                &["TYPESAFE_API_KEY"],
            ))),
            oauth: None,
        }),
        models: Vec::new(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::ByApi(IndexMap::new()),
        images: IndexMap::new(),
    })
}

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
    /// Recorded risk consent is missing for an impersonating provider (mhc divergence SD-4).
    ConsentRequired,
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
            ModelsErrorCode::ConsentRequired => "provider_consent_required",
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
        Self {
            code,
            message: message.into(),
        }
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
    pub ambient: bool,
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

#[derive(Clone, Default)]
pub struct ModelsLoginOptions {
    pub device_id: Option<String>,
}

#[derive(Clone, Default)]
pub struct ModelsLogoutOptions {
    pub signal: Option<AbortSignal>,
    pub slot_id: Option<String>,
}

/// The auth surface the models collection needs.
pub trait ModelsAuth: Send + Sync {
    fn login<'a>(
        &'a self,
        provider: &'a dyn Provider,
        _auth_type: crate::auth::types::AuthType,
        _interaction: Arc<dyn crate::auth::types::AuthInteraction>,
        _options: ModelsLoginOptions,
    ) -> BoxFuture<'a, Result<crate::auth::types::Credential, ModelsError>> {
        Box::pin(async move {
            Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Login is not supported for {}", provider.id()),
            ))
        })
    }

    fn logout<'a>(
        &'a self,
        provider_id: &'a str,
        _options: ModelsLogoutOptions,
    ) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async move {
            Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Logout is not supported for {provider_id}"),
            ))
        })
    }

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

    /// Side-effect-free availability check (`checkProviderAuth`). The default treats a successful
    /// resolution as configured, which is senpi's behavior for an api-key auth without `check`.
    fn check<'a>(
        &'a self,
        provider: &'a dyn Provider,
        signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<Option<crate::auth::types::AuthCheck>, ModelsError>> {
        Box::pin(async move {
            let overrides = AuthResolutionOverrides {
                signal: Some(signal.clone()),
                ..AuthResolutionOverrides::default()
            };
            Ok(self.resolve(provider, &overrides).await?.map(|resolution| {
                crate::auth::types::AuthCheck {
                    ambient: resolution.ambient,
                    source: None,
                    auth_type: crate::auth::types::AuthType::ApiKey,
                }
            }))
        })
    }
}

/// `get_env_api_key`'s value for ambient (non-key) credentials such as AWS profiles or Vertex ADC.
pub const AMBIENT_AUTH_MARKER: &str = "<authenticated>";

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
            let api_key = overrides
                .api_key
                .clone()
                .or_else(|| get_env_api_key(provider.id(), overrides.env.as_ref()));
            // senpi's ambient providers (Bedrock SigV4/bearer env, Vertex ADC) resolve to `auth: {}`;
            // the `<authenticated>` marker means "configured" and is never a request key.
            Ok(api_key.map(|api_key| AuthResolution {
                ambient: api_key == AMBIENT_AUTH_MARKER && overrides.api_key.is_none(),
                auth: ProviderAuthResult {
                    api_key: (api_key != AMBIENT_AUTH_MARKER).then_some(api_key),
                    ..ProviderAuthResult::default()
                },
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
            Ok(get_env_api_key(provider.id(), None)
                .filter(|key| key != AMBIENT_AUTH_MARKER)
                .map(|key| serde_json::json!({ "type": "api_key", "key": key })))
        })
    }
}

/// senpi `ModelsImpl`'s default auth: every provider resolves through its own declared
/// `provider.auth` (`resolveProviderAuth`) against an in-memory credential store and the default
/// auth context. A provider registered without declared auth keeps the env-var lookup.
pub struct ProviderModelsAuth {
    credentials: Arc<dyn crate::auth::types::CredentialStore>,
    auth_context: Arc<dyn crate::auth::types::AuthContext>,
}

impl Default for ProviderModelsAuth {
    fn default() -> Self {
        Self {
            credentials: Arc::new(crate::auth::credential_store::InMemoryCredentialStore::new()),
            auth_context: Arc::new(crate::auth::context::default_provider_auth_context()),
        }
    }
}

impl ProviderModelsAuth {
    pub fn new(
        credentials: Arc<dyn crate::auth::types::CredentialStore>,
        auth_context: Arc<dyn crate::auth::types::AuthContext>,
    ) -> Self {
        Self {
            credentials,
            auth_context,
        }
    }

    async fn read_stored(
        &self,
        provider_id: &str,
        signal: &AbortSignal,
    ) -> Result<Option<crate::auth::types::Credential>, ModelsError> {
        self.credentials
            .read(
                provider_id,
                Some(crate::auth::types::AuthOperationOptions {
                    signal: Some(signal.clone()),
                }),
            )
            .await
            .map_err(|error| {
                ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store read failed for {provider_id}"),
                    &error.to_string(),
                )
            })
    }
}

fn auth_models_error(error: crate::auth::resolve::ModelsError) -> ModelsError {
    let code = if error.code == "oauth" {
        ModelsErrorCode::OAuth
    } else {
        ModelsErrorCode::Auth
    };
    ModelsError::new(code, error.message)
}

impl ModelsAuth for ProviderModelsAuth {
    fn login<'a>(
        &'a self,
        provider: &'a dyn Provider,
        auth_type: crate::auth::types::AuthType,
        interaction: Arc<dyn crate::auth::types::AuthInteraction>,
        options: ModelsLoginOptions,
    ) -> BoxFuture<'a, Result<crate::auth::types::Credential, ModelsError>> {
        use crate::auth::pool::slots::{LoginSlotOrigin, PooledCredential, append_login_slot};
        use crate::auth::types::{
            AccountLoginOrigin, AccountLoginReceipt, AuthOperationOptions, AuthType, Credential,
            ProviderAuthInteraction,
        };
        Box::pin(async move {
            let signal = operation_signal(interaction.signal());
            let abort_error =
                |reason: AbortReason| ModelsError::new(ModelsErrorCode::Auth, reason.message);
            signal.throw_if_aborted().map_err(abort_error)?;
            let unsupported = || {
                ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("{} does not support {auth_type:?} login", provider.name()),
                )
            };
            let auth = provider.auth().ok_or_else(unsupported)?;
            let mut provider_interaction =
                ProviderAuthInteraction::new(signal.clone(), interaction.clone());
            if let Some(device_id) = options.device_id {
                provider_interaction = provider_interaction.with_device_id(device_id);
            }
            let login = async {
                match auth_type {
                    AuthType::ApiKey => {
                        let method = auth
                            .api_key
                            .as_ref()
                            .filter(|method| method.has_login())
                            .ok_or_else(unsupported)?;
                        method.login(&provider_interaction).await.map(|mut credential| {
                            if method.key_optional() && credential.key.as_ref().is_some_and(|key| key.trim().is_empty()) { credential.key = None; }
                            Credential::ApiKey(credential)
                        })
                    }
                    AuthType::OAuth => {
                        let method = auth.oauth.as_ref().ok_or_else(unsupported)?;
                        method
                            .login(&provider_interaction)
                            .await
                            .map(Credential::OAuth)
                    }
                }
                .map_err(|error| {
                    ModelsError::with_cause(
                        ModelsErrorCode::Auth,
                        format!("Login failed for {}", provider.id()),
                        &error.to_string(),
                    )
                })
            };
            let logged_in = race_with_abort_signal(login, &signal)
                .await
                .map_err(abort_error)??;
            let key_optional = auth_type == AuthType::ApiKey
                && auth.api_key.as_ref().is_some_and(|k| k.key_optional());
            let credential = if key_optional {
                crate::auth::pool::slots::without_blank_key(logged_in)
            } else {
                logged_in
            };
            crate::auth::pool::slots::assert_login_key_present(&credential)
                .map_err(|err| ModelsError::new(ModelsErrorCode::Auth, err.to_string()))?;
            let flat = PooledCredential::from(credential.clone());
            let (receipt_tx, receipt_rx) = tokio::sync::oneshot::channel();
            self.credentials
                .modify(
                    provider.id(),
                    Box::new(move |current| {
                        Box::pin(async move {
                            let mut allocated = None;
                            let next = append_login_slot(
                                current.map(PooledCredential::from),
                                flat,
                                |name, origin| {
                                    allocated = Some((
                                        name.to_owned(),
                                        match origin {
                                            LoginSlotOrigin::Generated => {
                                                AccountLoginOrigin::Generated
                                            }
                                            LoginSlotOrigin::Provider => {
                                                AccountLoginOrigin::Provider
                                            }
                                        },
                                    ));
                                },
                            )
                            .map_err(anyhow::Error::msg)?;
                            // The receipt is published to the caller only after modify succeeds.
                            let _ = receipt_tx.send(allocated);
                            Ok(Some(next.to_stored_credential()))
                        })
                    }),
                    Some(AuthOperationOptions {
                        signal: Some(signal),
                    }),
                )
                .await
                .map_err(|error| {
                    ModelsError::with_cause(
                        ModelsErrorCode::Auth,
                        format!("Credential store modify failed for {}", provider.id()),
                        &error.to_string(),
                    )
                })?;
            if let Ok(Some((name, origin))) = receipt_rx.await {
                interaction.on_account_committed(AccountLoginReceipt {
                    provider_id: provider.id().to_owned(),
                    name,
                    origin,
                });
            }
            Ok(credential)
        })
    }

    fn logout<'a>(
        &'a self,
        provider_id: &'a str,
        options: ModelsLogoutOptions,
    ) -> BoxFuture<'a, Result<(), ModelsError>> {
        use crate::auth::pool::slots::{PooledCredential, remove_slot};
        use crate::auth::types::AuthOperationOptions;
        Box::pin(async move {
            let signal = operation_signal(options.signal);
            let operation = Some(AuthOperationOptions {
                signal: Some(signal),
            });
            let result = async {
                if let Some(name) = options.slot_id {
                    self.credentials
                        .modify_or_delete(
                            provider_id,
                            Box::new(move |current| {
                                Box::pin(async move {
                                    let next =
                                        remove_slot(current.map(PooledCredential::from), &name);
                                    Ok(next.map(|pool| pool.to_stored_credential()))
                                })
                            }),
                            operation,
                        )
                        .await?;
                } else {
                    self.credentials.delete(provider_id, operation).await?;
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
            result.map_err(|error| {
                ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store delete failed for {provider_id}"),
                    &error.to_string(),
                )
            })
        })
    }

    fn resolve<'a>(
        &'a self,
        provider: &'a dyn Provider,
        overrides: &'a AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
        Box::pin(async move {
            let Some(provider_auth) = provider.auth() else {
                return EnvModelsAuth.resolve(provider, overrides).await;
            };
            let signal = operation_signal(overrides.signal.clone());
            let result = crate::auth::resolve::resolve_provider_auth(
                provider.id(),
                provider_auth,
                &self.credentials,
                self.auth_context.as_ref(),
                Some(overrides),
                &signal,
            )
            .await
            .map_err(auth_models_error)?;
            Ok(result.map(|result| AuthResolution {
                ambient: result.ambient,
                auth: ProviderAuthResult {
                    api_key: result.auth.api_key,
                    headers: result.auth.headers,
                    base_url: result.auth.base_url,
                },
                env: result.env,
            }))
        })
    }

    fn read_credential<'a>(
        &'a self,
        provider_id: &'a str,
        signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async move {
            self.read_stored(provider_id, signal)
                .await?
                .map(|credential| {
                    serde_json::to_value(credential)
                        .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.to_string()))
                })
                .transpose()
        })
    }

    fn refresh_credential<'a>(
        &'a self,
        provider: &'a dyn Provider,
        stored: Option<&'a Credential>,
        signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async move {
            let Some(provider_auth) = provider.auth() else {
                return EnvModelsAuth
                    .refresh_credential(provider, stored, signal)
                    .await;
            };
            let stored = match stored {
                Some(_) => self.read_stored(provider.id(), signal).await?,
                None => None,
            };
            let credential = crate::auth::refresh_credential::resolve_refresh_credential(
                provider.id(),
                provider_auth,
                &self.credentials,
                self.auth_context.as_ref(),
                stored.as_ref(),
                signal,
            )
            .await
            .map_err(auth_models_error)?;
            credential
                .map(|credential| {
                    serde_json::to_value(credential)
                        .map_err(|error| ModelsError::new(ModelsErrorCode::Auth, error.to_string()))
                })
                .transpose()
        })
    }

    /// senpi `checkProviderAuth`: side-effect-free; never refreshes OAuth.
    fn check<'a>(
        &'a self,
        provider: &'a dyn Provider,
        signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<Option<crate::auth::types::AuthCheck>, ModelsError>> {
        Box::pin(async move {
            let Some(provider_auth) = provider.auth() else {
                return EnvModelsAuth.check(provider, signal).await;
            };
            let id = provider.id();
            let ctx = self.auth_context.as_ref();
            let credential = self.read_stored(id, signal).await?;
            let check_error = |kind: &str, error: anyhow::Error| {
                ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("{kind} auth check failed for provider {id}"),
                    &error.to_string(),
                )
            };
            if let Some(crate::auth::types::Credential::OAuth(credential)) = &credential {
                let Some(oauth) = &provider_auth.oauth else {
                    return Ok(None);
                };
                return oauth
                    .check(ctx, Some(credential), signal)
                    .await
                    .map_err(|error| check_error("OAuth", error));
            }
            if let Some(api_key) = &provider_auth.api_key {
                let stored = credential
                    .as_ref()
                    .and_then(crate::auth::types::Credential::as_api_key);
                if api_key.has_check() {
                    if let Some(check) = api_key
                        .check(ctx, stored, signal)
                        .await
                        .map_err(|error| check_error("API key", error))?
                    {
                        return Ok(Some(check));
                    }
                } else if let Some(resolution) = api_key
                    .resolve(ctx, stored, signal)
                    .await
                    .map_err(|error| check_error("API key", error))?
                {
                    return Ok(Some(crate::auth::types::AuthCheck {
                        source: resolution.source,
                        auth_type: crate::auth::types::AuthType::ApiKey,
                        ambient: resolution.ambient,
                    }));
                }
            }
            match &provider_auth.oauth {
                Some(oauth) => oauth
                    .check(ctx, None, signal)
                    .await
                    .map_err(|error| check_error("OAuth", error)),
                None => Ok(None),
            }
        })
    }
}

/// `ModelsPublication`: `persist: None` leaves the store untouched, `Some(None)` deletes the entry.
#[derive(Default)]
pub struct ModelsPublication {
    pub persist: Option<Option<ModelsStoreEntry>>,
    pub update: Option<Box<dyn FnOnce() + Send>>,
}

type PublishFn =
    Arc<dyn Fn(ModelsPublication) -> BoxFuture<'static, Result<bool, AbortReason>> + Send + Sync>;

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

    /// A `'static` handle to `publish`, for providers that publish from a future they do not own.
    pub fn publisher(&self) -> PublishFn {
        self.publish.clone()
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

pub type TransformHeaders =
    Arc<dyn Fn(ProviderHeaders) -> BoxFuture<'static, ProviderHeaders> + Send + Sync>;

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
    /// `Provider.auth`: the provider's api-key and OAuth auth methods.
    fn auth(&self) -> Option<&crate::auth::types::ProviderAuth> {
        None
    }
    /// Provider-owned retry policy; omission selects the shipped default profile.
    fn retry_policy(&self) -> Option<&crate::utils::retry_profile::types::RetryPolicyProfile> {
        None
    }
    fn get_models(&self) -> Vec<Model>;
    fn get_all_models(&self) -> Vec<AnyModel> {
        self.get_models().into_iter().map(AnyModel::Chat).collect()
    }
    fn supports_classification(&self) -> bool {
        false
    }
    fn classify<'a>(
        &'a self,
        model: &'a ClassifierModel,
        _context: &'a ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> BoxFuture<'a, ClassifierResult> {
        Box::pin(async move {
            let mut output = system_one_shared::result(model);
            system_one_shared::fail(
                &mut output,
                &options.unwrap_or_default(),
                format!("Provider {} does not support classification", self.id()),
            );
            output
        })
    }
    /// `get_models().find(id)` without materializing the whole catalog when the provider can avoid it.
    fn get_model(&self, id: &str) -> Option<Model> {
        self.get_models().into_iter().find(|model| model.id == id)
    }
    /// `get_models().some(api)` without materializing the whole catalog when the provider can avoid it.
    fn has_api(&self, api: &str) -> bool {
        self.get_models().iter().any(|model| model.api == api)
    }
    fn supports_refresh(&self) -> bool {
        false
    }
    fn refresh_models<'a>(
        &'a self,
        _context: RefreshModelsContext,
    ) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async { Ok(()) })
    }
    fn filter_models(&self, models: Vec<Model>, _credential: Option<&Credential>) -> Vec<Model> {
        models
    }
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream;
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream;
    fn generate_images<'a>(
        &'a self,
        _model: &'a crate::types::ImagesModel,
        _context: &'a crate::types::ImagesContext,
        _options: Option<crate::types::ImagesOptions>,
    ) -> BoxFuture<'a, Result<crate::types::AssistantImages, String>> {
        Box::pin(async { Err("Provider does not support image generation".to_owned()) })
    }
    fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: Option<crate::types::DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        None
    }
    /// `cancelDeferred(model, handle, options)`: the provider-level operation future. The default
    /// is the pinned unsupported error (`Provider.cancelDeferred` undefined), never a no-op
    /// success; `supports_cancel_deferred()` is the model-free capability probe
    /// (`streams.some((entry) => entry.cancelDeferred !== undefined)`).
    fn cancel_deferred<'a>(
        &'a self,
        _model: &'a Model,
        _handle: &'a DeferredHandle,
        _options: Option<crate::types::DeferredCancelOptions>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("API cannot cancel deferred responses".to_owned()) })
    }
    fn supports_cancel_deferred(&self) -> bool {
        false
    }
}

pub type FetchModels = Arc<
    dyn Fn(&RefreshModelsContext) -> BoxFuture<'static, Result<Vec<Model>, ModelsError>>
        + Send
        + Sync,
>;
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
    pub auth: Option<crate::auth::types::ProviderAuth>,
    pub models: Vec<Model>,
    pub fetch_models: Option<FetchModels>,
    pub restore_models: Option<RestoreModels>,
    pub filter_models: Option<FilterModels>,
    pub api: ProviderApi,
    pub images: IndexMap<String, Arc<dyn crate::types::ProviderImages>>,
}

struct CreatedProvider {
    input: CreateProviderOptions,
    name: String,
    dynamic_models: Arc<RwLock<Vec<Model>>>,
    retry_policy: Option<crate::utils::retry_profile::types::RetryPolicyProfile>,
    other_models: Vec<AnyModel>,
    classifiers: IndexMap<String, Arc<dyn ProviderClassifier>>,
}

impl CreatedProvider {
    fn api_for(&self, model: &Model) -> Option<&Arc<dyn ProviderStreams>> {
        match &self.input.api {
            ProviderApi::Single(streams) => Some(streams),
            ProviderApi::ByApi(by_api) => by_api.get(&model.api),
        }
    }

    fn no_api(&self, model: &Model) -> AssistantMessageEventStream {
        error_stream(
            model,
            &format!(
                "Provider {} has no API implementation for \"{}\"",
                self.input.id, model.api
            ),
        )
    }

    fn image_api(&self, api: &str) -> Option<&Arc<dyn crate::types::ProviderImages>> {
        self.input.images.get(api)
    }
}

impl Provider for CreatedProvider {
    fn get_all_models(&self) -> Vec<AnyModel> {
        self.get_models()
            .into_iter()
            .map(AnyModel::Chat)
            .chain(self.other_models.iter().cloned())
            .collect()
    }

    fn supports_classification(&self) -> bool {
        !self.classifiers.is_empty()
    }

    fn classify<'a>(
        &'a self,
        model: &'a ClassifierModel,
        context: &'a ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> BoxFuture<'a, ClassifierResult> {
        match self.classifiers.get(&model.api) {
            Some(classifier) => classifier.classify(model, context, options),
            None => Box::pin(async move {
                let mut output = system_one_shared::result(model);
                system_one_shared::fail(
                    &mut output,
                    &options.unwrap_or_default(),
                    format!(
                        "Provider {} has no classifier API implementation for \"{}\"",
                        self.id(),
                        model.api
                    ),
                );
                output
            }),
        }
    }

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

    fn auth(&self) -> Option<&crate::auth::types::ProviderAuth> {
        self.input.auth.as_ref()
    }

    fn retry_policy(&self) -> Option<&crate::utils::retry_profile::types::RetryPolicyProfile> {
        self.retry_policy.as_ref()
    }

    fn get_models(&self) -> Vec<Model> {
        let mut merged = self.input.models.clone();
        for model in self
            .dynamic_models
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
        {
            match merged.iter_mut().find(|entry| entry.id == model.id) {
                Some(slot) => *slot = model.clone(),
                None => merged.push(model.clone()),
            }
        }
        merged
    }

    fn get_model(&self, id: &str) -> Option<Model> {
        let dynamic = self
            .dynamic_models
            .read()
            .unwrap_or_else(|p| p.into_inner());
        // Later dynamic entries overwrite earlier ones in `get_models`, so the last dynamic match wins.
        if let Some(model) = dynamic.iter().rev().find(|model| model.id == id) {
            return Some(model.clone());
        }
        self.input
            .models
            .iter()
            .find(|model| model.id == id)
            .cloned()
    }

    fn has_api(&self, api: &str) -> bool {
        let dynamic = self
            .dynamic_models
            .read()
            .unwrap_or_else(|p| p.into_inner());
        // Mirrors `get_models`: a dynamic entry survives unless a later dynamic entry shares its id; a static
        // entry survives unless it is the first static entry with an id some dynamic entry overrides.
        dynamic.iter().enumerate().any(|(i, model)| {
            model.api == api && !dynamic[i + 1..].iter().any(|e| e.id == model.id)
        }) || self.input.models.iter().enumerate().any(|(i, model)| {
            model.api == api
                && (!dynamic.iter().any(|entry| entry.id == model.id)
                    || self.input.models[..i]
                        .iter()
                        .any(|prior| prior.id == model.id))
        })
    }

    fn supports_refresh(&self) -> bool {
        self.input.fetch_models.is_some()
    }

    fn refresh_models<'a>(
        &'a self,
        context: RefreshModelsContext,
    ) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async move {
            let Some(fetch_models) = self.input.fetch_models.clone() else {
                return Ok(());
            };
            if let Some(stored) = &context.stored {
                let mut restored: Vec<Model> = stored
                    .models
                    .iter()
                    .filter(|model| model.provider == self.input.id)
                    .cloned()
                    .collect();
                if let Some(restore) = &self.input.restore_models
                    && let Ok(models) = restore(restored.clone())
                {
                    restored = models;
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
            let entry = ModelsStoreEntry {
                models: refreshed.clone(),
                checked_at: Some(now_ms()),
                ..ModelsStoreEntry::default()
            };
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

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        match self.api_for(model) {
            Some(streams) => streams.stream(model, context, options),
            None => self.no_api(model),
        }
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        match self.api_for(model) {
            Some(streams) => streams.stream_simple(model, context, options),
            None => self.no_api(model),
        }
    }

    fn generate_images<'a>(
        &'a self,
        model: &'a crate::types::ImagesModel,
        context: &'a crate::types::ImagesContext,
        options: Option<crate::types::ImagesOptions>,
    ) -> BoxFuture<'a, Result<crate::types::AssistantImages, String>> {
        match self.image_api(&model.api) {
            Some(streams) => streams.generate_images(model, context, options),
            None => Box::pin(async move {
                Err(format!(
                    "Provider {} has no image API implementation for \"{}\"",
                    self.input.id, model.api
                ))
            }),
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
            return Some(error_stream(
                model,
                &format!(
                    "Provider {} does not support deferred responses",
                    self.input.id
                ),
            ));
        }
        streams.fetch_deferred(model, handle, options)
    }

    fn supports_cancel_deferred(&self) -> bool {
        match &self.input.api {
            ProviderApi::Single(streams) => streams.supports_cancel_deferred(),
            ProviderApi::ByApi(by_api) => by_api
                .values()
                .any(|streams| streams.supports_cancel_deferred()),
        }
    }

    fn cancel_deferred<'a>(
        &'a self,
        model: &'a Model,
        handle: &'a DeferredHandle,
        options: Option<crate::types::DeferredCancelOptions>,
    ) -> BoxFuture<'a, Result<(), String>> {
        let provider_id = self.input.id.clone();
        let api = model.api.clone();
        match self.api_for(model).cloned() {
            // Pinned `createProvider`: a model whose api has no entry yields
            // `Provider {id} cannot cancel deferred responses for "{api}"`.
            None => Box::pin(async move {
                Err(format!(
                    "Provider {provider_id} cannot cancel deferred responses for \"{api}\""
                ))
            }),
            Some(streams) => {
                Box::pin(async move { streams.cancel_deferred(model, handle, options).await })
            }
        }
    }
}

pub fn create_provider(input: CreateProviderOptions) -> Result<Arc<dyn Provider>, ModelsError> {
    create_provider_with_retry_policy(input, None)
}

/// Additive constructor preserving existing provider-option struct literals.
pub fn create_provider_with_retry_policy(
    input: CreateProviderOptions,
    retry_policy: Option<crate::utils::retry_profile::types::RetryPolicyProfile>,
) -> Result<Arc<dyn Provider>, ModelsError> {
    let other_models: Vec<_> = crate::models_generated::CLASSIFIER_MODELS
        .get(&input.id)
        .into_iter()
        .flat_map(|models| models.values().cloned().map(AnyModel::Classifier))
        .collect();
    let mut classifiers: IndexMap<String, Arc<dyn ProviderClassifier>> = IndexMap::new();
    for model in &other_models {
        if let AnyModel::Classifier(model) = model {
            classifiers.insert(model.api.clone(), Arc::new(BuiltinClassifier));
        }
    }
    create_provider_with_classifiers(input, retry_policy, other_models, classifiers)
}

pub fn create_provider_with_classifiers(
    input: CreateProviderOptions,
    retry_policy: Option<crate::utils::retry_profile::types::RetryPolicyProfile>,
    other_models: Vec<AnyModel>,
    classifiers: IndexMap<String, Arc<dyn ProviderClassifier>>,
) -> Result<Arc<dyn Provider>, ModelsError> {
    let has_chat = match &input.api {
        ProviderApi::Single(_) => true,
        ProviderApi::ByApi(entries) => !entries.is_empty(),
    };
    if !has_chat && input.images.is_empty() && classifiers.is_empty() {
        return Err(ModelsError::new(
            ModelsErrorCode::Provider,
            format!(
                "Provider {}: at least one of \"api\", \"images\", or \"classifiers\" is required.",
                input.id
            ),
        ));
    }
    let name = input.name.clone().unwrap_or_else(|| input.id.clone());
    Ok(Arc::new(CreatedProvider {
        input,
        name,
        dynamic_models: Arc::new(RwLock::new(Vec::new())),
        retry_policy,
        other_models,
        classifiers,
    }))
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
            models_store: options
                .models_store
                .unwrap_or_else(|| Arc::new(InMemoryModelsStore::new())),
            auth: options
                .auth
                .unwrap_or_else(|| Arc::new(ProviderModelsAuth::default())),
            refresh: Mutex::new(RefreshState::default()),
            publication_locks: Mutex::new(HashMap::new()),
        }),
    }
}

fn merge_headers(
    base: Option<&ProviderHeaders>,
    overrides: Option<&ProviderHeaders>,
) -> Option<ProviderHeaders> {
    if base.is_none() && overrides.is_none() {
        return None;
    }
    let mut merged = base.cloned().unwrap_or_default();
    for (name, value) in overrides.into_iter().flatten() {
        // PERF-DIVERGENCE(senpi models.ts): upstream lowercases every existing key per override; this port compares ASCII names case-insensitively without allocating and keeps Unicode lowercasing only for non-ASCII names. The merged map is identical.
        let lower = if name.is_ascii() {
            None
        } else {
            Some(name.to_lowercase())
        };
        merged.retain(|existing, _| match &lower {
            None if existing.is_ascii() => !existing.eq_ignore_ascii_case(name),
            None => existing.to_lowercase() != name.to_lowercase(),
            Some(lower) => existing.to_lowercase() != *lower,
        });
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
        self.inner
            .providers
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(provider.id().to_owned(), provider);
    }

    pub fn delete_provider(&self, id: &str) {
        self.supersede_provider_refresh(id);
        self.inner
            .providers
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .shift_remove(id);
    }

    pub fn clear_providers(&self) {
        let mut ids: Vec<String> = self
            .inner
            .providers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        ids.extend(self.refresh_state().controllers.keys().cloned());
        for id in ids {
            self.supersede_provider_refresh(&id);
        }
        self.inner
            .providers
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    pub fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
        self.inner
            .providers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.inner
            .providers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned()
    }

    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        match provider {
            Some(id) => self
                .get_provider(id)
                .map(|p| p.get_models())
                .unwrap_or_default(),
            None => self
                .get_providers()
                .iter()
                .flat_map(|p| p.get_models())
                .collect(),
        }
    }

    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.get_provider(provider).and_then(|p| p.get_model(id))
    }

    pub fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
        match provider {
            Some(id) => self
                .get_provider(id)
                .map(|p| p.get_all_models())
                .unwrap_or_default(),
            None => self
                .get_providers()
                .iter()
                .flat_map(|p| p.get_all_models())
                .collect(),
        }
    }

    pub fn get_classifier_models(&self, provider: Option<&str>) -> Vec<ClassifierModel> {
        self.get_all_models(provider)
            .into_iter()
            .filter_map(|model| match model {
                AnyModel::Classifier(model) => Some(model),
                AnyModel::Chat(_) | AnyModel::Image(_) => None,
            })
            .collect()
    }

    pub fn get_classifier_model(&self, provider: &str, id: &str) -> Option<ClassifierModel> {
        self.get_classifier_models(Some(provider))
            .into_iter()
            .find(|model| model.id == id)
    }

    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ModelsClassifierOptions>,
    ) -> ClassifierResult {
        let mut options = options.unwrap_or_default();
        let operation = async {
            let provider = self
                .get_provider(&model.provider)
                .ok_or_else(|| format!("Unknown provider: {}", model.provider))?;
            if !provider.supports_classification() {
                return Err(format!(
                    "Provider {} does not support classification",
                    model.provider
                ));
            }
            let (_, request_model, request_options) = self
                .apply_auth(
                    &system_one_shared::request_model(model),
                    &options.options.request,
                    &options.transforms,
                )
                .await
                .map_err(|e| e.message)?;
            let mut model = model.clone();
            model.base_url = request_model.base_url;
            options.options.request = request_options;
            Ok::<_, String>(
                provider
                    .classify(&model, context, Some(options.options.clone()))
                    .await,
            )
        }
        .await;
        match operation {
            Ok(result) => result,
            Err(error) => {
                let mut result = system_one_shared::result(model);
                system_one_shared::fail(&mut result, &options.options, error);
                result
            }
        }
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
        self.refresh_state()
            .controllers
            .insert(provider_id.to_owned(), (generation, controller.clone()));
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
                    let options = ModelsStoreOperationOptions {
                        signal: Some(signal.clone()),
                    };
                    match publication.persist {
                        Some(None) => {
                            models
                                .inner
                                .models_store
                                .delete(&provider_id, Some(&options))
                                .await?
                        }
                        Some(Some(entry)) => {
                            models
                                .inner
                                .models_store
                                .write(&provider_id, &entry, Some(&options))
                                .await?
                        }
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
        let options = ModelsStoreOperationOptions {
            signal: Some(signal.clone()),
        };
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

    async fn refresh_one(
        &self,
        provider: Arc<dyn Provider>,
        options: &ModelsRefreshOptions,
        caller: &AbortSignal,
    ) -> Option<ModelsError> {
        let (generation, controller) = self.begin_provider_refresh(provider.id());
        let combined = combine_abort_signals(&[Some(caller.clone()), Some(controller.signal())]);
        let signal = combined
            .signal
            .clone()
            .unwrap_or_else(|| controller.signal());
        let allow_network = options.allow_network.unwrap_or(true);
        let operation = async {
            let (stored, credential_error) = match self
                .inner
                .auth
                .read_credential(provider.id(), &signal)
                .await
            {
                Ok(credential) => (credential, None),
                Err(error) => (None, Some(error)),
            };
            self.run_refresh_phase(&provider, stored.clone(), false, None, generation, &signal)
                .await?;
            if let Some(error) = credential_error {
                return Err(error);
            }
            if !allow_network || signal.aborted() {
                return Ok(());
            }
            let Some(credential) = self
                .inner
                .auth
                .refresh_credential(provider.as_ref(), stored.as_ref(), &signal)
                .await?
            else {
                return Ok(());
            };
            self.run_refresh_phase(
                &provider,
                Some(credential),
                true,
                options.force,
                generation,
                &signal,
            )
            .await
        };
        let outcome = race_with_abort_signal(operation, &signal).await;
        combined.cleanup();
        {
            let mut state = self.refresh_state();
            if state
                .controllers
                .get(provider.id())
                .is_some_and(|(g, _)| *g == generation)
            {
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
            .filter(|p| {
                p.supports_refresh()
                    && options
                        .providers
                        .as_ref()
                        .is_none_or(|ids| ids.iter().any(|id| id == p.id()))
            })
            .collect();
        let runs = refreshable.into_iter().map(|provider| {
            let id = provider.id().to_owned();
            let options = &options;
            let caller = &caller;
            async move { (id, self.refresh_one(provider, options, caller).await) }
        });
        if let Ok(outcomes) = race_with_abort_signal(futures::future::join_all(runs), &caller).await
        {
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
    pub async fn get_available(
        &self,
        provider_id: Option<&str>,
        signal: Option<AbortSignal>,
    ) -> Result<Vec<Model>, ModelsError> {
        let providers: Vec<Arc<dyn Provider>> = match provider_id {
            Some(id) => self.get_provider(id).into_iter().collect(),
            None => self.get_providers(),
        };
        let signal = operation_signal(signal);
        let available = async {
            let checks = providers.iter().map(|provider| {
                let signal = &signal;
                async move {
                    let credential = self
                        .inner
                        .auth
                        .read_credential(provider.id(), signal)
                        .await?;
                    let auth = self.inner.auth.check(provider.as_ref(), signal).await?;
                    Ok::<_, ModelsError>((provider, credential, auth))
                }
            });
            let authenticated = futures::future::try_join_all(checks).await?;
            Ok(authenticated
                .into_iter()
                .filter(|(_, _, auth)| auth.is_some())
                .flat_map(|(provider, credential, _)| {
                    provider.filter_models(provider.get_models(), credential.as_ref())
                })
                .collect())
        };
        race_with_abort_signal(available, &signal)
            .await
            .map_err(|reason| ModelsError::new(ModelsErrorCode::Auth, reason.message))?
    }

    /// `checkAuth(providerId, options)`: side-effect-free availability of the provider's auth.
    pub async fn check_auth(
        &self,
        provider_id: &str,
        signal: &AbortSignal,
    ) -> Result<Option<crate::auth::types::AuthCheck>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else {
            return Ok(None);
        };
        match race_with_abort_signal(self.inner.auth.check(provider.as_ref(), signal), signal).await
        {
            Ok(result) => result,
            Err(reason) => Err(ModelsError::new(ModelsErrorCode::Auth, reason.message)),
        }
    }

    pub async fn get_auth(
        &self,
        provider_id: &str,
        overrides: &AuthResolutionOverrides,
    ) -> Result<Option<AuthResolution>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else {
            return Ok(None);
        };
        let signal = operation_signal(overrides.signal.clone());
        match race_with_abort_signal(
            self.inner.auth.resolve(provider.as_ref(), overrides),
            &signal,
        )
        .await
        {
            Ok(result) => result,
            Err(reason) => Err(ModelsError::new(ModelsErrorCode::Auth, reason.message)),
        }
    }

    /// `getAuth(model | providerId, overrides)`'s model overload: the resolved auth headers are
    /// merged under the model's own headers (`mergeHeaders(result.auth.headers, model.headers)`).
    pub async fn get_auth_for_model(
        &self,
        model: &Model,
        overrides: &AuthResolutionOverrides,
    ) -> Result<Option<AuthResolution>, ModelsError> {
        let Some(mut resolution) = self.get_auth(&model.provider, overrides).await? else {
            return Ok(None);
        };
        if let Some(model_headers) = &model.headers {
            let model_headers: ProviderHeaders = model_headers
                .iter()
                .map(|(name, value)| (name.clone(), Some(value.clone())))
                .collect();
            resolution.auth.headers =
                merge_headers(resolution.auth.headers.as_ref(), Some(&model_headers));
        }
        Ok(Some(resolution))
    }

    fn require_provider(&self, model: &Model) -> Result<Arc<dyn Provider>, ModelsError> {
        self.get_provider(&model.provider).ok_or_else(|| {
            ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {}", model.provider),
            )
        })
    }

    pub async fn login(
        &self,
        provider_id: &str,
        auth_type: crate::auth::types::AuthType,
        interaction: Arc<dyn crate::auth::types::AuthInteraction>,
        options: Option<ModelsLoginOptions>,
    ) -> Result<crate::auth::types::Credential, ModelsError> {
        let provider = self.get_provider(provider_id).ok_or_else(|| {
            ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
            )
        })?;
        self.inner
            .auth
            .login(
                provider.as_ref(),
                auth_type,
                interaction,
                options.unwrap_or_default(),
            )
            .await
    }

    pub async fn logout(
        &self,
        provider_id: &str,
        options: Option<ModelsLogoutOptions>,
    ) -> Result<(), ModelsError> {
        self.inner
            .auth
            .logout(provider_id, options.unwrap_or_default())
            .await
    }

    async fn apply_auth(
        &self,
        model: &Model,
        request: &crate::types::ProviderRequestOptions,
        transforms: &ModelsRequestTransforms,
    ) -> Result<
        (
            Arc<dyn Provider>,
            Model,
            crate::types::ProviderRequestOptions,
        ),
        ModelsError,
    > {
        let (provider, base_url, options) = self
            .apply_request_auth(&model.provider, model.headers.as_ref(), request, transforms)
            .await?;
        let mut request_model = model.clone();
        if let Some(base_url) = base_url {
            request_model.base_url = base_url;
        }
        Ok((provider, request_model, options))
    }

    async fn apply_request_auth(
        &self,
        provider_id: &str,
        model_headers: Option<&std::collections::BTreeMap<String, String>>,
        request: &crate::types::ProviderRequestOptions,
        transforms: &ModelsRequestTransforms,
    ) -> Result<
        (
            Arc<dyn Provider>,
            Option<String>,
            crate::types::ProviderRequestOptions,
        ),
        ModelsError,
    > {
        let provider = self.get_provider(provider_id).ok_or_else(|| {
            ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
            )
        })?;
        let overrides = AuthResolutionOverrides {
            api_key: request.api_key.clone(),
            env: request.env.clone(),
            signal: request.signal.clone(),
            ..AuthResolutionOverrides::default()
        };
        let mut resolution = self
            .get_auth(provider_id, &overrides)
            .await?
            .ok_or_else(|| {
                ModelsError::new(
                    ModelsErrorCode::Auth,
                    provider_not_configured_message(provider_id),
                )
            })?;
        if let Some(model_headers) = model_headers {
            let model_headers = model_headers
                .iter()
                .map(|(name, value)| (name.clone(), Some(value.clone())))
                .collect();
            resolution.auth.headers =
                merge_headers(resolution.auth.headers.as_ref(), Some(&model_headers));
        }
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
        let mut options = request.clone();
        options.api_key = request.api_key.clone().or(resolution.auth.api_key);
        options.headers = headers;
        options.env = env;
        Ok((provider, resolution.auth.base_url, options))
    }

    fn lazy(
        &self,
        model: &Model,
        setup: impl std::future::Future<Output = Result<AssistantMessageEventStream, ModelsError>>
        + Send
        + 'static,
    ) -> AssistantMessageEventStream {
        crate::api::lazy::lazy_stream_typed(model, async move {
            setup.await.map_err(|error| {
                if error.code == ModelsErrorCode::OAuth {
                    crate::utils::event_stream::StreamError::oauth_refresh_unavailable(
                        error.message,
                    )
                } else {
                    crate::utils::event_stream::StreamError::new(error.message)
                }
            })
        })
        .into_stream()
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
            let (provider, request_model, request) = models
                .apply_auth(&model_owned, &options.request, &transforms)
                .await?;
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
        let stream = self.stream(model, context, options, transforms);
        // Pop events so their partial-message clones are freed instead of queued until drop.
        while let Ok(Some(_)) = stream.next().await {}
        stream.result().await
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
            let (provider, request_model, request) = models
                .apply_auth(&model_owned, &options.stream.request, &transforms)
                .await?;
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
        let stream = self.stream_simple(model, context, options, transforms);
        // Pop events so their partial-message clones are freed instead of queued until drop.
        while let Ok(Some(_)) = stream.next().await {}
        stream.result().await
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
            let (provider, request_model, request) = models
                .apply_auth(&model_owned, &options.request, &transforms)
                .await?;
            options.request = request;
            provider
                .fetch_deferred(&request_model, &handle, Some(options))
                .ok_or_else(|| {
                    ModelsError::new(
                        ModelsErrorCode::Provider,
                        format!(
                            "Provider {} does not support deferred responses",
                            model_owned.provider
                        ),
                    )
                })
        })
    }

    pub async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<crate::types::DeferredFetchOptions>,
        transforms: ModelsRequestTransforms,
    ) -> Result<AssistantMessage, crate::utils::event_stream::StreamError> {
        let stream = self.stream_deferred(model, handle, options, transforms);
        while let Ok(Some(_)) = stream.next().await {}
        stream.result().await
    }

    pub async fn generate_images(
        &self,
        model: &crate::types::ImagesModel,
        context: &crate::types::ImagesContext,
        options: Option<crate::types::ImagesOptions>,
        transforms: ModelsRequestTransforms,
    ) -> crate::types::AssistantImages {
        let options = options.unwrap_or_default();
        let result = async {
            let (provider, base_url, request) = self
                .apply_request_auth(
                    &model.provider,
                    model.headers.as_ref(),
                    &options.request,
                    &transforms,
                )
                .await
                .map_err(|error| error.message)?;
            let mut request_model = model.clone();
            if let Some(base_url) = base_url {
                request_model.base_url = base_url;
            }
            let mut request_options = options.clone();
            request_options.request = request;
            provider
                .generate_images(&request_model, context, Some(request_options))
                .await
        }
        .await;
        match result {
            Ok(images) => images,
            Err(error) => crate::types::AssistantImages {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                output: Vec::new(),
                response_id: None,
                usage: None,
                background: None,
                stop_reason: if options
                    .request
                    .signal
                    .as_ref()
                    .is_some_and(AbortSignal::aborted)
                {
                    crate::types::ImagesStopReason::Aborted
                } else {
                    crate::types::ImagesStopReason::Error
                },
                error_message: Some(error),
                timestamp: now_ms(),
                provider_diagnostic: None,
            },
        }
    }

    /// Pinned `Models.cancelDeferred(model, handle, options)`: resolve the provider, throw
    /// `Provider {id} does not support deferred responses` when it exposes no cancellation, then
    /// resolve auth (`applyAuth`) and forward the request model/options to the provider. The error
    /// is the pinned message string; `cancelDeferredBestEffort` swallows it.
    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<crate::types::DeferredCancelOptions>,
    ) -> Result<(), String> {
        self.cancel_deferred_with_transforms(
            model,
            handle,
            options,
            ModelsRequestTransforms::default(),
        )
        .await
    }

    /// Cancellation with the same caller-owned authentication transforms as streaming/fetching.
    pub async fn cancel_deferred_with_transforms(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<crate::types::DeferredCancelOptions>,
        transforms: ModelsRequestTransforms,
    ) -> Result<(), String> {
        let provider = self
            .require_provider(model)
            .map_err(|error| error.message)?;
        if !provider.supports_cancel_deferred() {
            return Err(format!(
                "Provider {} does not support deferred responses",
                model.provider
            ));
        }
        let request = options.unwrap_or_default();
        let (provider, request_model, request_options) = self
            .apply_auth(model, &request, &transforms)
            .await
            .map_err(|error| error.message)?;
        provider
            .cancel_deferred(&request_model, handle, Some(request_options))
            .await
    }
}

pub fn has_api(model: &Model, api: &str) -> bool {
    model.api == api
}

#[path = "model_cost.rs"]
pub mod model_cost;

/// Fills `usage.cost` using the cost card in force now.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> UsageCost {
    calculate_cost_at(model, usage, now_ms())
}

/// Fills `usage.cost` using the cost card in force at a supplied Unix millisecond timestamp.
pub fn calculate_cost_at(model: &Model, usage: &mut Usage, at_ms: i64) -> UsageCost {
    if model.cost.unknown == Some(true) {
        usage.cost.input = 0.0;
        usage.cost.output = 0.0;
        usage.cost.cache_read = 0.0;
        usage.cost.cache_write = 0.0;
        usage.cost.total = 0.0;
        usage.cost_unknown = Some(true);
        return usage.cost;
    }
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let card = model_cost::cost_card_at(&model.cost, at_ms);
    let mut rates = ModelCostRates {
        input: card.input,
        output: card.output,
        cache_read: card.cache_read,
        cache_write: card.cache_write,
    };
    let mut matched_threshold: i128 = -1;
    for tier in card.tiers.iter().flatten() {
        let above = i128::from(tier.input_tokens_above);
        if i128::from(input_tokens) > above && above > matched_threshold {
            rates = ModelCostRates {
                input: tier.input,
                output: tier.output,
                cache_read: tier.cache_read,
                cache_write: tier.cache_write,
            };
            matched_threshold = above;
        }
    }
    let long_write = usage.cache_write_1h.unwrap_or(0) as f64;
    let short_write = usage.cache_write as f64 - long_write;
    usage.cost.input = (rates.input / 1_000_000.0) * usage.input as f64;
    usage.cost.output = (rates.output / 1_000_000.0) * usage.output as f64;
    usage.cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    usage.cost.cache_write =
        (rates.cache_write * short_write + rates.input * 2.0 * long_write) / 1_000_000.0;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost
}

#[path = "google_thinking_generation.rs"]
pub mod google_thinking_generation;

const OPENAI_THINKING_APIS: &[&str] = &[
    "openai-completions",
    "openai-responses",
    "azure-openai-responses",
    "openai-codex-responses",
];

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
    if OPENAI_THINKING_APIS.contains(&model.api.as_str())
        && ["gpt-6-astra", "gpt-6.1-sol"]
            .iter()
            .any(|id| matches_model_family(&model.id, id))
    {
        return Some(gpt_6_astra_thinking_level_map());
    }
    None
}

/// The row's explicit map, or documented provider controls when no map is supplied.
pub fn infer_thinking_level_map(model: &Model) -> Option<ThinkingLevelMap> {
    infer_openai_thinking_level_map(model)
        .or_else(|| google_thinking_generation::infer_google_thinking_level_map(model))
}

/// `map?.[level]`: `None` = key absent, `Some(None)` = explicit null.
fn mapped_level(model: &Model, level: ModelThinkingLevel) -> Option<Option<String>> {
    infer_thinking_level_map(model).and_then(|map| map.get(&level).cloned())
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
        .unwrap_or_else(|| {
            available
                .first()
                .copied()
                .unwrap_or(ModelThinkingLevel::Off)
        })
}

const XHIGH_MODEL_IDS: &[&str] = &[
    "gpt-5.2",
    "gpt-5.3",
    "gpt-5.4",
    "gpt-5.5",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-6-astra",
    "gpt-6.1-sol",
    "gpt-6-sol",
    "gpt-6-luna",
    "deepseek-v4-pro",
    "deepseek-v4-flash",
    "opus-4-6",
    "opus-4.6",
    "opus-4-7",
    "opus-4.7",
    "opus-4-8",
    "opus-4.8",
    "opus-5",
    "sonnet-5",
    "fable-5",
];

pub fn supports_xhigh(model: &Model) -> bool {
    match infer_openai_thinking_level_map(model)
        .and_then(|map| map.get(&ModelThinkingLevel::Xhigh).cloned())
    {
        Some(None) => false,
        Some(Some(_)) => true,
        None if model.thinking_level_map.is_some() => false,
        None => {
            model.reasoning
                && XHIGH_MODEL_IDS
                    .iter()
                    .any(|id| matches_model_family(&model.id, id))
        }
    }
}

/// Family match at a token boundary: `gpt-*` families do not match after a `-` (so `gpt-5.5` does
/// not match `chatgpt-5.5`), and the family must end at a non-alphanumeric character.
fn matches_model_family(model_id: &str, family: &str) -> bool {
    let allow_dash = !family.starts_with("gpt-");
    let family = family.to_lowercase();
    let id = model_id.to_lowercase();
    id.char_indices().any(|(start, _)| {
        id[start..].starts_with(family.as_str())
            && id[..start]
                .chars()
                .next_back()
                .is_none_or(|c| matches!(c, '/' | '.' | ':' | '_') || (allow_dash && c == '-'))
            && id[start + family.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit())
    })
}

const CONFIGURATION_UPDATE_APIS: &[&str] = &[
    "openai-responses",
    "openai-codex-responses",
    "azure-openai-responses",
];

/// Only explicit compatibility metadata opts a Responses model into configuration updates.
pub fn supports_configuration_update(model: &Model) -> bool {
    CONFIGURATION_UPDATE_APIS.contains(&model.api.as_str())
        && model
            .compat
            .as_ref()
            .and_then(|compat| compat.0.get("supportsConfigurationUpdate"))
            == Some(&serde_json::Value::Bool(true))
}

const OPENAI_MAX_APIS: &[&str] = &[
    "openai-responses",
    "azure-openai-responses",
    "openai-codex-responses",
    "openai-completions",
];
const OPENAI_MAX_MODEL_IDS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-6-astra",
    "gpt-6.1-sol",
    "gpt-6-sol",
    "gpt-6-luna",
];
const MAX_MODEL_IDS: &[&str] = &[
    "opus-4-6", "opus-4.6", "opus-4-7", "opus-4.7", "opus-4-8", "opus-4.8", "opus-5", "sonnet-5",
    "fable-5",
];

pub fn supports_max(model: &Model) -> bool {
    match infer_openai_thinking_level_map(model)
        .and_then(|map| map.get(&ModelThinkingLevel::Max).cloned())
    {
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
    if crate::model_catalog::builtin_catalog_advertises_max(&model.id) {
        return true;
    }
    if OPENAI_MAX_APIS.contains(&model.api.as_str())
        && OPENAI_MAX_MODEL_IDS
            .iter()
            .any(|id| matches_model_family(&model.id, id))
    {
        return true;
    }
    MAX_MODEL_IDS
        .iter()
        .any(|id| matches_model_family(&model.id, id))
}

pub fn models_are_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "models/audit_core_tests.rs"]
mod audit_core_tests;

#[cfg(test)]
mod models_api_audit_tests {
    use super::*;
    use crate::types::{ImagesContext, ImagesModel, ImagesOptions, ImagesStopReason};

    struct LoginInteraction {
        receipts: Mutex<Vec<crate::auth::types::AccountLoginReceipt>>,
    }

    #[async_trait::async_trait]
    impl crate::auth::types::AuthInteraction for LoginInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            None
        }
        async fn prompt(&self, _: crate::auth::types::AuthPrompt) -> anyhow::Result<String> {
            Ok("second-key".into())
        }
        fn notify(&self, _: crate::auth::types::AuthEvent) {}
        fn on_account_committed(&self, receipt: crate::auth::types::AccountLoginReceipt) {
            self.receipts.lock().expect("receipts").push(receipt);
        }
    }

    async fn pooled_auth() -> (
        Models,
        Arc<dyn crate::auth::types::CredentialStore>,
        Arc<LoginInteraction>,
    ) {
        use crate::auth::types::{ApiKeyAuth, CredentialStore};
        let store: Arc<dyn CredentialStore> =
            Arc::new(crate::auth::credential_store::InMemoryCredentialStore::new());
        let credential = serde_json::from_value(serde_json::json!({
            "type": "api_key", "key": "first-key", "pinned": "default", "order": ["default"],
            "providerField": "retained"
        }))
        .expect("credential");
        store
            .modify(
                "pool",
                Box::new(move |_| Box::pin(async move { Ok(Some(credential)) })),
                None,
            )
            .await
            .expect("seed");
        let auth = ProviderModelsAuth::new(
            store.clone(),
            Arc::new(crate::auth::context::default_provider_auth_context()),
        );
        let models = create_models(Some(CreateModelsOptions {
            auth: Some(Arc::new(auth)),
            models_store: None,
        }));
        let api_key: Arc<dyn ApiKeyAuth> =
            Arc::new(crate::auth::helpers::env_api_key_auth("test key", &[]));
        let classifier: Arc<dyn ProviderClassifier> = Arc::new(BuiltinClassifier);
        let provider = create_provider_with_classifiers(
            CreateProviderOptions {
                id: "pool".into(),
                name: None,
                base_url: None,
                headers: None,
                auth: Some(crate::auth::types::ProviderAuth {
                    api_key: Some(api_key),
                    oauth: None,
                }),
                models: Vec::new(),
                fetch_models: None,
                restore_models: None,
                filter_models: None,
                api: ProviderApi::ByApi(IndexMap::new()),
                images: IndexMap::new(),
            },
            None,
            Vec::new(),
            [("test".into(), classifier)].into_iter().collect(),
        )
        .expect("provider");
        models.set_provider(provider);
        (
            models,
            store,
            Arc::new(LoginInteraction {
                receipts: Mutex::new(Vec::new()),
            }),
        )
    }

    #[tokio::test]
    async fn login_preserves_siblings_and_metadata_before_reporting_commit() {
        // Given a stored credential with pool metadata.
        let (models, store, interaction) = pooled_auth().await;
        // When the provider logs in a second account.
        models
            .login(
                "pool",
                crate::auth::types::AuthType::ApiKey,
                interaction.clone(),
                None,
            )
            .await
            .expect("login");
        // Then the serialized pool and committed receipt retain both accounts.
        let stored = serde_json::to_value(
            store
                .read("pool", None)
                .await
                .expect("read")
                .expect("stored"),
        )
        .expect("json");
        assert_eq!(stored["accounts"][0]["key"], "first-key");
        assert_eq!(stored["accounts"][1]["key"], "second-key");
        assert_eq!(stored["pinned"], "default");
        assert_eq!(stored["order"], serde_json::json!(["default"]));
        assert_eq!(stored["providerField"], "retained");
        assert_eq!(
            interaction.receipts.lock().expect("receipts")[0].name,
            "login-2"
        );
    }

    #[tokio::test]
    async fn slot_logout_reprojects_survivor_and_deletes_last_account() {
        // Given two accounts in the stored pool.
        let (models, store, interaction) = pooled_auth().await;
        models
            .login(
                "pool",
                crate::auth::types::AuthType::ApiKey,
                interaction,
                None,
            )
            .await
            .expect("login");
        // When each account is removed through slot-scoped logout.
        models
            .logout(
                "pool",
                Some(ModelsLogoutOptions {
                    slot_id: Some("default".into()),
                    ..Default::default()
                }),
            )
            .await
            .expect("logout");
        // Then the survivor is projected without discarding provider metadata.
        let stored = serde_json::to_value(
            store
                .read("pool", None)
                .await
                .expect("read")
                .expect("stored"),
        )
        .expect("json");
        assert_eq!(stored["key"], "second-key");
        assert_eq!(stored["providerField"], "retained");
        assert!(stored.get("pinned").is_none());
        models
            .logout(
                "pool",
                Some(ModelsLogoutOptions {
                    slot_id: Some("login-2".into()),
                    ..Default::default()
                }),
            )
            .await
            .expect("last logout");
        assert!(store.read("pool", None).await.expect("read").is_none());
    }

    fn relay_model() -> Model {
        crate::models_generated::get_builtin_model("anthropic", "claude-fable-5")
            .expect("model")
            .clone()
    }

    #[tokio::test]
    async fn wrapper_preserves_oauth_setup_diagnostic() {
        // Given a typed OAuth setup failure.
        let models = create_models(None);
        let stream = models.lazy(&relay_model(), async {
            Err(ModelsError::new(
                ModelsErrorCode::OAuth,
                "refresh unavailable",
            ))
        });
        // When the wrapper publishes its terminal error message.
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("result deadline")
            .expect("message");
        // Then the machine-consumed OAuth diagnostic remains attached.
        assert_eq!(
            result.diagnostics.expect("diagnostics")[0].kind,
            crate::utils::retry::OAUTH_REFRESH_UNAVAILABLE_DIAGNOSTIC
        );
        stream.cancel().await.expect("cleanup");
    }

    #[tokio::test]
    async fn wrapper_cancellation_retains_partial_when_upstream_has_no_result() {
        // Given a relayed partial and a source whose cleanup supplies no result.
        let models = create_models(None);
        let model = relay_model();
        let source = AssistantMessageEventStream::assistant();
        source.set_cancellation_handler(Box::new(|| Box::pin(async { Ok(()) })));
        let partial = crate::utils::lazy::setup_error_message(&model, "partial");
        source.push(crate::types::AssistantMessageEvent::Start {
            partial: partial.clone(),
        });
        let stream = models.lazy(&model, async move { Ok(source) });
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("event deadline")
            .expect("event result")
            .expect("start");
        // When cancellation interrupts forwarding.
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.cancel())
            .await
            .expect("cancel deadline")
            .expect("cancel");
        // Then the last partial is settled instead of waiting forever for a source result.
        assert_eq!(stream.result_if_ready(), Some(Ok(partial)));
    }

    #[tokio::test]
    async fn wrapper_cancellation_drops_pending_setup_before_returning() {
        // Given setup holding a task-owned resource and waiting indefinitely.
        let models = create_models(None);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (resource_tx, mut resource_rx) = tokio::sync::oneshot::channel::<()>();
        let stream = models.lazy(&relay_model(), async move {
            let _resource = resource_tx;
            entered_tx.send(()).expect("entered subscriber");
            std::future::pending().await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
            .await
            .expect("setup deadline")
            .expect("entered");
        // When explicit cancellation is acknowledged.
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.cancel())
            .await
            .expect("cancel deadline")
            .expect("cancel");
        // Then setup has been dropped, not detached.
        assert!(matches!(
            resource_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn wrapper_cancellation_awaits_upstream_after_terminal_race() {
        for failed in [false, true] {
            // Given an upstream that terminates before gated cleanup completes.
            let models = create_models(None);
            let model = relay_model();
            let source = AssistantMessageEventStream::assistant();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            source.set_cancellation_handler(Box::new(move || {
                Box::pin(async move {
                    entered_tx.send(()).expect("cleanup subscriber");
                    release_rx.await.expect("cleanup release");
                    Ok(())
                })
            }));
            let message = crate::utils::lazy::setup_error_message(&model, "terminal");
            if failed {
                source.fail(crate::utils::event_stream::StreamError::new(
                    "terminal failure",
                ));
            } else {
                source.end(Some(message));
            }
            let stream = models.lazy(&model, async move { Ok(source) });
            let before = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
                .await
                .expect("terminal deadline");
            // When cancellation races with a relay that may already have exited.
            let cancellation = stream.cancel();
            tokio::pin!(cancellation);
            assert!(futures::poll!(cancellation.as_mut()).is_pending());
            tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
                .await
                .expect("cleanup deadline")
                .expect("entered");
            assert!(futures::poll!(cancellation.as_mut()).is_pending());
            release_tx.send(()).expect("release subscriber");
            tokio::time::timeout(std::time::Duration::from_secs(5), cancellation)
                .await
                .expect("cancel deadline")
                .expect("cancel");
            // Then upstream cleanup is joined without changing the terminal result.
            assert_eq!(stream.result_if_ready(), Some(before));
        }
    }

    struct ImageProvider;

    impl Provider for ImageProvider {
        fn id(&self) -> &str {
            "audit-images"
        }
        fn name(&self) -> &str {
            self.id()
        }
        fn get_models(&self) -> Vec<Model> {
            Vec::new()
        }
        fn stream(
            &self,
            _: &Model,
            _: &Context,
            _: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            panic!("image request must not stream chat")
        }
        fn stream_simple(
            &self,
            _: &Model,
            _: &Context,
            _: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream {
            panic!("image request must not stream chat")
        }
        fn generate_images<'a>(
            &'a self,
            model: &'a ImagesModel,
            _: &'a ImagesContext,
            options: Option<ImagesOptions>,
        ) -> BoxFuture<'a, Result<crate::types::AssistantImages, String>> {
            Box::pin(async move {
                let options = options.expect("resolved request");
                assert_eq!(model.base_url, "https://auth.example/v1");
                assert_eq!(options.request.api_key.as_deref(), Some("explicit"));
                let headers = options.request.headers.expect("headers");
                assert_eq!(headers.get("x-key"), Some(&Some("request".into())));
                assert_eq!(headers.get("model"), Some(&Some("model".into())));
                assert_eq!(headers.get("transformed"), Some(&Some("yes".into())));
                assert_eq!(
                    options.request.env.expect("env").get("origin"),
                    Some(&"request".into())
                );
                assert_eq!(options.extra.get("n"), Some(&Value::from(2)));
                Err("transport failed".into())
            })
        }
    }

    struct AuditAuth {
        failure: Option<&'static str>,
        entered: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    }

    impl ModelsAuth for AuditAuth {
        fn resolve<'a>(
            &'a self,
            _: &'a dyn Provider,
            _: &'a AuthResolutionOverrides,
        ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
            Box::pin(async {
                Ok(Some(AuthResolution {
                    ambient: false,
                    auth: ProviderAuthResult {
                        api_key: Some("auth".into()),
                        base_url: Some("https://auth.example/v1".into()),
                        headers: Some(
                            [("X-Key".into(), Some("auth".into()))]
                                .into_iter()
                                .collect(),
                        ),
                    },
                    env: Some([("origin".into(), "auth".into())].into_iter().collect()),
                }))
            })
        }
        fn refresh_credential<'a>(
            &'a self,
            _: &'a dyn Provider,
            _: Option<&'a Credential>,
            _: &'a AbortSignal,
        ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
            Box::pin(async { Ok(None) })
        }
        fn read_credential<'a>(
            &'a self,
            _: &'a str,
            _: &'a AbortSignal,
        ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
            Box::pin(async move {
                if let Some(entered) = &self.entered {
                    entered.send(()).expect("subscriber");
                    return std::future::pending().await;
                }
                if self.failure == Some("read") {
                    return Err(ModelsError::new(ModelsErrorCode::Auth, "read"));
                }
                Ok(None)
            })
        }
        fn check<'a>(
            &'a self,
            _: &'a dyn Provider,
            _: &'a AbortSignal,
        ) -> BoxFuture<'a, Result<Option<crate::auth::types::AuthCheck>, ModelsError>> {
            Box::pin(async move {
                if self.failure == Some("check") {
                    return Err(ModelsError::new(ModelsErrorCode::OAuth, "check"));
                }
                Ok(Some(crate::auth::types::AuthCheck {
                    ambient: false,
                    source: None,
                    auth_type: crate::auth::types::AuthType::ApiKey,
                }))
            })
        }
    }

    fn collection(auth: AuditAuth) -> Models {
        let models = create_models(Some(CreateModelsOptions {
            auth: Some(Arc::new(auth)),
            models_store: None,
        }));
        models.set_provider(Arc::new(ImageProvider));
        models
    }

    fn image_model() -> ImagesModel {
        let mut model = crate::image_models::get_image_model("openai", "gpt-image-2.5-flare")
            .expect("image model")
            .clone();
        model.provider = "audit-images".into();
        model.headers = Some([("model".into(), "model".into())].into_iter().collect());
        model
    }

    #[tokio::test]
    async fn image_requests_apply_shared_auth_and_normalize_transport_failure() {
        // Given distinct auth, model, request and transformed fields.
        let models = collection(AuditAuth {
            failure: None,
            entered: None,
        });
        let model = image_model();
        let mut options = ImagesOptions::default();
        options.request.api_key = Some("explicit".into());
        options.request.headers = Some(
            [("x-key".into(), Some("request".into()))]
                .into_iter()
                .collect(),
        );
        options.request.env = Some([("origin".into(), "request".into())].into_iter().collect());
        options.extra.insert("n".into(), Value::from(2));
        let transforms = ModelsRequestTransforms {
            transform_headers: Some(Arc::new(|mut headers| {
                Box::pin(async move {
                    headers.insert("transformed".into(), Some("yes".into()));
                    headers
                })
            })),
        };
        // When the provider fails after observing the resolved request.
        let result = models
            .generate_images(&model, &ImagesContext::default(), Some(options), transforms)
            .await;
        // Then the failure is a normalized image result using original model identity.
        assert_eq!(result.stop_reason, ImagesStopReason::Error);
        assert_eq!(result.error_message.as_deref(), Some("transport failed"));
        assert_eq!(result.model, model.id);
        assert!(result.output.is_empty());
    }

    #[tokio::test]
    async fn image_setup_failure_is_aborted_when_caller_signal_is_aborted() {
        // Given an already cancelled authenticated request.
        let models = collection(AuditAuth {
            failure: None,
            entered: None,
        });
        let controller = AbortController::new();
        controller.abort(Some(AbortReason::new("AbortError", "cancelled")));
        let mut options = ImagesOptions::default();
        options.request.signal = Some(controller.signal());
        // When auth setup observes cancellation.
        let result = models
            .generate_images(
                &image_model(),
                &ImagesContext::default(),
                Some(options),
                ModelsRequestTransforms::default(),
            )
            .await;
        // Then no provider invocation occurs and the result is aborted.
        assert_eq!(result.stop_reason, ImagesStopReason::Aborted);
        assert_eq!(result.error_message.as_deref(), Some("cancelled"));
    }

    #[tokio::test]
    async fn enumeration_propagates_credential_and_auth_failures() {
        for (failure, code) in [
            ("read", ModelsErrorCode::Auth),
            ("check", ModelsErrorCode::OAuth),
        ] {
            // Given a failing credential/auth boundary.
            let models = collection(AuditAuth {
                failure: Some(failure),
                entered: None,
            });
            // When available models are enumerated.
            let result = models.get_available(None, None).await;
            // Then the original failure is preserved, not an empty successful list.
            assert_eq!(result, Err(ModelsError::new(code, failure)));
        }
    }

    #[tokio::test]
    async fn enumeration_cancels_pending_credential_read() {
        // Given an exact notification that credential reading has begun.
        let (entered, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let models = collection(AuditAuth {
            failure: None,
            entered: Some(entered),
        });
        let controller = AbortController::new();
        let signal = controller.signal();
        // When cancellation occurs after entering a non-cooperative read.
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(models.get_available(None, Some(signal)), async {
                receiver.recv().await.expect("read entered");
                controller.abort(Some(AbortReason::new("AbortError", "cancelled")));
            })
            .0
        })
        .await
        .expect("bounded cancellation");
        // Then enumeration settles without waiting for the store.
        assert_eq!(
            result,
            Err(ModelsError::new(ModelsErrorCode::Auth, "cancelled"))
        );
    }
}
