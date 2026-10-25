//! Builtin, models.json, and extension model layers, in upstream precedence order.
use crate::model_config::ModelConfig;
use crate::model_config_schema::{ModelsJsonModel, ModelsJsonProvider, ThinkingLevelMapMode};
use maho_ai::model::ModelCompat;
use maho_ai::models::Provider;
use maho_ai::types::{
    AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions,
};
use serde_json::{Map, Value};
use std::sync::Arc;

pub type ExtensionStream = Arc<
    dyn Fn(&Model, &Context, Option<SimpleStreamOptions>) -> AssistantMessageEventStream
        + Send
        + Sync,
>;
pub type ExtensionRefresh = Arc<
    dyn Fn(
            maho_ai::models::RefreshModelsContext,
        ) -> maho_ai::types::BoxFuture<'static, Result<Vec<ModelsJsonModel>, String>>
        + Send
        + Sync,
>;
#[derive(Clone, Default)]
pub struct ProviderConfigInput {
    pub config: ModelsJsonProvider,
    pub stream_simple: Option<ExtensionStream>,
    pub oauth: Option<Arc<dyn maho_ai::auth::types::OAuthAuth>>,
    pub fallback_eligible: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub owns_account_selection: bool,
    pub refresh_models: Option<ExtensionRefresh>,
    pub retry_policy: Option<maho_ai::utils::retry_profile::types::RetryPolicyProfile>,
    pub ambient_auth: Option<Arc<dyn maho_ai::auth::types::ApiKeyAuth>>,
    pub images: indexmap::IndexMap<String, Arc<dyn maho_ai::types::ProviderImages>>,
    pub classifiers: indexmap::IndexMap<String, Arc<dyn maho_ai::types::ProviderClassifier>>,
}
impl std::ops::Deref for ProviderConfigInput {
    type Target = ModelsJsonProvider;
    fn deref(&self) -> &Self::Target {
        &self.config
    }
}
impl From<ModelsJsonProvider> for ProviderConfigInput {
    fn from(config: ModelsJsonProvider) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthStatus {
    pub ambient: bool,
    pub configured: bool,
    pub source: Option<String>,
    pub label: Option<String>,
}

pub use crate::resolve_config_value::clear_config_value_cache as clear_api_key_cache;

fn merge_compat(
    base: Option<&Map<String, Value>>,
    overlay: Option<&Map<String, Value>>,
) -> Option<ModelCompat> {
    if base.is_none() && overlay.is_none() {
        return None;
    }
    let mut merged = base.cloned().unwrap_or_default();
    for (key, value) in overlay.into_iter().flatten() {
        if [
            "openRouterRouting",
            "vercelGatewayRouting",
            "chatTemplateKwargs",
            "chatTemplateArgs",
        ]
        .contains(&key.as_str())
        {
            let mut nested = merged
                .get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(values) = value.as_object() {
                nested.extend(values.clone());
            }
            merged.insert(key.clone(), Value::Object(nested));
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    Some(ModelCompat(merged))
}

fn model_from_json(
    id: &str,
    definition: &ModelsJsonModel,
    config: &ModelsJsonProvider,
    defaults: Option<&Model>,
    extension: Option<&ProviderConfigInput>,
) -> Result<Model, String> {
    let api = definition
        .api
        .as_ref()
        .or(config.api.as_ref())
        .or(extension.and_then(|e| e.api.as_ref()))
        .or(defaults.map(|m| &m.api))
        .ok_or_else(|| {
            format!(
                "Provider {id}, model {}: no \"api\" specified. Set at provider or model level.",
                definition.id
            )
        })?;
    let base_url = definition
        .base_url
        .as_ref()
        .or(config.base_url.as_ref())
        .or(extension.and_then(|e| e.base_url.as_ref()))
        .or(defaults.map(|m| &m.base_url))
        .ok_or_else(|| {
            format!("Provider {id}: \"baseUrl\" is required when defining custom models.")
        })?;
    if definition.context_window.is_some_and(|v| v <= 0.0) {
        return Err(format!(
            "Provider {id}, model {}: invalid contextWindow",
            definition.id
        ));
    }
    if definition.max_tokens.is_some_and(|v| v <= 0.0) {
        return Err(format!(
            "Provider {id}, model {}: invalid maxTokens",
            definition.id
        ));
    }
    Ok(Model {
        id: definition.id.clone(),
        name: definition
            .name
            .clone()
            .unwrap_or_else(|| definition.id.clone()),
        api: api.clone(),
        provider: id.to_owned(),
        base_url: base_url.clone(),
        reasoning: definition.reasoning.unwrap_or(false),
        thinking_level_map: definition.thinking_level_map.clone(),
        input: definition
            .input
            .clone()
            .unwrap_or_else(|| vec![maho_ai::types::InputModality::Text]),
        cost: definition.cost.clone().or_else(|| defaults.filter(|m| m.id == definition.id).map(|m| m.cost.clone())).unwrap_or_else(|| maho_ai::types::ModelCost { unknown: Some(id != "ollama"), ..Default::default() }),
        context_window: definition.context_window.unwrap_or(128000.0) as u64,
        max_tokens: definition.max_tokens.unwrap_or(16384.0) as u64,
        sampling_params: definition.sampling_params.clone(),
        headers: None,
        cache_retention: definition.cache_retention.or(config.cache_retention),
        upstream_model_id: definition.upstream_model_id.clone(),
        deprecated: None,
        service_tier: definition.service_tier,
        recover_text_tool_calls: definition.recover_text_tool_calls,
        compat: merge_compat(config.compat.as_ref(), definition.compat.as_ref()),
        default_thinking_level: definition.default_thinking_level,
        input_limits: definition
            .input_limits
            .as_ref()
            .map(crate::model_config_schema::ModelInputLimits::to_shared),
        prompt_cache: definition.prompt_cache.as_ref().map(|cache| {
            maho_ai::model::ModelPromptCache {
                short: cache.short.map(std::num::NonZeroU64::get),
                long: cache.long.map(std::num::NonZeroU64::get),
            }
        }),
        supports_assistant_prefill: definition.supports_assistant_prefill,
    })
}

fn defaults<'a>(models: &'a [Model], id: &str, api: Option<&str>) -> Option<&'a Model> {
    models
        .iter()
        .find(|m| m.id == id)
        .or_else(|| api.and_then(|api| models.iter().find(|m| m.api == api)))
        .or_else(|| models.iter().find(|m| m.api == "openai-completions"))
        .or_else(|| models.first())
}

pub fn compose_models(
    id: &str,
    base: &[Model],
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Result<Vec<Model>, String> {
    let mut models = base.to_vec();
    if let Some(config) = config {
        if config.models.as_ref().is_none_or(Vec::is_empty)
            && config.base_url.is_none()
            && config.headers.is_none()
            && config.extra_body.is_none()
            && config.compat.is_none()
            && config.model_overrides.as_ref().is_none_or(|v| v.is_empty())
            && config.whitelist.is_none()
            && config.blacklist.is_none()
            && config.api_key.is_none()
            && config.auth_header.is_none()
        {
            return Err(format!(
                "Provider {id}: must specify \"baseUrl\", \"headers\", \"extraBody\", \"compat\", \"modelOverrides\", or \"models\"."
            ));
        }
        if id == "ollama" && config.models.as_ref().is_some_and(|v| !v.is_empty()) {
            models.clear();
        }
        for model in &mut models {
            if let Some(url) = &config.base_url {
                model.base_url.clone_from(url);
            }
            model.compat =
                merge_compat(model.compat.as_ref().map(|c| &c.0), config.compat.as_ref());
        }
        for definition in config.models.iter().flatten() {
            if definition
                .model_type
                .is_some_and(|kind| kind != maho_ai::types::ModelType::Chat)
            {
                continue;
            }
            let model = model_from_json(
                id,
                definition,
                config,
                defaults(
                    &models,
                    &definition.id,
                    definition
                        .api
                        .as_deref()
                        .or(extension.and_then(|e| e.api.as_deref()))
                        .or(config.api.as_deref()),
                ),
                extension,
            )?;
            if let Some(slot) = models.iter_mut().find(|m| m.id == definition.id) {
                *slot = model;
            } else {
                models.push(model);
            }
        }
        models.retain(|m| {
            config.whitelist.as_ref().is_none_or(|v| v.contains(&m.id))
                && config.blacklist.as_ref().is_none_or(|v| !v.contains(&m.id))
                && !(config.hide_free == Some(true) && m.cost.unknown != Some(true) && m.cost.input == 0.0 && m.cost.output == 0.0)
        });
    }
    if let Some(extension) = extension {
        if let Some(declared) = &extension.models {
            let mut replacement = Vec::new();
            for definition in declared {
                if definition
                    .model_type
                    .is_some_and(|kind| kind != maho_ai::types::ModelType::Chat)
                {
                    continue;
                }
                replacement.push(model_from_json(
                    id,
                    definition,
                    extension,
                    defaults(
                        &models,
                        &definition.id,
                        definition.api.as_deref().or(extension.api.as_deref()),
                    ),
                    None,
                )?);
            }
            replacement.extend(models.into_iter().filter(|m| {
                config
                    .and_then(|c| c.models.as_ref())
                    .is_some_and(|v| v.iter().any(|d| d.id == m.id))
                    && !declared.iter().any(|d| d.id == m.id)
            }));
            models = replacement;
        } else if let Some(url) = &extension.base_url {
            for model in &mut models {
                model.base_url.clone_from(url);
            }
        }
    }
    for model in &mut models {
        let Some(overlay) = config
            .and_then(|c| c.model_overrides.as_ref())
            .and_then(|v| v.get(&model.id))
        else {
            continue;
        };
        if let Some(name) = &overlay.name {
            model.name.clone_from(name);
        }
        if let Some(value) = overlay.reasoning {
            model.reasoning = value;
        }
        if let Some(value) = overlay.recover_text_tool_calls {
            model.recover_text_tool_calls = Some(value);
        }
        if let Some(map) = &overlay.thinking_level_map {
            match overlay.thinking_level_map_mode {
                Some(ThinkingLevelMapMode::Replace) => model.thinking_level_map = Some(map.clone()),
                Some(ThinkingLevelMapMode::Merge) | None => model
                    .thinking_level_map
                    .get_or_insert_with(Default::default)
                    .extend(map.clone()),
            }
        }
        if let Some(input) = &overlay.input {
            model.input.clone_from(input);
        }
        if let Some(cost) = &overlay.cost {
            model.cost.unknown = Some(model.cost.unknown == Some(true) && !(cost.input.is_some() && cost.output.is_some() && cost.cache_read.is_some() && cost.cache_write.is_some()));
            model.cost.effective_rates = None;
            if let Some(value) = cost.input {
                model.cost.input = value;
            }
            if let Some(value) = cost.output {
                model.cost.output = value;
            }
            if let Some(value) = cost.cache_read {
                model.cost.cache_read = value;
            }
            if let Some(value) = cost.cache_write {
                model.cost.cache_write = value;
            }
            if let Some(value) = &cost.tiers {
                model.cost.tiers = Some(value.clone());
            }
        }
        if let Some(value) = overlay.context_window {
            model.context_window = value as u64;
        }
        if let Some(value) = overlay.max_tokens {
            model.max_tokens = value as u64;
        }
        if let Some(value) = overlay.cache_retention {
            model.cache_retention = Some(value);
        }
        if let Some(cache) = &overlay.prompt_cache {
            let existing = model.prompt_cache.get_or_insert_with(Default::default);
            if let Some(short) = cache.short {
                existing.short = Some(short.get());
            }
            if let Some(long) = cache.long {
                existing.long = Some(long.get());
            }
        }
        if let Some(value) = &overlay.sampling_params {
            model
                .sampling_params
                .get_or_insert_with(Default::default)
                .extend(value.clone());
        }
        model.compat = merge_compat(model.compat.as_ref().map(|c| &c.0), overlay.compat.as_ref());
    }
    Ok(models)
}

/// `streamWith`'s text tool-call branch: a `compat.toolCallFormat` model with tools gets a
/// transformed (tool-free) context and its stream parsed back into tool calls.
fn tool_call_protocol(
    model: &Model,
    context: &Context,
) -> Option<(
    &'static dyn maho_ai::tool_call_middleware::types::ToolCallProtocol,
    Vec<maho_ai::types::Tool>,
)> {
    let format = maho_ai::get_tool_call_format(model)?;
    let tools = context.tools.as_ref().filter(|tools| !tools.is_empty())?;
    Some((
        maho_ai::tool_call_middleware::get_protocol(format),
        tools.clone(),
    ))
}

fn compose_nonchat_models(
    id: &str,
    base: Option<&Arc<dyn Provider>>,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Result<Vec<maho_ai::types::AnyModel>, String> {
    use maho_ai::types::{AnyModel, ModelType};
    let mut models: Vec<_> = base
        .into_iter()
        .flat_map(|base| base.get_all_models())
        .filter(|model| !matches!(model, AnyModel::Chat(_)))
        .collect();
    let defaults = models.clone();
    if extension.is_some_and(|extension| extension.models.is_some()) {
        models.clear();
    }
    for (provider, definitions) in config
        .into_iter()
        .map(|config| (config, config.models.as_ref()))
        .chain(
            extension
                .into_iter()
                .map(|extension| (&extension.config, extension.models.as_ref())),
        )
    {
        for definition in definitions.into_iter().flatten() {
            let Some(kind) = definition
                .model_type
                .filter(|kind| *kind != ModelType::Chat)
            else {
                continue;
            };
            let candidates: Vec<_> = defaults
                .iter()
                .filter(|model| {
                    matches!(
                        (kind, model),
                        (ModelType::Image, AnyModel::Image(_))
                            | (ModelType::Classifier, AnyModel::Classifier(_))
                    )
                })
                .collect();
            let default = candidates
                .iter()
                .copied()
                .find(|model| match model {
                    AnyModel::Image(model) => model.id == definition.id,
                    AnyModel::Classifier(model) => model.id == definition.id,
                    AnyModel::Chat(_) => false,
                })
                .or_else(|| {
                    definition.api.as_ref().and_then(|api| {
                        candidates.iter().copied().find(|model| match model {
                            AnyModel::Image(model) => &model.api == api,
                            AnyModel::Classifier(model) => &model.api == api,
                            AnyModel::Chat(_) => false,
                        })
                    })
                })
                .or_else(|| candidates.first().copied());
            let api = definition
                .api
                .as_ref()
                .or_else(|| {
                    default.map(|model| match model {
                        AnyModel::Image(model) => &model.api,
                        AnyModel::Classifier(model) => &model.api,
                        AnyModel::Chat(model) => &model.api,
                    })
                })
                .ok_or_else(|| {
                    format!("Provider {id}, model {}: no api specified", definition.id)
                })?;
            let base_url = definition
                .base_url
                .as_ref()
                .or(provider.base_url.as_ref())
                .or_else(|| {
                    default.map(|model| match model {
                        AnyModel::Image(model) => &model.base_url,
                        AnyModel::Classifier(model) => &model.base_url,
                        AnyModel::Chat(model) => &model.base_url,
                    })
                })
                .ok_or_else(|| {
                    format!(
                        "Provider {id}, model {}: no baseUrl specified",
                        definition.id
                    )
                })?;
            let model = match kind {
                ModelType::Image => AnyModel::Image(maho_ai::types::ImagesModel {
                    id: definition.id.clone(),
                    name: definition
                        .name
                        .clone()
                        .unwrap_or_else(|| definition.id.clone()),
                    api: api.clone(),
                    provider: id.into(),
                    base_url: base_url.clone(),
                    thinking_level_map: definition.thinking_level_map.clone(),
                    input: definition.input.clone().unwrap_or_default(),
                    input_limits: definition
                        .input_limits
                        .as_ref()
                        .map(crate::model_config_schema::ModelInputLimits::to_shared),
                    output: definition
                        .output
                        .clone()
                        .filter(|output| !output.is_empty())
                        .ok_or_else(|| {
                            format!(
                                "Provider {id}, image model {}: output is required",
                                definition.id
                            )
                        })?,
                    cost: definition.image_cost.clone().unwrap_or_else(|| {
                        let cost = definition.cost.clone().unwrap_or_default();
                        maho_ai::types::ImagesModelCost {
                            input: cost.input,
                            output: cost.output,
                            cache_read: cost.cache_read,
                            cache_write: cost.cache_write,
                            tiers: cost.tiers,
                            effective_rates: cost.effective_rates,
                            image_input: None,
                        }
                    }),
                    sampling_params: definition.sampling_params.clone(),
                    headers: None,
                    cache_retention: definition.cache_retention.or(provider.cache_retention),
                    upstream_model_id: definition.upstream_model_id.clone(),
                    service_tier: definition.service_tier,
                    recover_text_tool_calls: definition.recover_text_tool_calls,
                }),
                ModelType::Classifier => AnyModel::Classifier(maho_ai::types::ClassifierModel {
                    model_type: maho_ai::types::ClassifierModelType::Classifier,
                    id: definition.id.clone(),
                    name: definition
                        .name
                        .clone()
                        .unwrap_or_else(|| definition.id.clone()),
                    api: api.clone(),
                    provider: id.into(),
                    base_url: base_url.clone(),
                    input: definition.input.clone().unwrap_or_default(),
                    input_limits: definition
                        .input_limits
                        .as_ref()
                        .map(crate::model_config_schema::ModelInputLimits::to_shared),
                    cost: definition.cost.clone().unwrap_or_default(),
                    headers: None,
                    context_window: definition
                        .context_window
                        .filter(|value| value.is_finite() && *value > 0.0)
                        .ok_or_else(|| {
                            format!(
                                "Provider {id}, classifier model {}: contextWindow is required",
                                definition.id
                            )
                        })? as u64,
                }),
                ModelType::Chat => unreachable!("chat models are composed separately"),
            };
            let model_id = |model: &AnyModel| match model {
                AnyModel::Chat(model) => model.id.clone(),
                AnyModel::Image(model) => model.id.clone(),
                AnyModel::Classifier(model) => model.id.clone(),
            };
            if let Some(slot) = models
                .iter_mut()
                .find(|model| model_id(model) == definition.id)
            {
                *slot = model;
            } else {
                models.push(model);
            }
        }
    }
    models.retain(|model| {
        let model_id = match model {
            AnyModel::Chat(model) => &model.id,
            AnyModel::Image(model) => &model.id,
            AnyModel::Classifier(model) => &model.id,
        };
        config.is_none_or(|config| {
            config
                .whitelist
                .as_ref()
                .is_none_or(|list| list.contains(model_id))
                && config
                    .blacklist
                    .as_ref()
                    .is_none_or(|list| !list.contains(model_id))
        })
    });
    Ok(models)
}

struct ComposedProvider {
    id: String,
    name: String,
    models: Vec<Model>,
    nonchat_models: Vec<maho_ai::types::AnyModel>,
    refreshed_nonchat: Arc<std::sync::RwLock<Option<Vec<maho_ai::types::AnyModel>>>>,
    base: Option<Arc<dyn Provider>>,
    config: Option<ModelsJsonProvider>,
    extension: Option<ProviderConfigInput>,
    refreshed: Arc<std::sync::RwLock<Option<Vec<ModelsJsonModel>>>>,
}
impl Provider for ComposedProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn auth(&self) -> Option<&maho_ai::auth::types::ProviderAuth> {
        self.base.as_ref().and_then(|base| base.auth())
    }
    fn retry_policy(&self) -> Option<&maho_ai::utils::retry_profile::types::RetryPolicyProfile> {
        self.extension
            .as_ref()
            .and_then(|extension| extension.retry_policy.as_ref())
            .or_else(|| self.base.as_ref().and_then(|base| base.retry_policy()))
    }
    fn get_models(&self) -> Vec<Model> {
        if self.supports_refresh() {
            let mut extension = self.extension.clone();
            if let Some(refreshed) = self
                .refreshed
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
                && let Some(extension) = &mut extension
            {
                extension.config.models = Some(refreshed);
            }
            compose_models(
                &self.id,
                &self
                    .base
                    .as_ref()
                    .map(|p| p.get_models())
                    .unwrap_or_default(),
                self.config.as_ref(),
                extension.as_ref(),
            )
            .unwrap_or_else(|_| self.models.clone())
        } else {
            self.models.clone()
        }
    }
    fn get_all_models(&self) -> Vec<maho_ai::types::AnyModel> {
        self.get_models()
            .into_iter()
            .map(maho_ai::types::AnyModel::Chat)
            .chain(
                self.refreshed_nonchat
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .unwrap_or(&self.nonchat_models)
                    .clone(),
            )
            .collect()
    }
    fn supports_classification(&self) -> bool {
        self.extension
            .as_ref()
            .is_some_and(|extension| !extension.classifiers.is_empty())
            || self
                .base
                .as_ref()
                .is_some_and(|base| base.supports_classification())
    }
    fn classify<'a>(
        &'a self,
        model: &'a maho_ai::types::ClassifierModel,
        context: &'a maho_ai::types::ClassifierContext,
        options: Option<maho_ai::types::ClassifierOptions>,
    ) -> maho_ai::types::BoxFuture<'a, maho_ai::types::ClassifierResult> {
        if let Some(classifier) = self
            .extension
            .as_ref()
            .and_then(|extension| extension.classifiers.get(&model.api))
        {
            return classifier.classify(model, context, options);
        }
        if let Some(base) = &self.base {
            return base.classify(model, context, options);
        }
        Box::pin(async move {
            let mut result = maho_ai::models::system_one_shared::result(model);
            maho_ai::models::system_one_shared::fail(
                &mut result,
                &options.unwrap_or_default(),
                format!(
                    "Provider {} has no classifier API implementation for \"{}\"",
                    self.id, model.api
                ),
            );
            result
        })
    }
    fn generate_images<'a>(
        &'a self,
        model: &'a maho_ai::types::ImagesModel,
        context: &'a maho_ai::types::ImagesContext,
        options: Option<maho_ai::types::ImagesOptions>,
    ) -> maho_ai::types::BoxFuture<'a, Result<maho_ai::types::AssistantImages, String>> {
        if let Some(images) = self
            .extension
            .as_ref()
            .and_then(|extension| extension.images.get(&model.api))
        {
            return images.generate_images(model, context, options);
        }
        match &self.base {
            Some(base) => base.generate_images(model, context, options),
            None => Box::pin(async move {
                Err(format!(
                    "Provider {} has no image API implementation for \"{}\"",
                    self.id, model.api
                ))
            }),
        }
    }
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        if let Some((protocol, tools)) = tool_call_protocol(model, context) {
            return maho_ai::tool_call_middleware::wrap_stream_with_tool_call_middleware(
                self.stream(
                    model,
                    &maho_ai::tool_call_middleware::transform_context(context, protocol),
                    options,
                ),
                protocol,
                tools,
            );
        }
        if let Some(extension) = &self.extension
            && extension.api.as_ref().is_some_and(|api| api == &model.api)
            && let Some(stream) = &extension.stream_simple
        {
            return stream(
                model,
                context,
                options.map(|stream| SimpleStreamOptions {
                    stream,
                    ..Default::default()
                }),
            );
        }
        match &self.base {
            Some(base) if base.get_models().iter().any(|m| m.api == model.api) => {
                base.stream(model, context, options)
            }
            _ => maho_ai::compat::stream(model, context, options),
        }
    }
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        if let Some((protocol, tools)) = tool_call_protocol(model, context) {
            return maho_ai::tool_call_middleware::wrap_stream_with_tool_call_middleware(
                self.stream_simple(
                    model,
                    &maho_ai::tool_call_middleware::transform_context(context, protocol),
                    options,
                ),
                protocol,
                tools,
            );
        }
        if let Some(extension) = &self.extension
            && extension.api.as_ref().is_some_and(|api| api == &model.api)
            && let Some(stream) = &extension.stream_simple
        {
            return stream(model, context, options);
        }
        match &self.base {
            Some(base) if base.get_models().iter().any(|m| m.api == model.api) => {
                base.stream_simple(model, context, options)
            }
            _ => maho_ai::compat::stream_simple(model, context, options),
        }
    }
    fn filter_models(
        &self,
        models: Vec<Model>,
        credential: Option<&maho_ai::models::Credential>,
    ) -> Vec<Model> {
        match &self.base {
            Some(base) => base.filter_models(models, credential),
            None => models,
        }
    }
    fn supports_refresh(&self) -> bool {
        self.base.as_ref().is_some_and(|p| p.supports_refresh())
            || self
                .extension
                .as_ref()
                .is_some_and(|e| e.refresh_models.is_some())
    }
    fn refresh_models<'a>(
        &'a self,
        context: maho_ai::models::RefreshModelsContext,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), maho_ai::models::ModelsError>> {
        Box::pin(async move {
            if let Some(refresh) = self
                .extension
                .as_ref()
                .and_then(|e| e.refresh_models.clone())
            {
                let publisher = context.publisher();
                let values = refresh(context).await.map_err(|e| {
                    maho_ai::models::ModelsError::new(
                        maho_ai::models::ModelsErrorCode::ModelSource,
                        e,
                    )
                })?;
                let mut extension = self.extension.clone();
                if let Some(extension) = &mut extension {
                    extension.config.models = Some(values.clone());
                }
                compose_models(
                    &self.id,
                    &self
                        .base
                        .as_ref()
                        .map(|p| p.get_models())
                        .unwrap_or_default(),
                    self.config.as_ref(),
                    extension.as_ref(),
                )
                .map_err(|e| {
                    maho_ai::models::ModelsError::new(
                        maho_ai::models::ModelsErrorCode::ModelValidation,
                        e,
                    )
                })?;
                let nonchat = compose_nonchat_models(
                    &self.id,
                    self.base.as_ref(),
                    self.config.as_ref(),
                    extension.as_ref(),
                )
                .map_err(|error| {
                    maho_ai::models::ModelsError::new(
                        maho_ai::models::ModelsErrorCode::ModelValidation,
                        error,
                    )
                })?;
                let refreshed_nonchat = self.refreshed_nonchat.clone();
                let refreshed = self.refreshed.clone();
                publisher(maho_ai::models::ModelsPublication {
                    persist: None,
                    update: Some(Box::new(move || {
                        *refreshed_nonchat
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(nonchat);
                        *refreshed.write().unwrap_or_else(|p| p.into_inner()) = Some(values);
                    })),
                })
                .await
                .map_err(|e| {
                    maho_ai::models::ModelsError::new(
                        maho_ai::models::ModelsErrorCode::ModelSource,
                        e.to_string(),
                    )
                })?;
                Ok(())
            } else if let Some(base) = &self.base {
                base.refresh_models(context).await
            } else {
                Ok(())
            }
        })
    }
    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &maho_ai::types::DeferredHandle,
        options: Option<maho_ai::types::DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.base
            .as_ref()
            .and_then(|p| p.fetch_deferred(model, handle, options))
    }
    fn supports_cancel_deferred(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|p| p.supports_cancel_deferred())
    }
    fn cancel_deferred<'a>(
        &'a self,
        model: &'a Model,
        handle: &'a maho_ai::types::DeferredHandle,
        options: Option<maho_ai::types::DeferredCancelOptions>,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), String>> {
        match &self.base {
            Some(base) => base.cancel_deferred(model, handle, options),
            None => Box::pin(async { Err("API cannot cancel deferred responses".to_owned()) }),
        }
    }
}

