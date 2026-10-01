//! Provider catalog composition and authenticated model execution.
use crate::model_config::ModelConfig;
use crate::provider_composer::{AuthStatus, ProviderConfigInput, compose_model_provider};
use maho_ai::legacy_provider_ids::normalize_provider_id;
use maho_ai::models::{AuthResolution, AuthResolutionOverrides, CreateModelsOptions, Models, ModelsAuth, ModelsError, ModelsErrorCode, ModelsRefreshOptions, ModelsRefreshResult, Provider, create_models};
use maho_ai::types::{AssistantMessage, AssistantMessageEventStream, BoxFuture, Context, Model, SimpleStreamOptions, StreamOptions};
use indexmap::IndexMap;
use std::{path::PathBuf, sync::{Arc, RwLock}};

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
    refreshed:Arc<tokio::sync::Mutex<IndexMap<String,serde_json::Value>>>,
    auth_path:Option<PathBuf>,
}

impl ModelsAuth for RuntimeAuth {
    fn resolve<'a>(&'a self, provider: &'a dyn Provider, overrides: &'a AuthResolutionOverrides) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
        Box::pin(async move {
            let config = self.config.read().unwrap_or_else(|p| p.into_inner()).get_provider(provider.id()).cloned();
            let extension = self.extensions.read().unwrap_or_else(|p| p.into_inner()).get(provider.id()).cloned();
            let mut refreshed=self.refreshed.lock().await;
            let stored = refreshed.get(provider.id()).cloned().or_else(||self.auth_path.as_ref().and_then(|path|crate::auth_storage::read_stored_credential(provider.id(),&path.to_string_lossy()))).or_else(||self.credentials.get(provider.id()));
            if overrides.api_key.is_none()&&let Some(stored)=&stored&&crate::auth_storage::credential_kind(stored)==Some(crate::auth_storage::CredentialKind::Oauth) {
                let Some(oauth)=extension.as_ref().and_then(|e|e.oauth.clone()).or_else(||builtin_oauth(provider.id())) else{return Ok(None);};
                let mut credential:maho_ai::auth::types::OAuthCredential=serde_json::from_value(stored.clone()).map_err(|e|ModelsError::new(ModelsErrorCode::OAuth,e.to_string()))?;
                let now=maho_ai::utils::diagnostics::now_ms() as f64;
                if credential.expires-now < overrides.min_oauth_validity_ms.unwrap_or(60_000) as f64 {
                    let signal=maho_ai::utils::abort::operation_signal(overrides.signal.clone());
                    credential=oauth.refresh(&credential,&signal).await.map_err(|e|ModelsError::new(ModelsErrorCode::OAuth,e.to_string()))?;
                    let value=serde_json::to_value(maho_ai::auth::types::Credential::OAuth(credential.clone())).map_err(|e|ModelsError::new(ModelsErrorCode::OAuth,e.to_string()))?;
                    if let Some(path)=&self.auth_path {
                        let backend=crate::auth_storage::FileAuthStorageBackend::new(&path.to_string_lossy());
                        backend.with_lock_async(async |current| {
                            let mut data=crate::auth_storage::parse_auth_json(current.unwrap_or("{}"))?;
                            data.insert(provider.id().to_owned(),value.clone());Ok(((),Some(crate::auth_storage::serialize_auth_data(&data))))
                        }).await.map_err(|e|ModelsError::new(ModelsErrorCode::OAuth,e))?;
                    }
                    refreshed.insert(provider.id().to_owned(),value);
                }
                let auth=oauth.to_auth(&credential).await.map_err(|e|ModelsError::new(ModelsErrorCode::OAuth,e.to_string()))?;
                let raw_headers=crate::provider_api_key_auth::configured_headers(config.as_ref(),extension.as_ref().map(|e|&e.config));
                let headers=crate::resolve_config_value::resolve_headers_or_throw(raw_headers.as_ref(),&format!("provider \"{}\"",provider.id()),None).await.map_err(|e|ModelsError::new(ModelsErrorCode::Auth,e))?.map(|v|v.into_iter().map(|(k,v)|(k,Some(v))).collect());
                let auth_header=extension.as_ref().and_then(|c|c.auth_header).or(config.as_ref().and_then(|c|c.auth_header)).unwrap_or(false);
                let auth=crate::provider_api_key_auth::with_configured_auth(maho_ai::models::ProviderAuthResult{api_key:auth.api_key,headers:auth.headers,base_url:auth.base_url},headers,auth_header).map_err(|e|ModelsError::new(ModelsErrorCode::Auth,e))?;
                return Ok(Some(AuthResolution{auth,env:overrides.env.clone()}));
            }
            drop(refreshed);
            let key = overrides.api_key.as_deref().or_else(|| stored.as_ref().and_then(crate::auth_storage::credential_key));
            let env = overrides.env.as_ref().map(|v| v.iter().map(|(k,v)|(k.clone(),v.clone())).collect());
            let auth = crate::provider_api_key_auth::resolve_configured_auth(provider.id(),config.as_ref(),extension.as_ref().map(|e|&e.config),key,env.as_ref()).await
                .map_err(|e| ModelsError::new(ModelsErrorCode::Auth,e))?;
            Ok(auth.map(|auth| AuthResolution {auth,env:overrides.env.clone()}))
        })
    }
    fn read_credential<'a>(&'a self,id: &'a str,_signal: &'a maho_ai::utils::abort::AbortSignal) -> BoxFuture<'a,Result<Option<serde_json::Value>,ModelsError>> {
        Box::pin(async move { Ok(self.credentials.get(id)) })
    }
    fn refresh_credential<'a>(&'a self,provider: &'a dyn Provider,stored: Option<&'a serde_json::Value>,signal: &'a maho_ai::utils::abort::AbortSignal) -> BoxFuture<'a,Result<Option<serde_json::Value>,ModelsError>> {
        Box::pin(async move {
            if let Some(stored) = stored { return Ok(Some(stored.clone())); }
            self.read_credential(provider.id(),signal).await
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
        "devin"=>Some(oauth::devin::devin_oauth()),_=>None,
    }
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
}

impl ModelRuntime {
    pub fn create_sync(options: CreateModelRuntimeOptions) -> Self {
        let credentials = options.credentials.unwrap_or_else(|| Arc::new(options.auth_path.as_ref().map_or_else(||crate::auth_storage::AuthStorage::in_memory(Default::default()),|path|crate::auth_storage::AuthStorage::create(&path.to_string_lossy()))));
        let config = Arc::new(RwLock::new(ModelConfig::load_sync(options.models_path.as_deref())));
        let extensions = Arc::new(RwLock::new(IndexMap::new()));
        let auth = RuntimeAuth {config:config.clone(),extensions:extensions.clone(),credentials:credentials.clone(),refreshed:Arc::new(tokio::sync::Mutex::new(IndexMap::new())),auth_path:options.auth_path};
        let mut runtime = Self {
            models:create_models(Some(CreateModelsOptions {auth:Some(Arc::new(auth)),..Default::default()})),
            config, builtins: options.providers.unwrap_or_else(maho_ai::providers::all::builtin_providers).into_iter().map(|p| (p.id().to_owned(),p)).collect(),
            native:IndexMap::new(), extensions, errors:IndexMap::new(),models_path:options.models_path,credentials,
        };
        runtime.rebuild_providers(); runtime
    }
    pub async fn create(options: CreateModelRuntimeOptions) -> Self { Self::create_sync(options) }
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
    }
    pub fn get_error(&self) -> Option<String> {
        let mut errors:Vec<String> = self.config.read().unwrap_or_else(|p|p.into_inner()).get_error().map(str::to_owned).into_iter().collect();
        errors.extend(self.errors.iter().map(|(id,error)|format!("Provider \"{id}\": {error}")));
        if errors.is_empty() {None} else {Some(errors.join("\n\n"))}
    }
    pub fn get_warnings(&self) -> Vec<String> {self.config.read().unwrap_or_else(|p|p.into_inner()).get_warnings().to_vec()}
    pub fn get_models(&self,provider:Option<&str>) -> Vec<Model> {self.models.get_models(provider)}
    pub fn get_model(&self,provider:&str,id:&str) -> Option<Model> {self.models.get_model(&normalize_provider_id(provider),id)}
    pub fn get_provider(&self,id:&str) -> Option<Arc<dyn Provider>> {self.models.get_provider(&normalize_provider_id(id))}
    pub fn get_providers(&self) -> Vec<Arc<dyn Provider>> {self.models.get_providers()}
    pub async fn get_available(&self,provider:Option<&str>) -> Vec<Model> {self.models.get_available(provider).await}
    pub fn get_provider_auth_status(&self,id:&str) -> AuthStatus {
        if self.credentials.get(id).is_some() {return AuthStatus {configured:true,source:Some("stored".into()),label:None};}
        let config = self.config.read().unwrap_or_else(|p|p.into_inner()).get_provider(id).cloned();
        let extension = self.extensions.read().unwrap_or_else(|p|p.into_inner()).get(id).cloned();
        if let Some(status)=crate::provider_api_key_auth::configured_request_auth_status(config.as_ref(),extension.as_ref().map(|e|&e.config)){return status;}
        AuthStatus {configured:maho_ai::env_api_keys::get_env_api_key(id,None).is_some(),source:Some("environment".into()),label:None}
    }
    pub async fn get_auth(&self,id:&str) -> Result<Option<AuthResolution>,ModelsError> {self.models.get_auth(id,&AuthResolutionOverrides::default()).await}
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
        let config = ModelConfig::load(self.models_path.as_deref()).await;
        *self.config.write().unwrap_or_else(|p|p.into_inner()) = config;
        self.rebuild_providers(); self.models.refresh(options).await
    }
    pub fn register_provider(&mut self,id:&str,config:ProviderConfigInput) -> Result<(),String> {
        let id = normalize_provider_id(id);
        compose_model_provider(&id,self.native.get(&id).or_else(||self.builtins.get(&id)).cloned(),&self.config.read().unwrap_or_else(|p|p.into_inner()),Some(&config))?;
        self.extensions.write().unwrap_or_else(|p|p.into_inner()).insert(id.clone(),config);self.recompose_provider(&id);Ok(())
    }
    pub fn register_native_provider(&mut self,provider:Arc<dyn Provider>) {let id=normalize_provider_id(provider.id());self.native.insert(id.clone(),provider);self.recompose_provider(&id);}
    pub fn unregister_provider(&mut self,id:&str) {let id=normalize_provider_id(id);self.native.shift_remove(&id);self.extensions.write().unwrap_or_else(|p|p.into_inner()).shift_remove(&id);self.recompose_provider(&id);}
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
        let models=self.models.clone();let context=context.clone();let original=model.clone();
        Self::lazy_request(model,async move {
            Self::apply_configured_headers(&original,config.as_ref(),extension.as_ref(),&mut options).await?;
            Ok(models.stream(&request_model,&context,Some(options),Default::default()))
        })
    }
    pub fn stream_simple(&self,model:&Model,context:&Context,options:Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
        let mut options=options.unwrap_or_default();let (request_model,config,extension)=self.prepare_request(model,&mut options.stream);
        let models=self.models.clone();let context=context.clone();let original=model.clone();
        Self::lazy_request(model,async move {
            Self::apply_configured_headers(&original,config.as_ref(),extension.as_ref(),&mut options.stream).await?;
            Ok(models.stream_simple(&request_model,&context,Some(options),Default::default()))
        })
    }
    async fn apply_configured_headers(model:&Model,config:Option<&crate::model_config_schema::ModelsJsonProvider>,extension:Option<&ProviderConfigInput>,options:&mut StreamOptions)->Result<(),String> {
        let env=options.request.env.as_ref().map(|v|v.iter().map(|(k,v)|(k.clone(),v.clone())).collect());
        if let Some(mut headers)=crate::provider_composer::resolve_compatibility_request_headers(model,config,extension,env.as_ref()).await?{headers.extend(options.request.headers.take().unwrap_or_default());options.request.headers=Some(headers);}
        Ok(())
    }
    fn lazy_request(model:&Model,setup:impl std::future::Future<Output=Result<AssistantMessageEventStream,String>>+Send+'static)->AssistantMessageEventStream {
        let stream=AssistantMessageEventStream::assistant();let target=stream.clone();let model=model.clone();
        tokio::spawn(async move {
            let source=match setup.await {Ok(stream)=>stream,Err(error)=>maho_ai::utils::lazy::error_stream(&model,&error)};
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
