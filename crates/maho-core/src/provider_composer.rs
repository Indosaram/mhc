//! Builtin, models.json, and extension model layers, in upstream precedence order.
use crate::model_config::ModelConfig;
use crate::model_config_schema::{ModelsJsonModel, ModelsJsonProvider, ThinkingLevelMapMode};
use maho_ai::model::ModelCompat;
use maho_ai::models::Provider;
use maho_ai::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions};
use serde_json::{Map, Value};
use std::sync::Arc;

pub type ExtensionStream = Arc<dyn Fn(&Model,&Context,Option<SimpleStreamOptions>)->AssistantMessageEventStream+Send+Sync>;
pub type ExtensionRefresh = Arc<dyn Fn(maho_ai::models::RefreshModelsContext)->maho_ai::types::BoxFuture<'static,Result<Vec<ModelsJsonModel>,String>>+Send+Sync>;
#[derive(Clone,Default)]
pub struct ProviderConfigInput {
    pub config:ModelsJsonProvider,
    pub stream_simple:Option<ExtensionStream>,
    pub oauth:Option<Arc<dyn maho_ai::auth::types::OAuthAuth>>,
    pub fallback_eligible:Option<Arc<dyn Fn()->bool+Send+Sync>>,
    pub refresh_models:Option<ExtensionRefresh>,
    pub retry_policy:Option<maho_ai::utils::retry_profile::types::RetryPolicyProfile>,
}
impl std::ops::Deref for ProviderConfigInput {type Target=ModelsJsonProvider;fn deref(&self)->&Self::Target{&self.config}}
impl From<ModelsJsonProvider> for ProviderConfigInput {fn from(config:ModelsJsonProvider)->Self{Self{config,..Default::default()}}}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthStatus {
    pub configured: bool,
    pub source: Option<String>,
    pub label: Option<String>,
}

pub use crate::resolve_config_value::clear_config_value_cache as clear_api_key_cache;

fn merge_compat(base: Option<&Map<String, Value>>, overlay: Option<&Map<String, Value>>) -> Option<ModelCompat> {
    if base.is_none() && overlay.is_none() { return None; }
    let mut merged = base.cloned().unwrap_or_default();
    for (key, value) in overlay.into_iter().flatten() {
        if ["openRouterRouting", "vercelGatewayRouting", "chatTemplateKwargs", "chatTemplateArgs"].contains(&key.as_str()) {
            let mut nested = merged.get(key).and_then(Value::as_object).cloned().unwrap_or_default();
            if let Some(values) = value.as_object() { nested.extend(values.clone()); }
            merged.insert(key.clone(), Value::Object(nested));
        } else { merged.insert(key.clone(), value.clone()); }
    }
    Some(ModelCompat(merged))
}

fn model_from_json(id: &str, definition: &ModelsJsonModel, config: &ModelsJsonProvider, defaults: Option<&Model>, extension: Option<&ProviderConfigInput>) -> Result<Model, String> {
    let api = definition.api.as_ref().or(config.api.as_ref()).or(extension.and_then(|e| e.api.as_ref())).or(defaults.map(|m| &m.api))
        .ok_or_else(|| format!("Provider {id}, model {}: no \"api\" specified. Set at provider or model level.", definition.id))?;
    let base_url = definition.base_url.as_ref().or(config.base_url.as_ref()).or(extension.and_then(|e| e.base_url.as_ref())).or(defaults.map(|m| &m.base_url))
        .ok_or_else(|| format!("Provider {id}: \"baseUrl\" is required when defining custom models."))?;
    if definition.context_window.is_some_and(|v| v <= 0.0) { return Err(format!("Provider {id}, model {}: invalid contextWindow", definition.id)); }
    if definition.max_tokens.is_some_and(|v| v <= 0.0) { return Err(format!("Provider {id}, model {}: invalid maxTokens", definition.id)); }
    Ok(Model {
        id: definition.id.clone(), name: definition.name.clone().unwrap_or_else(|| definition.id.clone()),
        api: api.clone(), provider: id.to_owned(), base_url: base_url.clone(), reasoning: definition.reasoning.unwrap_or(false),
        thinking_level_map: definition.thinking_level_map.clone(), input: definition.input.clone().unwrap_or_else(|| vec![maho_ai::types::InputModality::Text]),
        cost: definition.cost.clone().unwrap_or_default(), context_window: definition.context_window.unwrap_or(128000.0) as u64,
        max_tokens: definition.max_tokens.unwrap_or(16384.0) as u64, sampling_params: definition.sampling_params.clone(), headers: None,
        cache_retention: definition.cache_retention.or(config.cache_retention), upstream_model_id: None, service_tier: None,
        recover_text_tool_calls: definition.recover_text_tool_calls, compat: merge_compat(config.compat.as_ref(), definition.compat.as_ref()),
    })
}

