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
        let compatibility=self.model_runtime.get_compatibility_request_config(model);
        let resolution=match self.model_runtime.get_auth(&model.provider).await{Ok(auth)=>auth,Err(error)=>return ResolvedRequestAuth::Failed{error:error.message}};
        if resolution.is_none()&&compatibility.auth_header{return ResolvedRequestAuth::Failed{error:format!("No API key found for \"{}\"",model.provider)};}
        let mut auth=resolution.as_ref().map(|r|r.auth.clone()).unwrap_or_default();
        let env=resolution.as_ref().and_then(|r|r.env.clone());let header_env=env.as_ref().map(|v|v.iter().map(|(k,v)|(k.clone(),v.clone())).collect());
        match self.model_runtime.get_compatibility_request_headers(model,header_env.as_ref()).await {
            Ok(Some(headers))=>auth.headers.get_or_insert_with(Default::default).extend(headers),Ok(None)=>{},Err(error)=>return ResolvedRequestAuth::Failed{error},
        }
        ResolvedRequestAuth::Resolved{auth,compatibility,env}
    }
    pub async fn get_api_key_for_provider(&self,id:&str)->Option<String>{self.model_runtime.get_auth(id).await.ok().flatten().and_then(|r|r.auth.api_key)}
    pub fn stream(&self,model:&Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::StreamOptions>)->maho_ai::types::AssistantMessageEventStream{self.model_runtime.stream(model,context,options)}
    pub fn stream_simple(&self,model:&Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::SimpleStreamOptions>)->maho_ai::types::AssistantMessageEventStream{self.model_runtime.stream_simple(model,context,options)}
    pub async fn complete(&self,model:&Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::StreamOptions>)->Result<maho_ai::types::AssistantMessage,maho_ai::utils::event_stream::StreamError>{self.model_runtime.complete(model,context,options).await}
    pub async fn refresh(&mut self,options:maho_ai::models::ModelsRefreshOptions)->maho_ai::models::ModelsRefreshResult{self.model_runtime.refresh(options).await}
}
