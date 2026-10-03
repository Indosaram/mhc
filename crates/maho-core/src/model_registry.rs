//! Synchronous extension-facing facade; execution remains in ModelRuntime.
use crate::model_runtime::{CreateModelRuntimeOptions,ModelRuntime};
use crate::provider_composer::{AuthStatus,ProviderConfigInput};
use crate::auth_storage::AuthStorage;
use maho_ai::models::Provider;
use maho_ai::types::Model;
use std::{path::PathBuf,sync::Arc};

#[derive(Clone)]
pub struct ModelRegistry {pub model_runtime:ModelRuntime,pub auth_storage:Arc<AuthStorage>}
#[derive(Debug)]
pub enum ResolvedRequestAuth {Resolved {auth:maho_ai::models::ProviderAuthResult,compatibility:crate::provider_composer::CompatibilityRequestConfig,env:Option<maho_ai::types::ProviderEnv>},Failed {error:String}}
impl ModelRegistry {
    pub fn new(runtime:ModelRuntime) -> Self {Self {auth_storage:runtime.credentials.clone(),model_runtime:runtime}}
    pub fn create(auth_storage:Arc<AuthStorage>,models_path:PathBuf) -> Self {Self::new(ModelRuntime::create_sync(CreateModelRuntimeOptions {credentials:Some(auth_storage),models_path:Some(models_path),..Default::default()}))}
    pub fn in_memory(auth_storage:Arc<AuthStorage>) -> Self {Self::new(ModelRuntime::create_sync(CreateModelRuntimeOptions {credentials:Some(auth_storage),..Default::default()}))}
    pub fn get_all(&self) -> Vec<Model> {self.model_runtime.get_models(None)}
    pub fn find(&self,provider:&str,id:&str) -> Option<Model> {self.model_runtime.get_model(provider,id)}
    pub fn get_error(&self) -> Option<String> {self.model_runtime.get_error()}
    pub fn get_available(&self) -> Vec<Model> {self.get_all().into_iter().filter(|m|self.has_configured_auth(m)).collect()}
    pub fn has_configured_auth(&self,model:&Model) -> bool {self.auth_storage.get(&model.provider).is_some()||self.model_runtime.get_provider_auth_status(&model.provider).configured}
    pub fn get_provider_auth_status(&self,id:&str) -> AuthStatus {self.model_runtime.get_provider_auth_status(id)}
    pub fn get_provider(&self,id:&str) -> Option<Arc<dyn Provider>> {self.model_runtime.get_provider(id)}
    pub fn get_provider_display_name(&self,id:&str) -> String {self.get_provider(id).map_or_else(||id.to_owned(),|p|p.name().to_owned())}
    pub fn is_using_oauth(&self,model:&Model) -> bool {self.model_runtime.is_using_oauth(&model.provider)}
    pub fn is_fallback_eligible(&self,model:&Model) -> bool {self.model_runtime.is_fallback_eligible(&model.provider)}
    pub fn register_provider(&mut self,id:&str,config:ProviderConfigInput) -> Result<(),String> {self.model_runtime.register_provider(id,config)}
    pub fn unregister_provider(&mut self,id:&str) {self.model_runtime.unregister_provider(id)}
    pub fn get_upstream_model_id(&self,model:&Model)->Option<String>{self.model_runtime.get_compatibility_request_config(model).upstream_model_id}
    pub fn get_service_tier(&self,model:&Model)->Option<maho_ai::types::ServiceTierPreference>{self.model_runtime.get_compatibility_request_config(model).service_tier}
    pub async fn get_api_key_and_headers(&self,model:&Model)->ResolvedRequestAuth {
        let mut compatibility=self.model_runtime.get_compatibility_request_config(model);
        let resolution=match self.model_runtime.get_auth(&model.provider).await{Ok(auth)=>auth,Err(error)=>return ResolvedRequestAuth::Failed{error:error.message}};
        if resolution.is_none()&&compatibility.auth_header{return ResolvedRequestAuth::Failed{error:format!("No API key found for \"{}\"",model.provider)};}
        if resolution.is_none() {
            compatibility.upstream_model_id = None;
            compatibility.service_tier = None;
        }
        let mut auth=resolution.as_ref().map(|r|r.auth.clone()).unwrap_or_default();
        let env=resolution.as_ref().and_then(|r|r.env.clone());let header_env=env.as_ref().map(|v|v.iter().map(|(k,v)|(k.clone(),v.clone())).collect());
        match self.model_runtime.get_compatibility_request_headers(model,header_env.as_ref()).await {
            Ok(Some(headers))=>auth.headers.get_or_insert_with(Default::default).extend(headers),Ok(None)=>{},Err(error)=>return ResolvedRequestAuth::Failed{error},
        }
        ResolvedRequestAuth::Resolved{auth,compatibility,env}
    }
    pub async fn get_api_key_for_provider(&self,id:&str)->Option<String>{self.model_runtime.get_auth(id).await.ok().flatten().and_then(|r|r.auth.api_key)}
    pub async fn get_credential_accounts(&self,provider:&str,agent_dir:&str)->Result<Vec<maho_ext_api::CredentialAccountSummary>,String>{
        let repository=crate::credential_pool::state_store::CredentialSlotRepository::new(
            &crate::credential_pool::state_store::credential_pool_state_path(agent_dir));
        let accounts=crate::credential_accounts::get_credential_accounts(&self.auth_storage,provider,
            &|key|std::env::var(key).ok(),&repository,maho_ai::utils::diagnostics::now_ms().max(0) as u64).await?;
        Ok(accounts.into_iter().map(|account|maho_ext_api::CredentialAccountSummary {
            name:account.name,display_name:account.display_name,blocked:account.blocked,pinned:account.pinned,
            source:match account.source {
                crate::credential_accounts::CredentialAccountSource::Login=>maho_ext_api::CredentialAccountSource::Login,
                crate::credential_accounts::CredentialAccountSource::Import=>maho_ext_api::CredentialAccountSource::Import,
                crate::credential_accounts::CredentialAccountSource::Env=>maho_ext_api::CredentialAccountSource::Env,
            },
        }).collect())
    }
    pub async fn pin_credential_account(&self,provider:&str,name:Option<&str>,agent_dir:&str)->Result<(),String>{
        self.pin_credential_account_guarded(provider,name,agent_dir,&||Ok(())).await
    }
    pub(crate) async fn pin_credential_account_guarded(&self,provider:&str,name:Option<&str>,agent_dir:&str,admit:&(dyn Fn()->Result<(),String>+Send+Sync))->Result<(),String>{
        let _mutation=self.auth_storage.account_mutation.lock().await;
        admit()?;
        let repository=crate::credential_pool::state_store::CredentialSlotRepository::new(
            &crate::credential_pool::state_store::credential_pool_state_path(agent_dir));
        crate::credential_accounts::pin_credential_account_guarded(&self.auth_storage,provider,name,
            &|key|std::env::var(key).ok(),&repository,maho_ai::utils::diagnostics::now_ms().max(0) as u64,admit).await.map(|_|())
    }
    pub async fn remove_credential_account(&self,provider:&str,name:&str,agent_dir:&str)->Result<(),String>{
        self.remove_credential_account_guarded(provider,name,agent_dir,&||Ok(())).await
    }
    pub(crate) async fn remove_credential_account_guarded(&self,provider:&str,name:&str,agent_dir:&str,admit:&(dyn Fn()->Result<(),String>+Send+Sync))->Result<(),String>{
        let _mutation=self.auth_storage.account_mutation.lock().await;
        admit()?;
        let repository=crate::credential_pool::state_store::CredentialSlotRepository::new(
            &crate::credential_pool::state_store::credential_pool_state_path(agent_dir));
        crate::credential_accounts::remove_credential_account_guarded(&self.auth_storage,provider,name,
            &|key|std::env::var(key).ok(),&repository,maho_ai::utils::diagnostics::now_ms().max(0) as u64,admit).await.map(|_|())
    }
    pub async fn rename_credential_account(&self,provider:&str,name:&str,display_name:Option<&str>)->Result<(),String>{
        self.rename_credential_account_guarded(provider,name,display_name,&||Ok(())).await
    }
    pub(crate) async fn rename_credential_account_guarded(&self,provider:&str,name:&str,display_name:Option<&str>,admit:&(dyn Fn()->Result<(),String>+Send+Sync))->Result<(),String>{
        let _mutation=self.auth_storage.account_mutation.lock().await;
        admit()?;
        crate::credential_accounts::rename_credential_account(&self.auth_storage,provider,name,display_name).await.map(|_|())
    }
    pub fn stream(&self,model:&Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::StreamOptions>)->maho_ai::types::AssistantMessageEventStream{self.model_runtime.stream(model,context,options)}
    pub fn stream_simple(&self,model:&Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::SimpleStreamOptions>)->maho_ai::types::AssistantMessageEventStream{self.model_runtime.stream_simple(model,context,options)}
    pub async fn complete(&self,model:&Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::StreamOptions>)->Result<maho_ai::types::AssistantMessage,maho_ai::utils::event_stream::StreamError>{self.model_runtime.complete(model,context,options).await}
    pub async fn refresh(&mut self,options:maho_ai::models::ModelsRefreshOptions)->maho_ai::models::ModelsRefreshResult{self.model_runtime.refresh(options).await}
}
