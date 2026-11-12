use std::sync::Arc;

use maho_ai::types::{InputModality, ModelCost};
use maho_ext_api::ProviderModelConfig;

pub type CcResult<T> = Result<T, String>;

pub const DEFAULT_API_BASE: &str = "https://api.commandcode.ai";
pub const DEFAULT_PROVIDER_API_BASE: &str = "https://api.commandcode.ai/provider/v1";
pub const DEFAULT_MODELS_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 65_536;
pub const MODEL_CACHE_VERSION: u32 = 1;
/// How old the cached catalog may be before a launch refreshes it in the background. A fresh
/// cache skips the network entirely, so startup never pays for a fetch it does not need.
pub const MODEL_CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
pub const GENERATE_MAX_TOKENS_CAP: u64 = 64_000;
pub const OVERFLOW_PREFIX: &str = "context_length_exceeded:";
pub const PROVIDER_ID: &str = "commandcode";
pub const SYNTHETIC_API: &str = "commandcode-custom";
pub const PROVIDER_NAME: &str = "Command Code";
pub const STUDIO_BASE_URL: &str = "https://commandcode.ai";
pub const QUOTA_TIMEOUT_MS: u64 = 15_000;
pub const AUTH_TIMEOUT_MS: u64 = 120_000;
pub const QUOTA_DETAIL_URL: &str = "https://commandcode.ai/usage";

pub fn zdr_headers() -> Option<std::collections::BTreeMap<String, String>> {
    let enabled = std::env::var("CMD_ZDR").ok().as_deref() == Some("1")
        || std::env::var("COMMANDCODE_ZDR").ok().as_deref() == Some("1");
    enabled.then(|| std::collections::BTreeMap::from([("x-cmd-zdr".to_owned(), "1".to_owned())]))
}

/// One model as discovered from the live catalog or restored from cache.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub context_window: u64,
    pub max_tokens: u64,
    pub reasoning: bool,
    pub input: Vec<InputModality>,
    /// `"anthropic-messages"` or `"openai-completions"`.
    pub api: String,
    pub base_url: String,
}

impl CatalogModel {
    pub fn into_provider_config(self) -> ProviderModelConfig {
        let effort_map = crate::catalog::thinking_level_map(&self.id);
        let cost = crate::pricing::model_cost(&self.id);
        crate::contract::provider_model_config(self, cost, effort_map)
    }
}

/// Build the extension-facing model config. Kept here so every lane agrees on the field mapping.
pub fn provider_model_config(
    model: CatalogModel,
    cost: ModelCost,
    thinking_level_map: maho_ai::types::ThinkingLevelMap,
) -> ProviderModelConfig {
    ProviderModelConfig {
        model_type: None,
        output: None,
        image_cost: None,
        input_limits: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        default_thinking_level: None,
        id: model.id.clone(),
        name: model.name,
        upstream_model_id: None,
        api: Some(SYNTHETIC_API.to_owned()),
        base_url: Some(model.base_url),
        reasoning: model.reasoning,
        recover_text_tool_calls: None,
        thinking_level_map: Some(thinking_level_map),
        input: model.input,
        cost,
        context_window: model.context_window,
        max_tokens: model.max_tokens,
        headers: None,
        extra_body: None,
        compat: Some(model_compat(&model.id, model.reasoning)),
    }
}

/// compat flags the Command Code gateway needs, keyed by the model's wire protocol.
pub fn model_compat(model_id: &str, reasoning: bool) -> maho_ai::model::ModelCompat {
    let value = if crate::catalog::api_for_model_id(model_id) == "anthropic-messages" {
        let mut object = serde_json::json!({
            "supportsEagerToolInputStreaming": false,
            "supportsLongCacheRetention": false,
            "supportsCacheControlOnTools": false,
            "supportsToolReferences": false,
        });
        if reasoning {
            object["forceAdaptiveThinking"] = serde_json::Value::Bool(true);
        }
        object
    } else {
        serde_json::json!({
            "supportsStore": false,
            "supportsDeveloperRole": false,
            "supportsReasoningEffort": crate::catalog::efforts_for_model(model_id).is_some(),
            "maxTokensField": "max_tokens",
        })
    };
    maho_ai::model::ModelCompat(value.as_object().cloned().unwrap_or_default())
}

/// Result of a catalog load, mirroring `LoadCommandCodeModelsResult` in the TypeScript source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogSource {
    Live,
    Cache,
    Empty,
}

impl CatalogSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            CatalogSource::Live => "live",
            CatalogSource::Cache => "cache",
            CatalogSource::Empty => "empty",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedCatalog {
    pub models: Vec<CatalogModel>,
    pub source: CatalogSource,
    pub warning: Option<String>,
}

/// Environment and filesystem inputs, injected so tests never touch the real environment.
#[derive(Clone, Debug)]
pub struct ProviderEnv {
    pub api_base: String,
    pub models_url: String,
    pub models_timeout_ms: u64,
    pub cache_path: std::path::PathBuf,
    pub cwd: std::path::PathBuf,
    pub home: std::path::PathBuf,
}

impl ProviderEnv {
    pub fn from_process() -> Self {
        let api_base = std::env::var("COMMANDCODE_API_BASE")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_PROVIDER_API_BASE.to_owned());
        let models_url = std::env::var("COMMANDCODE_MODELS_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("{}/models", api_base.trim_end_matches('/')));
        let models_timeout_ms = models_timeout_ms_from(
            std::env::var("COMMANDCODE_MODELS_TIMEOUT_MS")
                .ok()
                .as_deref(),
        );
        let agent_dir = std::path::PathBuf::from(maho_core::config::get_agent_dir());
        let cache_path = std::env::var("COMMANDCODE_MODELS_CACHE")
            .ok()
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| agent_dir.join("commandcode-models.json"));
        let home = std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| agent_dir.clone());
        let cwd = std::env::current_dir().unwrap_or(agent_dir);
        Self {
            api_base,
            models_url,
            models_timeout_ms,
            cache_path,
            cwd,
            home,
        }
    }

    /// The legacy base strips a trailing `/provider/v1`, as `legacyApiBase` does.
    pub fn legacy_api_base(&self) -> String {
        let trimmed = self.api_base.trim_end_matches('/');
        trimmed
            .strip_suffix("/provider/v1")
            .map(str::to_owned)
            .unwrap_or_else(|| trimmed.to_owned())
    }
}

/// `getModelsTimeoutMs`: a non-empty value must parse to a positive number, else the default.
pub fn models_timeout_ms_from(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MODELS_TIMEOUT_MS)
}

/// The base URL a model talks to: trailing slashes trimmed, and anthropic loses its `/v1`.
pub fn base_url_for_model(api_base: &str, api: &str) -> String {
    let normalized = api_base.trim_end_matches('/');
    if api != "anthropic-messages" {
        return normalized.to_owned();
    }
    normalized
        .strip_suffix("/v1")
        .map(str::to_owned)
        .unwrap_or_else(|| normalized.to_owned())
}

/// `projectSlugFromPath`: lowercase, drive letter stripped, non-alphanumerics collapsed to `-`.
pub fn project_slug(cwd: &std::path::Path) -> String {
    let raw = cwd.to_string_lossy().to_lowercase();
    let stripped = match raw.strip_prefix(|c: char| c.is_ascii_alphabetic()) {
        Some(rest) => rest.strip_prefix(':').unwrap_or(rest),
        None => raw.as_str(),
    };
    let slug = stripped
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        "project".to_owned()
    } else {
        slug
    }
}

/// `AbortSignal`-shaped cancellation shared by the transport, catalog, and quota paths.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<std::sync::atomic::AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
