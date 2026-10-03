use maho_ai::model::Model;
use std::{collections::BTreeMap, future::Future, pin::Pin};

pub type AuthFuture<'a> = Pin<Box<dyn Future<Output = Result<Option<Credentials>, String>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials { pub api_key: Option<String>, pub headers: BTreeMap<String, Option<String>> }
pub trait ImageGenAuthRegistry: Send + Sync {
    fn stored_openai_is_api_key(&self) -> bool;
    fn get_all(&self) -> Vec<Model>;
    fn get_provider_auth(&self, provider: &str) -> AuthFuture<'_>;
    fn get_api_key_and_headers<'a>(&'a self, model: &'a Model) -> AuthFuture<'a>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageGenAuthResolution {
    Configured { kind: &'static str, api_key: String, base_url: String, headers: BTreeMap<String, String>, provenance: &'static str, provider_id: Option<String> },
    None { reason: &'static str },
}
const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const SETUP_REASON: &str = "Image generation is not configured. Store an OpenAI API key for provider \"openai\", configure an OpenAI-compatible gateway in models.json and optionally pin it with PI_IMAGE_GEN_PROVIDER, or set OPENAI_API_KEY.";
pub async fn resolve_context_image_gen_auth(ctx: &maho_ext_api::ExtensionContext) -> Result<ImageGenAuthResolution, maho_ext_api::ExtensionFailure> {
    let registry = if let Some(registry)=crate::state::image_gen_registry_override(){registry}else{
        std::sync::Arc::new(ContextAuthRegistry {stored_api_key:ctx.model_registry.get_stored_credential_type("openai")?==Some(maho_ai::auth::types::CredentialType::ApiKey),registry:ctx.model_registry.clone()}) as std::sync::Arc<dyn ImageGenAuthRegistry>
    };
    Ok(resolve_image_gen_auth(registry.as_ref(), &std::env::vars().collect()).await)
}
struct ContextAuthRegistry {stored_api_key:bool,registry:std::sync::Arc<dyn maho_ext_api::ModelRegistry>}
impl ImageGenAuthRegistry for ContextAuthRegistry {
    fn stored_openai_is_api_key(&self)->bool{self.stored_api_key}
    fn get_all(&self)->Vec<Model>{self.registry.get_all()}
    fn get_provider_auth(&self,provider:&str)->AuthFuture<'_>{
        let provider=provider.to_owned();Box::pin(async move{self.registry.get_provider_auth(&provider).await.map(|auth|auth.map(|auth|Credentials{api_key:auth.auth.api_key,headers:auth.auth.headers.unwrap_or_default()})).map_err(|error|error.message)})
    }
    fn get_api_key_and_headers<'a>(&'a self,model:&'a Model)->AuthFuture<'a>{
        Box::pin(async move{self.registry.get_api_key_and_headers(model).await.map(|auth|Some(Credentials{api_key:auth.auth.api_key,headers:auth.auth.headers.unwrap_or_default()})).map_err(|error|error.message)})
    }
}
fn non_empty(value: Option<&str>) -> Option<&str> { value.map(str::trim).filter(|value| !value.is_empty()) }
fn credentials(value: Option<Credentials>) -> Option<(String, BTreeMap<String,String>)> {
    let value = value?;
    let key = non_empty(value.api_key.as_deref())?;
    if key.to_ascii_uppercase().starts_with("SK-SENTINEL-DO-NOT-LOG") { return None; }
    let headers = value.headers.into_iter().filter_map(|(name,value)|value.filter(|value| !value.trim().is_empty()).map(|value|(name,value))).collect();
    Some((key.into(),headers))
}
pub async fn resolve_image_gen_auth(registry: &dyn ImageGenAuthRegistry, env: &BTreeMap<String,String>) -> ImageGenAuthResolution {
    if registry.stored_openai_is_api_key()
        && let Some((api_key,headers)) = credentials(registry.get_provider_auth("openai").await.ok().flatten()) {
        return ImageGenAuthResolution::Configured {kind:"native-openai",api_key,headers,base_url:OPENAI_BASE_URL.into(),provenance:"store",provider_id:Some("openai".into())};
    }
    let mut models = registry.get_all();
    models.sort_by(|left,right| left.provider.cmp(&right.provider).then(left.id.cmp(&right.id)));
    let mut groups = BTreeMap::new();
    for model in models {
        if model.provider != "openai" && matches!(model.api.as_str(),"openai-completions"|"openai-responses") && non_empty(Some(&model.base_url)).is_some() {
            groups.entry(model.provider.clone()).or_insert(model);
        }
    }
    let pinned = non_empty(env.get("PI_IMAGE_GEN_PROVIDER").map(String::as_str));
    let mut providers = groups.keys().filter(|provider|Some(provider.as_str())!=pinned).cloned().collect::<Vec<_>>();
    providers.sort_by(|left,right| (!left.to_ascii_lowercase().contains("openai")).cmp(&(!right.to_ascii_lowercase().contains("openai"))).then(left.cmp(right)));
    if let Some(pinned) = pinned { providers.insert(0,pinned.into()); }
    for provider in providers {
        let Some(model) = groups.get(&provider) else { continue; };
        if let Some((api_key,headers)) = credentials(registry.get_api_key_and_headers(model).await.ok().flatten()) {
            return ImageGenAuthResolution::Configured {kind:"gateway",api_key,headers,base_url:model.base_url.trim().into(),provenance:"provider-config",provider_id:Some(provider)};
        }
    }
    if let Some((api_key,headers)) = credentials(Some(Credentials {api_key:env.get("OPENAI_API_KEY").cloned(),headers:BTreeMap::new()})) {
        return ImageGenAuthResolution::Configured {kind:"native-openai",api_key,headers,base_url:OPENAI_BASE_URL.into(),provenance:"env",provider_id:None};
    }
    ImageGenAuthResolution::None {reason:SETUP_REASON}
}