/// `composeOAuthAuth`: the extension's (or base provider's) OAuth with models.json / extension
/// headers and `authHeader` applied to every derived request auth.
pub struct ComposedOAuthAuth {
    provider_id: String,
    oauth: Arc<dyn maho_ai::auth::types::OAuthAuth>,
    raw_headers: Option<std::collections::HashMap<String, String>>,
    auth_header: bool,
}

pub fn compose_oauth_auth(
    provider_id: &str,
    oauth: Option<Arc<dyn maho_ai::auth::types::OAuthAuth>>,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ModelsJsonProvider>,
) -> Option<ComposedOAuthAuth> {
    Some(ComposedOAuthAuth {
        provider_id: provider_id.to_owned(),
        oauth: oauth?,
        raw_headers: crate::provider_api_key_auth::configured_headers(config, extension),
        auth_header: extension
            .and_then(|c| c.auth_header)
            .or(config.and_then(|c| c.auth_header))
            .unwrap_or(false),
    })
}

#[async_trait::async_trait]
impl maho_ai::auth::types::OAuthAuth for ComposedOAuthAuth {
    fn name(&self) -> &str {
        self.oauth.name()
    }
    fn is_subscription(&self) -> bool {
        self.oauth.is_subscription()
    }
    fn login_label(&self) -> Option<&str> {
        self.oauth.login_label()
    }
    async fn login(
        &self,
        interaction: &maho_ai::auth::types::ProviderAuthInteraction,
    ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
        self.oauth.login(interaction).await
    }
    async fn refresh(
        &self,
        credential: &maho_ai::auth::types::OAuthCredential,
        signal: &maho_ai::utils::abort::AbortSignal,
    ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
        self.oauth.refresh(credential, signal).await
    }
    async fn check(
        &self,
        ctx: &dyn maho_ai::auth::types::AuthContext,
        credential: Option<&maho_ai::auth::types::OAuthCredential>,
        signal: &maho_ai::utils::abort::AbortSignal,
    ) -> anyhow::Result<Option<maho_ai::auth::types::AuthCheck>> {
        self.oauth.check(ctx, credential, signal).await
    }
    fn has_resolve_ambient(&self) -> bool {
        self.oauth.has_resolve_ambient()
    }
    async fn resolve_ambient(
        &self,
        ctx: &dyn maho_ai::auth::types::AuthContext,
        env: Option<&maho_ai::types::ProviderEnv>,
        signal: &maho_ai::utils::abort::AbortSignal,
    ) -> anyhow::Result<Option<maho_ai::auth::types::AuthResult>> {
        self.oauth.resolve_ambient(ctx, env, signal).await
    }
    async fn to_auth(
        &self,
        credential: &maho_ai::auth::types::OAuthCredential,
    ) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
        let auth = self.oauth.to_auth(credential).await?;
        let env: Option<std::collections::HashMap<String, String>> = credential
            .get_extra("env")
            .and_then(Value::as_object)
            .map(|env| {
                env.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .collect()
            });
        let headers = crate::resolve_config_value::resolve_headers_or_throw(
            self.raw_headers.as_ref(),
            &format!("provider \"{}\"", self.provider_id),
            env.as_ref(),
        )
        .await
        .map_err(|error| anyhow::anyhow!(error))?
        .map(|values| {
            values
                .into_iter()
                .map(|(k, v)| (k, Some(v)))
                .collect::<maho_ai::types::ProviderHeaders>()
        });
        let auth = crate::provider_api_key_auth::with_configured_auth(
            crate::provider_api_key_auth::model_auth_to_result(auth),
            headers,
            self.auth_header,
        )
        .map_err(|error| anyhow::anyhow!(error))?;
        Ok(crate::provider_api_key_auth::result_to_model_auth(auth))
    }
}

