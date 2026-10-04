//! Port of senpi `experimental/mini/worker/models-service.ts`.
//!
//! `ModelRuntime`, `Model`, and provider objects stay in the worker; what leaves is a serializable
//! catalog and account list, plus login prompts and notices as data. Provider login/logout need the
//! maho-core seam recorded as S5 in `.omo/evidence/residual-source/task-9-contracts.md` and are not
//! wired here.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use maho_ai::models::ModelsRefreshOptions;
use maho_core::auth_providers::oauth_provider_infos;

use super::runtime::ModelRuntimeHandle;
use super::shared::protocol::{AuthType, CommandResult, ModelSummary, ModelsEvent, ModelsState, ProviderAccount};

pub struct ModelsService {
    runtime: Arc<ModelRuntimeHandle>,
    publish: Arc<dyn Fn(ModelsEvent) + Send + Sync>,
    pending_auth: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Option<String>>>>,
    state: Mutex<ModelsState>,
}

impl ModelsService {
    pub async fn new(runtime: Arc<ModelRuntimeHandle>, publish: Arc<dyn Fn(ModelsEvent) + Send + Sync>) -> Self {
        let state = read_state(&runtime, false).await;
        Self { runtime, publish, pending_auth: Mutex::new(HashMap::new()), state: Mutex::new(state) }
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