fn defaults<'a>(models: &'a [Model], id: &str, api: Option<&str>) -> Option<&'a Model> {
    models.iter().find(|m| m.id == id).or_else(|| api.and_then(|api| models.iter().find(|m| m.api == api)))
        .or_else(|| models.iter().find(|m| m.api == "openai-completions")).or_else(|| models.first())
}

pub fn compose_models(id: &str, base: &[Model], config: Option<&ModelsJsonProvider>, extension: Option<&ProviderConfigInput>) -> Result<Vec<Model>, String> {
    let mut models = base.to_vec();
    if let Some(config) = config {
        if config.models.as_ref().is_none_or(Vec::is_empty) && config.base_url.is_none() && config.headers.is_none()
            && config.extra_body.is_none() && config.compat.is_none() && config.model_overrides.as_ref().is_none_or(|v| v.is_empty())
            && config.whitelist.is_none() && config.blacklist.is_none() && config.api_key.is_none() && config.auth_header.is_none() {
            return Err(format!("Provider {id}: must specify \"baseUrl\", \"headers\", \"extraBody\", \"compat\", \"modelOverrides\", or \"models\"."));
        }
        if id == "ollama" && config.models.as_ref().is_some_and(|v| !v.is_empty()) { models.clear(); }
        for model in &mut models {
            if let Some(url) = &config.base_url { model.base_url.clone_from(url); }
            model.compat = merge_compat(model.compat.as_ref().map(|c| &c.0), config.compat.as_ref());
        }
        for definition in config.models.iter().flatten() {
            let model = model_from_json(id, definition, config, defaults(&models, &definition.id, definition.api.as_deref().or(extension.and_then(|e| e.api.as_deref())).or(config.api.as_deref())), extension)?;
            if let Some(slot) = models.iter_mut().find(|m| m.id == definition.id) { *slot = model; } else { models.push(model); }
        }
        models.retain(|m| config.whitelist.as_ref().is_none_or(|v| v.contains(&m.id)) && config.blacklist.as_ref().is_none_or(|v| !v.contains(&m.id)));
    }
    if let Some(extension) = extension {
        if let Some(declared) = &extension.models {
            let mut replacement = Vec::new();
            for definition in declared { replacement.push(model_from_json(id, definition, extension, defaults(&models, &definition.id, definition.api.as_deref().or(extension.api.as_deref())), None)?); }
            replacement.extend(models.into_iter().filter(|m| config.and_then(|c| c.models.as_ref()).is_some_and(|v| v.iter().any(|d| d.id == m.id)) && !declared.iter().any(|d| d.id == m.id)));
            models = replacement;
        } else if let Some(url) = &extension.base_url { for model in &mut models { model.base_url.clone_from(url); } }
    }
    for model in &mut models {
        let Some(overlay) = config.and_then(|c| c.model_overrides.as_ref()).and_then(|v| v.get(&model.id)) else { continue; };
        if let Some(name) = &overlay.name { model.name.clone_from(name); }
        if let Some(value) = overlay.reasoning { model.reasoning = value; }
        if let Some(value) = overlay.recover_text_tool_calls { model.recover_text_tool_calls = Some(value); }
        if let Some(map) = &overlay.thinking_level_map {
            match overlay.thinking_level_map_mode {
                Some(ThinkingLevelMapMode::Replace) => model.thinking_level_map = Some(map.clone()),
                Some(ThinkingLevelMapMode::Merge) | None => model.thinking_level_map.get_or_insert_with(Default::default).extend(map.clone()),
            }
        }
        if let Some(input) = &overlay.input { model.input.clone_from(input); }
        if let Some(cost) = &overlay.cost {
            if let Some(value) = cost.input { model.cost.input = value; }
            if let Some(value) = cost.output { model.cost.output = value; }
            if let Some(value) = cost.cache_read { model.cost.cache_read = value; }
            if let Some(value) = cost.cache_write { model.cost.cache_write = value; }
            if let Some(value) = &cost.tiers { model.cost.tiers = Some(value.clone()); }
        }
        if let Some(value) = overlay.context_window { model.context_window = value as u64; }
        if let Some(value) = overlay.max_tokens { model.max_tokens = value as u64; }
        if let Some(value) = overlay.cache_retention { model.cache_retention = Some(value); }
        if let Some(value) = &overlay.sampling_params { model.sampling_params.get_or_insert_with(Default::default).extend(value.clone()); }
        model.compat = merge_compat(model.compat.as_ref().map(|c| &c.0), overlay.compat.as_ref());
    }
    Ok(models)
}