pub fn compose_model_provider(
    id: &str,
    base: Option<Arc<dyn Provider>>,
    config: &ModelConfig,
    extension: Option<&ProviderConfigInput>,
) -> Result<Arc<dyn Provider>, String> {
    if extension
        .is_some_and(|extension| extension.stream_simple.is_some() && extension.api.is_none())
    {
        return Err(format!(
            "Provider {id}: \"api\" is required when registering streamSimple."
        ));
    }
    let models = compose_models(
        id,
        &base.as_ref().map(|p| p.get_models()).unwrap_or_default(),
        config.get_provider(id),
        extension,
    )?;
    let nonchat_models =
        compose_nonchat_models(id, base.as_ref(), config.get_provider(id), extension)?;
    let name = extension
        .and_then(|e| e.name.clone())
        .or_else(|| config.get_provider(id).and_then(|c| c.name.clone()))
        .unwrap_or_else(|| {
            base.as_ref()
                .map_or_else(|| id.to_owned(), |p| p.name().to_owned())
        });
    Ok(Arc::new(ComposedProvider {
        id: id.to_owned(),
        name,
        models,
        nonchat_models,
        refreshed_nonchat: Arc::new(std::sync::RwLock::new(None)),
        base,
        config: config.get_provider(id).cloned(),
        extension: extension.cloned(),
        refreshed: Arc::new(std::sync::RwLock::new(None)),
    }))
}

