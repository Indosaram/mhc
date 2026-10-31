//! Port of senpi `experimental/mini/worker/models-service.ts`.
//!
//! `ModelRuntime`, `Model`, and provider objects stay in the worker; what leaves is a serializable
//! catalog and account list, plus login prompts and notices as data. `login`/`logout` drive the
//! provider flow through the published `ModelRuntime::login`/`logout` (maho-core, contract S5),
//! routing prompts and notices to the presentation as `Models` events.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use maho_ai::auth::types::{
    AuthEvent, AuthInteraction, AuthOperationOptions, AuthPrompt, AuthType as ProviderAuthType,
};
use maho_ai::models::ModelsRefreshOptions;

use super::runtime::ModelRuntimeHandle;
use super::shared::protocol::{
    AuthNotice, AuthPromptRequest, AuthType, CommandResult, ModelSummary, ModelsEvent, ModelsState,
    ProviderAccount,
};

/// senpi `CATALOG_REFRESH_TIMEOUT_MS`.
const CATALOG_REFRESH_TIMEOUT_MS: u64 = 15_000;

type PendingAuth = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Option<String>>>>>;

struct ServiceAuthInteraction {
    publish: Arc<dyn Fn(ModelsEvent) + Send + Sync>,
    pending: PendingAuth,
}

#[async_trait::async_trait]
impl AuthInteraction for ServiceAuthInteraction {
    fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> {
        None
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        let request_id = maho_ai::utils::uuid::uuidv7(None).unwrap_or_default();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(request_id.clone(), sender);
        (self.publish)(ModelsEvent::Prompt {
            request_id,
            request: AuthPromptRequest::from(prompt),
        });
        match receiver.await {
            Ok(Some(answer)) => Ok(answer),
            _ => Err(anyhow::anyhow!("Login cancelled")),
        }
    }

    fn notify(&self, event: AuthEvent) {
        (self.publish)(ModelsEvent::Notice {
            notice: AuthNotice::from(event),
        });
    }
}

pub struct ModelsService {
    runtime: Arc<ModelRuntimeHandle>,
    publish: Arc<dyn Fn(ModelsEvent) + Send + Sync>,
    pending_auth: PendingAuth,
    state: Mutex<ModelsState>,
}

impl ModelsService {
    pub async fn new(
        runtime: Arc<ModelRuntimeHandle>,
        publish: Arc<dyn Fn(ModelsEvent) + Send + Sync>,
    ) -> Result<Self, maho_ai::models::ModelsError> {
        let state = read_state(&runtime, false).await?;
        Ok(Self {
            runtime,
            publish,
            pending_auth: Arc::new(Mutex::new(HashMap::new())),
            state: Mutex::new(state),
        })
    }

    pub fn state(&self) -> ModelsState {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub async fn refresh(&self) -> CommandResult {
        if let Err(error) = self.update(true).await {
            return CommandResult::Error(error.to_string());
        }
        // senpi: `refreshModelCatalogs(runtime, signal)` under a CATALOG_REFRESH_TIMEOUT_MS abort.
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(CATALOG_REFRESH_TIMEOUT_MS)).await;
            controller.abort(None);
        });
        let runtime = self.runtime.clone();
        let runtime_key = Arc::as_ptr(&self.runtime) as usize as u64;
        let outcome = maho_interactive::model_catalog_refresh::refresh_model_catalogs(
            runtime_key,
            &signal,
            move |signal| async move {
                runtime
                    .refresh(ModelsRefreshOptions {
                        signal: Some(signal),
                        ..Default::default()
                    })
                    .await
            },
        )
        .await;
        timer.abort();
        let update = self.update(false).await;
        let result = match outcome {
            Ok(result) if result.errors.is_empty() => CommandResult::Ok,
            Ok(result) => CommandResult::Error(format!(
                "Some catalogs could not be refreshed: {}",
                result.errors.keys().cloned().collect::<Vec<_>>().join(", ")
            )),
            Err(reason) => CommandResult::Error(reason.to_string()),
        };
        match update {
            Err(error) => CommandResult::Error(error.to_string()),
            Ok(()) => result,
        }
    }

    pub async fn login(&self, provider_id: &str, auth_type: AuthType) -> CommandResult {
        let interaction = Arc::new(ServiceAuthInteraction {
            publish: self.publish.clone(),
            pending: self.pending_auth.clone(),
        });
        let provider_auth_type = match auth_type {
            AuthType::Oauth => ProviderAuthType::OAuth,
            AuthType::ApiKey => ProviderAuthType::ApiKey,
        };
        let result = self
            .runtime
            .login(provider_id, provider_auth_type, interaction)
            .await;
        if let Err(error) = self.update(false).await {
            return CommandResult::Error(error.to_string());
        }
        match result {
            Ok(_) => CommandResult::Ok,
            Err(error) => CommandResult::Error(error.message),
        }
    }

    pub async fn logout(&self, provider_id: &str) -> CommandResult {
        let result = self
            .runtime
            .logout(provider_id, AuthOperationOptions::default())
            .await;
        if let Err(error) = self.update(false).await {
            return CommandResult::Error(error.to_string());
        }
        match result {
            Ok(()) => CommandResult::Ok,
            Err(error) => CommandResult::Error(error.message),
        }
    }

    pub fn auth_reply(&self, request_id: &str, answer: Option<String>) {
        let waiter = self
            .pending_auth
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(request_id);
        if let Some(waiter) = waiter {
            let _ = waiter.send(answer);
        }
    }

    pub async fn update(&self, refreshing: bool) -> Result<(), maho_ai::models::ModelsError> {
        let state = read_state(&self.runtime, refreshing).await?;
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = state.clone();
        (self.publish)(ModelsEvent::State { state });
        Ok(())
    }
}

pub async fn read_state(
    runtime: &ModelRuntimeHandle,
    refreshing: bool,
) -> Result<ModelsState, maho_ai::models::ModelsError> {
    let models = runtime
        .available()
        .await?
        .into_iter()
        .map(|model| ModelSummary {
            provider: model.provider,
            model_id: model.id,
            name: model.name,
        })
        .collect();
    let mut accounts = Vec::new();
    for provider in runtime.providers().await {
        let (configured, source, label) = runtime.auth_status(provider.id()).await;
        // senpi spells this `(status.label ?? status.source === undefined) ? {} : { source: ... }`;
        // `===` binds tighter than `??`, so a label suppresses `source` and only a bare source shows.
        let source = if label.is_some() { None } else { source };
        let Some(auth) = provider.auth() else {
            continue;
        };
        let account = |auth_type, interactive, method_name: &str| ProviderAccount {
            id: provider.id().to_owned(),
            name: provider.name().to_owned(),
            auth_type,
            configured,
            source: source.clone(),
            interactive,
            method_name: Some(method_name.to_owned()),
        };
        if let Some(oauth) = &auth.oauth {
            accounts.push(account(AuthType::Oauth, true, oauth.name()));
        }
        if let Some(api_key) = &auth.api_key {
            accounts.push(account(
                AuthType::ApiKey,
                api_key.has_login(),
                api_key.name(),
            ));
        }
    }
    let collator = icu_collator::Collator::try_new(Default::default(), Default::default())
        .expect("compiled collation data is available");
    accounts.sort_by(|left, right| collator.compare(&left.name, &right.name));
    Ok(ModelsState {
        models,
        accounts,
        refreshing,
    })
}