struct ComposedProvider { id: String, name: String, models: Vec<Model>, base: Option<Arc<dyn Provider>>, config:Option<ModelsJsonProvider>, extension:Option<ProviderConfigInput>, refreshed:Arc<std::sync::RwLock<Option<Vec<ModelsJsonModel>>>> }
impl Provider for ComposedProvider {
    fn id(&self) -> &str { &self.id }
    fn name(&self) -> &str { &self.name }
    fn get_models(&self) -> Vec<Model> {
        if self.supports_refresh() {
            let mut extension=self.extension.clone();
            if let Some(refreshed)=self.refreshed.read().unwrap_or_else(|p|p.into_inner()).clone()&&let Some(extension)=&mut extension{extension.config.models=Some(refreshed);}
            compose_models(&self.id,&self.base.as_ref().map(|p|p.get_models()).unwrap_or_default(),self.config.as_ref(),extension.as_ref()).unwrap_or_else(|_|self.models.clone())
        } else {self.models.clone()}
    }
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        if let Some(extension)=&self.extension&&extension.api.as_ref().is_some_and(|api|api==&model.api)&&let Some(stream)=&extension.stream_simple{return stream(model,context,options.map(|stream|SimpleStreamOptions{stream,..Default::default()}));}
        match &self.base { Some(base) if base.get_models().iter().any(|m|m.api==model.api) => base.stream(model, context, options), _ => maho_ai::compat::stream(model, context, options) }
    }
    fn stream_simple(&self, model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
        if let Some(extension)=&self.extension&&extension.api.as_ref().is_some_and(|api|api==&model.api)&&let Some(stream)=&extension.stream_simple{return stream(model,context,options);}
        match &self.base { Some(base) if base.get_models().iter().any(|m|m.api==model.api) => base.stream_simple(model, context, options), _ => maho_ai::compat::stream_simple(model, context, options) }
    }
    fn filter_models(&self,models:Vec<Model>,credential:Option<&maho_ai::models::Credential>)->Vec<Model> {match &self.base {Some(base)=>base.filter_models(models,credential),None=>models}}
    fn supports_refresh(&self)->bool {self.base.as_ref().is_some_and(|p|p.supports_refresh())||self.extension.as_ref().is_some_and(|e|e.refresh_models.is_some())}
    fn refresh_models<'a>(&'a self,context:maho_ai::models::RefreshModelsContext)->maho_ai::types::BoxFuture<'a,Result<(),maho_ai::models::ModelsError>> {
        Box::pin(async move {
            if let Some(refresh)=self.extension.as_ref().and_then(|e|e.refresh_models.clone()) {
                let publisher=context.publisher();let values=refresh(context).await.map_err(|e|maho_ai::models::ModelsError::new(maho_ai::models::ModelsErrorCode::ModelSource,e))?;
                let mut extension=self.extension.clone();if let Some(extension)=&mut extension{extension.config.models=Some(values.clone());}
                compose_models(&self.id,&self.base.as_ref().map(|p|p.get_models()).unwrap_or_default(),self.config.as_ref(),extension.as_ref()).map_err(|e|maho_ai::models::ModelsError::new(maho_ai::models::ModelsErrorCode::ModelValidation,e))?;
                let refreshed=self.refreshed.clone();publisher(maho_ai::models::ModelsPublication{persist:None,update:Some(Box::new(move||{*refreshed.write().unwrap_or_else(|p|p.into_inner())=Some(values);} ))}).await.map_err(|e|maho_ai::models::ModelsError::new(maho_ai::models::ModelsErrorCode::ModelSource,e.to_string()))?;Ok(())
            } else if let Some(base)=&self.base {base.refresh_models(context).await} else {Ok(())}
        })
    }
    fn fetch_deferred(&self,model:&Model,handle:&maho_ai::types::DeferredHandle,options:Option<maho_ai::types::DeferredFetchOptions>)->Option<AssistantMessageEventStream> {self.base.as_ref().and_then(|p|p.fetch_deferred(model,handle,options))}
}

pub fn compose_model_provider(id: &str, base: Option<Arc<dyn Provider>>, config: &ModelConfig, extension: Option<&ProviderConfigInput>) -> Result<Arc<dyn Provider>, String> {
    let models = compose_models(id, &base.as_ref().map(|p| p.get_models()).unwrap_or_default(), config.get_provider(id), extension)?;
    let name = extension.and_then(|e| e.name.clone()).or_else(|| config.get_provider(id).and_then(|c| c.name.clone())).unwrap_or_else(|| base.as_ref().map_or_else(|| id.to_owned(), |p| p.name().to_owned()));
    Ok(Arc::new(ComposedProvider { id: id.to_owned(), name, models, base,config:config.get_provider(id).cloned(),extension:extension.cloned(),refreshed:Arc::new(std::sync::RwLock::new(None)) }))
}