#[derive(Debug, Clone, Default)]
pub struct CompatibilityRequestConfig {
    pub extra_body: Option<Map<String, Value>>,
    pub upstream_model_id: Option<String>,
    pub service_tier: Option<maho_ai::types::ServiceTierPreference>,
    pub auth_header: bool,
    /// `promptPreset` from `modelOverrides`, extension or models.json definition (upstream keeps it on the model).
    pub prompt_preset: Option<String>,
}
pub fn resolve_compatibility_request_config(
    model: &Model,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> CompatibilityRequestConfig {
    let definition = config
        .and_then(|c| c.models.as_ref())
        .and_then(|v| v.iter().find(|d| d.id == model.id));
    let ext = extension
        .and_then(|c| c.models.as_ref())
        .and_then(|v| v.iter().find(|d| d.id == model.id));
    let overlay = config
        .and_then(|c| c.model_overrides.as_ref())
        .and_then(|v| v.get(&model.id));
    let mut body = Map::new();
    for value in [
        config.and_then(|c| c.extra_body.as_ref()),
        extension.and_then(|c| c.extra_body.as_ref()),
        overlay.and_then(|c| c.extra_body.as_ref()),
        definition.and_then(|c| c.extra_body.as_ref()),
        ext.and_then(|c| c.extra_body.as_ref()),
    ]
    .into_iter()
    .flatten()
    {
        body.extend(value.clone());
    }
    CompatibilityRequestConfig {
        extra_body: (!body.is_empty()).then_some(body),
        upstream_model_id: ext
            .and_then(|d| d.upstream_model_id.clone())
            .or_else(|| definition.and_then(|d| d.upstream_model_id.clone()))
            .or_else(|| model.upstream_model_id.clone()),
        service_tier: ext
            .and_then(|d| d.service_tier)
            .or_else(|| definition.and_then(|d| d.service_tier))
            .or(model.service_tier),
        auth_header: extension
            .and_then(|c| c.auth_header)
            .or(config.and_then(|c| c.auth_header))
            .unwrap_or(false),
        prompt_preset: overlay
            .and_then(|o| o.prompt_preset.clone())
            .or_else(|| ext.and_then(|d| d.prompt_preset.clone()))
            .or_else(|| definition.and_then(|d| d.prompt_preset.clone())),
    }
}
pub async fn resolve_compatibility_request_headers(
    model: &Model,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
    env: Option<&std::collections::HashMap<String, String>>,
) -> Result<Option<maho_ai::types::ProviderHeaders>, String> {
    let mut headers =
        crate::provider_api_key_auth::configured_headers(config, extension.map(|e| &e.config))
            .unwrap_or_default();
    let definition = config
        .and_then(|c| c.models.as_ref())
        .and_then(|v| v.iter().find(|d| d.id == model.id));
    let ext = extension
        .and_then(|c| c.models.as_ref())
        .and_then(|v| v.iter().find(|d| d.id == model.id));
    let overlay = config
        .and_then(|c| c.model_overrides.as_ref())
        .and_then(|v| v.get(&model.id));
    for value in [
        overlay.and_then(|c| c.headers.as_ref()),
        definition.and_then(|c| c.headers.as_ref()),
        ext.and_then(|c| c.headers.as_ref()),
    ]
    .into_iter()
    .flatten()
    {
        headers.extend(value.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let resolved = crate::resolve_config_value::resolve_headers_or_throw(
        Some(&headers),
        &format!("model \"{}/{}\"", model.provider, model.id),
        env,
    )
    .await?;
    if model.headers.is_none() && resolved.is_none() {
        return Ok(None);
    }
    let mut headers: maho_ai::types::ProviderHeaders = model
        .headers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, Some(v)))
        .collect();
    headers.extend(resolved.into_iter().flatten().map(|(k, v)| (k, Some(v))));
    Ok(Some(headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_cache_overrides_merge_explicit_ttls() {
        let config = ModelConfig::parse(
            r#"{"providers":{"proxy":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol","promptCache":{"short":300,"long":3600}}],"modelOverrides":{"sol":{"promptCache":{"short":600}}}}}}"#,
            "memory",
        );
        let models =
            compose_models("proxy", &[], config.get_provider("proxy"), None).expect("compose");
        assert_eq!(
            models[0].prompt_cache,
            Some(maho_ai::model::ModelPromptCache {
                short: Some(600),
                long: Some(3600)
            })
        );
        assert_eq!(models[0].supports_assistant_prefill, None);
    }

    #[test]
    fn stream_registration_without_api_is_rejected() {
        let extension = ProviderConfigInput {
            stream_simple: Some(Arc::new(|_, _, _| AssistantMessageEventStream::assistant())),
            ..Default::default()
        };
        let config = ModelConfig::parse(r#"{"providers":{}}"#, "memory");
        assert!(compose_model_provider("p", None, &config, Some(&extension)).is_err());
    }
    #[test]
    fn custom_catalog_resolves_api_and_url() {
        let config = ModelConfig::parse(
            r#"{"providers":{"proxy":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"sol"}]}}}"#,
            "memory",
        );
        let models =
            compose_models("proxy", &[], config.get_provider("proxy"), None).expect("compose");
        assert_eq!(models[0].api, "openai-completions");
        assert_eq!(models[0].base_url, "http://localhost/v1");
    }
    #[test]
    fn user_overrides_apply_after_extension_catalog() {
        let config = ModelConfig::parse(
            r#"{"providers":{"proxy":{"models":[{"id":"sol"}],"modelOverrides":{"sol":{"maxTokens":42}}}}}"#,
            "memory",
        );
        let extension = ModelsJsonProvider {
            api: Some("openai-completions".into()),
            base_url: Some("http://localhost/v1".into()),
            models: Some(vec![ModelsJsonModel {
                id: "sol".into(),
                max_tokens: Some(100.0),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(
            compose_models(
                "proxy",
                &[],
                config.get_provider("proxy"),
                Some(&extension.into())
            )
            .expect("compose")[0]
                .max_tokens,
            42
        );
    }

    #[test]
    fn model_override_prompt_preset_reaches_request_config() {
        let config = ModelConfig::parse(
            r#"{"providers":{"proxy":{"models":[{"id":"sol","promptPreset":"gpt-5.4"}],"modelOverrides":{"sol":{"promptPreset":"glm-5.3"}}}}}"#,
            "memory",
        );
        assert_eq!(
            resolve_compatibility_request_config(&test_model(), config.get_provider("proxy"), None)
                .prompt_preset
                .as_deref(),
            Some("glm-5.3")
        );
        let config = ModelConfig::parse(
            r#"{"providers":{"proxy":{"models":[{"id":"sol","promptPreset":"gpt-5.4"}]}}}"#,
            "memory",
        );
        assert_eq!(
            resolve_compatibility_request_config(&test_model(), config.get_provider("proxy"), None)
                .prompt_preset
                .as_deref(),
            Some("gpt-5.4")
        );
    }

    struct CancelRecordingProvider {
        supports: bool,
        seen: std::sync::Mutex<
            Option<(
                maho_ai::types::DeferredHandle,
                Option<maho_ai::types::DeferredCancelOptions>,
            )>,
        >,
    }
    impl Provider for CancelRecordingProvider {
        fn id(&self) -> &str {
            "recording"
        }
        fn name(&self) -> &str {
            "recording"
        }
        fn get_models(&self) -> Vec<Model> {
            Vec::new()
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
        fn supports_cancel_deferred(&self) -> bool {
            self.supports
        }
        fn cancel_deferred<'a>(
            &'a self,
            _model: &'a Model,
            handle: &'a maho_ai::types::DeferredHandle,
            options: Option<maho_ai::types::DeferredCancelOptions>,
        ) -> maho_ai::types::BoxFuture<'a, Result<(), String>> {
            *self.seen.lock().expect("seen") = Some((handle.clone(), options.clone()));
            let supports = self.supports;
            Box::pin(async move {
                if supports {
                    Ok(())
                } else {
                    Err("API cannot cancel deferred responses".to_owned())
                }
            })
        }
    }
    fn composed(base: Option<Arc<dyn Provider>>) -> ComposedProvider {
        ComposedProvider {
            id: "proxy".into(),
            name: "proxy".into(),
            models: Vec::new(),
            nonchat_models: Vec::new(),
            refreshed_nonchat: Arc::new(std::sync::RwLock::new(None)),
            base,
            config: None,
            extension: None,
            refreshed: Arc::new(std::sync::RwLock::new(None)),
        }
    }
    fn test_model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "sol", "name": "sol", "api": "openai-completions", "provider": "proxy", "baseUrl": "http://localhost/v1",
            "reasoning": false, "input": [], "contextWindow": 128000, "maxTokens": 4096,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        })).expect("model")
    }

    #[tokio::test]
    async fn composed_provider_forwards_cancellation_and_preserves_unsupported() {
        let base = Arc::new(CancelRecordingProvider {
            supports: true,
            seen: Default::default(),
        });
        let provider = composed(Some(base.clone()));
        assert!(provider.supports_cancel_deferred());
        let model = test_model();
        let handle = maho_ai::types::DeferredHandle {
            provider: "proxy".into(),
            model_id: "sol".into(),
            api: "openai-completions".into(),
            id: "handle".into(),
            expires_at: None,
            poll_after_ms: None,
            data: None,
        };
        let options = maho_ai::types::DeferredCancelOptions {
            api_key: Some("resolved-key".into()),
            ..Default::default()
        };
        provider
            .cancel_deferred(&model, &handle, Some(options.clone()))
            .await
            .expect("forwards to the base provider");
        let (seen_handle, seen_options) =
            base.seen.lock().expect("seen").clone().expect("recorded");
        assert_eq!(seen_handle.id, "handle");
        assert_eq!(
            seen_options.and_then(|options| options.api_key).as_deref(),
            Some("resolved-key")
        );

        let unsupported = composed(None);
        assert!(!unsupported.supports_cancel_deferred());
        assert_eq!(
            unsupported
                .cancel_deferred(&model, &handle, None)
                .await
                .unwrap_err(),
            "API cannot cancel deferred responses"
        );

        let incapable_base = Arc::new(CancelRecordingProvider {
            supports: false,
            seen: Default::default(),
        });
        assert!(!composed(Some(incapable_base)).supports_cancel_deferred());
    }

    struct HermesBase(std::sync::Mutex<Option<Context>>);
    impl Provider for HermesBase {
        fn id(&self) -> &str {
            "proxy"
        }
        fn name(&self) -> &str {
            "proxy"
        }
        fn get_models(&self) -> Vec<Model> {
            vec![test_model()]
        }
        fn stream(
            &self,
            model: &Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            use maho_ai::types::{
                AssistantMessageEvent, ContentBlock, DoneReason, StopReason, TextContent,
            };
            *self.0.lock().expect("capture") = Some(context.clone());
            let text =
                r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool_call>"#;
            let stream = AssistantMessageEventStream::assistant();
            let mut message = maho_ai::utils::lazy::setup_error_message(model, "");
            message.error_message = None;
            message.stop_reason = StopReason::Stop;
            let start = message.clone();
            message.content = vec![ContentBlock::Text(TextContent {
                text: text.into(),
                ..Default::default()
            })];
            stream.push(AssistantMessageEvent::Start {
                partial: start.clone(),
            });
            stream.push(AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: start,
            });
            stream.push(AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: text.into(),
                partial: message.clone(),
            });
            stream.push(AssistantMessageEvent::TextEnd {
                content_index: 0,
                content: text.into(),
                partial: message.clone(),
            });
            stream.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message,
            });
            stream
        }
        fn stream_simple(
            &self,
            model: &Model,
            context: &Context,
            options: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream {
            self.stream(model, context, options.map(|o| o.stream))
        }
    }

    #[tokio::test]
    async fn tool_call_format_models_get_a_rewritten_context_and_parsed_tool_calls() {
        let base = Arc::new(HermesBase(Default::default()));
        let provider = composed(Some(base.clone()));
        let mut model = test_model();
        model.compat = Some(ModelCompat(Map::from_iter([(
            "toolCallFormat".to_owned(),
            serde_json::json!("hermes"),
        )])));
        let tool = maho_ai::types::Tool {
            name: "get_weather".into(),
            description: "d".into(),
            parameters: serde_json::json!({"type": "object"}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        };
        let context = Context {
            active_tool_names: None,
            system_prompt: Some("base".into()),
            messages: Vec::new(),
            tools: Some(vec![tool]),
        };
        let message = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.stream_simple(&model, &context, None).result(),
        )
        .await
        .expect("bounded stream")
        .expect("result");
        let sent = base
            .0
            .lock()
            .expect("capture")
            .clone()
            .expect("base provider called");
        assert!(sent.tools.is_none());
        assert!(
            sent.system_prompt
                .as_deref()
                .is_some_and(|prompt| prompt.contains("<tool_call>") && prompt.ends_with("base"))
        );
        assert_eq!(message.stop_reason, maho_ai::types::StopReason::ToolUse);
        assert!(message.content.iter().any(|block| matches!(block, maho_ai::types::ContentBlock::ToolCall(call) if call.name == "get_weather")));
    }
}

#[cfg(test)]
mod parity_cost_tests {
    use super::*;
    #[test]
    fn custom_prices_are_unknown_except_local_ollama_and_remain_visible() {
        let definition=ModelsJsonModel { id:"custom".into(), ..Default::default() };
        let config=ModelsJsonProvider { api:Some("openai-completions".into()), base_url:Some("http://localhost".into()), models:Some(vec![definition]), hide_free:Some(true), ..Default::default() };
        let models=compose_models("remote",&[],Some(&config),None).expect("models"); assert_eq!(models.len(),1); assert_eq!(models[0].cost.unknown, Some(true));
        let local=compose_models("ollama",&[],Some(&config),None).expect("local"); assert!(local.is_empty());
    }
}
