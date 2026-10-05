//! Port of senpi `experimental/mini/worker/models-service.ts`.
//!
//! `ModelRuntime`, `Model`, and provider objects stay in the worker; what leaves is a serializable
//! catalog and account list, plus login prompts and notices as data. `login`/`logout` drive the
//! provider flow through the published `ModelRuntime::login`/`logout` (maho-core, contract S5),
//! routing prompts and notices to the presentation as `Models` events.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use maho_ai::auth::types::{AuthEvent, AuthInteraction, AuthOperationOptions, AuthPrompt, AuthType as ProviderAuthType};
use maho_ai::models::ModelsRefreshOptions;
use maho_core::auth_providers::oauth_provider_infos;

use super::runtime::ModelRuntimeHandle;
use super::shared::protocol::{AuthNotice, AuthPromptRequest, AuthType, CommandResult, ModelSummary, ModelsEvent, ModelsState, ProviderAccount};

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
        self.pending.lock().unwrap_or_else(|error| error.into_inner()).insert(request_id.clone(), sender);
        (self.publish)(ModelsEvent::Prompt { request_id, request: AuthPromptRequest::from(prompt) });
        match receiver.await {
            Ok(Some(answer)) => Ok(answer),
            _ => Err(anyhow::anyhow!("Login cancelled")),
        }
    }

    fn notify(&self, event: AuthEvent) {
        (self.publish)(ModelsEvent::Notice { notice: AuthNotice::from(event) });
    }
}

pub struct ModelsService {
    runtime: Arc<ModelRuntimeHandle>,
    publish: Arc<dyn Fn(ModelsEvent) + Send + Sync>,
    pending_auth: PendingAuth,
    state: Mutex<ModelsState>,
}

impl ModelsService {
    pub async fn new(runtime: Arc<ModelRuntimeHandle>, publish: Arc<dyn Fn(ModelsEvent) + Send + Sync>) -> Self {
        let state = read_state(&runtime, false).await;
        Self { runtime, publish, pending_auth: Arc::new(Mutex::new(HashMap::new())), state: Mutex::new(state) }
    }

    pub fn state(&self) -> ModelsState {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub async fn refresh(&self) -> CommandResult {
        self.update(true).await;
        let result = self.runtime.refresh(ModelsRefreshOptions::default()).await;
        self.update(false).await;
        if result.errors.is_empty() {
            CommandResult::Ok
        } else {
            CommandResult::Error(format!(
                "Some catalogs could not be refreshed: {}",
                result.errors.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        }
    }

    pub async fn login(&self, provider_id: &str, auth_type: AuthType) -> CommandResult {
        let interaction = Arc::new(ServiceAuthInteraction { publish: self.publish.clone(), pending: self.pending_auth.clone() });
        let provider_auth_type = match auth_type { AuthType::Oauth => ProviderAuthType::OAuth, AuthType::ApiKey => ProviderAuthType::ApiKey };
        let result = self.runtime.login(provider_id, provider_auth_type, interaction).await;
        self.update(false).await;
        match result {
            Ok(_) => CommandResult::Ok,
            Err(error) => CommandResult::Error(error.message),
        }
    }

    pub async fn logout(&self, provider_id: &str) -> CommandResult {
        let result = self.runtime.logout(provider_id, AuthOperationOptions::default()).await;
        self.update(false).await;
        match result {
            Ok(()) => CommandResult::Ok,
            Err(error) => CommandResult::Error(error.message),
        }
    }

    pub fn auth_reply(&self, request_id: &str, answer: Option<String>) {
        let waiter = self.pending_auth.lock().unwrap_or_else(|error| error.into_inner()).remove(request_id);
        if let Some(waiter) = waiter {
            let _ = waiter.send(answer);
        }
    }

    pub async fn update(&self, refreshing: bool) {
        let state = read_state(&self.runtime, refreshing).await;
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = state.clone();
        (self.publish)(ModelsEvent::State { state });
    }
}

pub async fn read_state(runtime: &ModelRuntimeHandle, refreshing: bool) -> ModelsState {
    let oauth: HashSet<String> = oauth_provider_infos().into_iter().map(|info| info.id).collect();
    let models = runtime
        .available()
        .await
        .into_iter()
        .map(|model| ModelSummary { provider: model.provider, model_id: model.id, name: model.name })
        .collect();
    let mut accounts = Vec::new();
    for provider in runtime.providers().await {
        let (configured, source, label) = runtime.auth_status(provider.id()).await;
        let id = provider.id().to_owned();
        let name = provider.name().to_owned();
        let auth_type = if oauth.contains(&id) { AuthType::Oauth } else { AuthType::ApiKey };
        accounts.push(ProviderAccount {
            id,
            name: name.clone(),
            auth_type,
            configured,
            source: label.or(source),
            interactive: true,
            method_name: Some(name),
        });
    }
    accounts.sort_by(|left, right| left.name.cmp(&right.name));
    ModelsState { models, accounts, refreshing }
}