#[derive(Debug,Clone,Default)]
pub struct CompatibilityRequestConfig {
    pub extra_body:Option<Map<String,Value>>,pub upstream_model_id:Option<String>,pub service_tier:Option<maho_ai::types::ServiceTierPreference>,pub auth_header:bool,
}
pub fn resolve_compatibility_request_config(model:&Model,config:Option<&ModelsJsonProvider>,extension:Option<&ProviderConfigInput>)->CompatibilityRequestConfig {
    let definition=config.and_then(|c|c.models.as_ref()).and_then(|v|v.iter().find(|d|d.id==model.id));
    let ext=extension.and_then(|c|c.models.as_ref()).and_then(|v|v.iter().find(|d|d.id==model.id));
    let overlay=config.and_then(|c|c.model_overrides.as_ref()).and_then(|v|v.get(&model.id));
    let mut body=Map::new();
    for value in [config.and_then(|c|c.extra_body.as_ref()),extension.and_then(|c|c.extra_body.as_ref()),overlay.and_then(|c|c.extra_body.as_ref()),definition.and_then(|c|c.extra_body.as_ref()),ext.and_then(|c|c.extra_body.as_ref())].into_iter().flatten(){body.extend(value.clone());}
    CompatibilityRequestConfig{extra_body:(!body.is_empty()).then_some(body),upstream_model_id:ext.and_then(|d|d.upstream_model_id.clone()).or_else(||definition.and_then(|d|d.upstream_model_id.clone())).or_else(||model.upstream_model_id.clone()),service_tier:ext.and_then(|d|d.service_tier).or_else(||definition.and_then(|d|d.service_tier)).or(model.service_tier),auth_header:extension.and_then(|c|c.auth_header).or(config.and_then(|c|c.auth_header)).unwrap_or(false)}
}
pub async fn resolve_compatibility_request_headers(model:&Model,config:Option<&ModelsJsonProvider>,extension:Option<&ProviderConfigInput>,env:Option<&std::collections::HashMap<String,String>>)->Result<Option<maho_ai::types::ProviderHeaders>,String> {
    let mut headers=crate::provider_api_key_auth::configured_headers(config,extension.map(|e|&e.config)).unwrap_or_default();
    let definition=config.and_then(|c|c.models.as_ref()).and_then(|v|v.iter().find(|d|d.id==model.id));
    let ext=extension.and_then(|c|c.models.as_ref()).and_then(|v|v.iter().find(|d|d.id==model.id));
    let overlay=config.and_then(|c|c.model_overrides.as_ref()).and_then(|v|v.get(&model.id));
    for value in [overlay.and_then(|c|c.headers.as_ref()),definition.and_then(|c|c.headers.as_ref()),ext.and_then(|c|c.headers.as_ref())].into_iter().flatten(){headers.extend(value.iter().map(|(k,v)|(k.clone(),v.clone())));}
    let resolved=crate::resolve_config_value::resolve_headers_or_throw(Some(&headers),&format!("model \"{}/{}\"",model.provider,model.id),env).await?;
    if model.headers.is_none()&&resolved.is_none(){return Ok(None);}
    let mut headers:maho_ai::types::ProviderHeaders=model.headers.clone().unwrap_or_default().into_iter().map(|(k,v)|(k,Some(v))).collect();
    headers.extend(resolved.into_iter().flatten().map(|(k,v)|(k,Some(v))));Ok(Some(headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_catalog_resolves_api_and_url() {
        let config = ModelConfig::parse(r#"{"providers":{"proxy":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#, "memory");
        let models = compose_models("proxy", &[], config.get_provider("proxy"), None).expect("compose");
        assert_eq!(models[0].api, "openai-completions"); assert_eq!(models[0].base_url, "http://localhost/v1");
    }
    #[test]
    fn user_overrides_apply_after_extension_catalog() {
        let config = ModelConfig::parse(r#"{"providers":{"proxy":{"models":[{"id":"sol"}],"modelOverrides":{"sol":{"maxTokens":42}}}}}"#, "memory");
        let extension = ModelsJsonProvider { api: Some("openai-completions".into()), base_url: Some("http://localhost/v1".into()), models: Some(vec![ModelsJsonModel {id:"sol".into(),max_tokens:Some(100.0),..Default::default()}]), ..Default::default() };
        assert_eq!(compose_models("proxy", &[], config.get_provider("proxy"), Some(&extension.into())).expect("compose")[0].max_tokens, 42);
    }
}
