//! Port of senpi packages/ai/src/api/devin-agent/discovery.ts.
//!
//! Cascade model discovery. `GetCliModelConfigs` is credential-scoped: it reports the models the
//! signed-in account may actually use, which differs per plan and per rollout. A failed or empty
//! response therefore returns `None` so callers KEEP their static seed instead of publishing an
//! empty catalog.

use std::collections::HashSet;
use std::time::Duration;

use crate::api::devin_agent::r#gen::cascade_pb::{
    ClientModelConfig, DisplayOption, GetCliModelConfigsRequest, GetCliModelConfigsResponse, ModelDimensionKind,
};
use crate::api::devin_agent::metadata::devin_discovery_metadata;
use crate::api::devin_agent::paths::{DEVIN_CLI_MODEL_CONFIGS_PATH, DEVIN_DEFAULT_BASE_URL};
use crate::api::devin_agent::unary::{post_devin_unary, DevinUnaryInput};
use crate::types::{Model, ModelCostRates};

const DISCOVERY_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
const DEFAULT_MAX_TOKENS: u64 = 64_000;

/// Slots requested for parity with the native client but never surfaced as models.
fn is_internal_display(display: DisplayOption) -> bool {
    matches!(display, DisplayOption::QuickReview | DisplayOption::InternalDefault)
}

/// Lanes whose configs advertise image support while the backend silently drops
/// `ChatMessagePrompt.images` (verified live on SWE-1.6 and SWE-1.6 Fast).
const IMAGE_BLIND_UIDS: [&str; 2] = ["swe-1-6", "swe-1-6-fast"];

/// `DevinDiscoveryOptions`.
pub struct DevinDiscoveryOptions<'a> {
    pub api_key: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub timeout_ms: Option<u64>,
    pub signal: Option<&'a crate::utils::abort::AbortSignal>,
    pub client: &'a reqwest::Client,
}

/// `fetchDevinModels`: the account's models, or `None` when the call fails or reports none.
pub async fn fetch_devin_models(options: &DevinDiscoveryOptions<'_>) -> Option<Vec<Model>> {
    let base_url = options.base_url.unwrap_or(DEVIN_DEFAULT_BASE_URL).trim_end_matches('/').to_owned();
    let request = GetCliModelConfigsRequest { metadata: Some(devin_discovery_metadata(options.api_key)) };
    let call = post_devin_unary::<_, GetCliModelConfigsResponse>(DevinUnaryInput {
        client: options.client,
        base_url: &base_url,
        path: DEVIN_CLI_MODEL_CONFIGS_PATH,
        request: &request,
        signal: options.signal,
    });
    let response = tokio::time::timeout(Duration::from_millis(options.timeout_ms.unwrap_or(DISCOVERY_TIMEOUT_MS)), call)
        .await
        .ok()?
        .ok()?;
    let models = normalize_devin_models(&response.client_model_configs, &base_url);
    (!models.is_empty()).then_some(models)
}

/// `normalizeDevinModels`.
pub fn normalize_devin_models(configs: &[ClientModelConfig], base_url: &str) -> Vec<Model> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut models: Vec<Model> = Vec::new();
    for config in configs {
        if config.disabled {
            continue;
        }
        let display = config
            .model_info
            .as_ref()
            .and_then(|info| DisplayOption::try_from(info.display_option).ok())
            .unwrap_or(DisplayOption::Unspecified);
        if is_internal_display(display) {
            continue;
        }
        let uid = config.model_uid.trim().to_owned();
        if uid.is_empty() || !seen.insert(uid.clone()) {
            continue;
        }
        let is_router = display == DisplayOption::ModelRouter
            || config.model_info.as_ref().is_some_and(|info| info.is_model_router);
        models.push(to_model(config, &uid, base_url, is_router));
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn to_model(config: &ClientModelConfig, uid: &str, base_url: &str, is_router: bool) -> Model {
    let features = config.model_info.as_ref().and_then(|info| info.model_features.as_ref());
    let advertises_images = features.map(|features| features.supports_images).unwrap_or(config.supports_images);
    let supports_images = advertises_images && !IMAGE_BLIND_UIDS.contains(&uid);
    let max_output_tokens = config.model_info.as_ref().map(|info| info.max_output_tokens).unwrap_or(0);
    let supports_parallel_tool_calls = features.is_some_and(|features| features.supports_parallel_tool_calls);
    let compat = (is_router || supports_parallel_tool_calls).then(|| {
        let mut compat = serde_json::Map::new();
        if is_router {
            compat.insert(String::from("modelRouter"), serde_json::Value::Bool(true));
        }
        if supports_parallel_tool_calls {
            compat.insert(String::from("supportsParallelToolCalls"), serde_json::Value::Bool(true));
        }
        crate::model::ModelCompat(compat)
    });
    let label = config.label.trim();
    Model {
        id: uid.to_owned(),
        name: if label.is_empty() { uid.to_owned() } else { label.to_owned() },
        api: String::from("devin-agent"),
        provider: String::from("devin"),
        base_url: base_url.to_owned(),
        // The catalog's supportsThinking flag (and effort-sounding labels) describe whether the lane
        // produces thinking output, not whether the client may pick a level: Cascade has no
        // request-side thinking field, so the effort baked into the lane uid is the only control and
        // a generic level is never forwarded.
        reasoning: false,
        thinking_level_map: None,
        input: if supports_images {
            vec![crate::types::InputModality::Text, crate::types::InputModality::Image]
        } else {
            vec![crate::types::InputModality::Text]
        },
        cost: cost_of(config),
        context_window: if config.max_tokens > 0 { config.max_tokens as u64 } else { DEFAULT_CONTEXT_WINDOW },
        max_tokens: if max_output_tokens > 0 { max_output_tokens as u64 } else { DEFAULT_MAX_TOKENS },
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat,
    }
}

/// Per-million rates from the cost dimensions; Devin bills cache writes at the input rate, so
/// cacheWrite stays 0.
fn cost_of(config: &ClientModelConfig) -> crate::types::ModelCost {
    let mut cost = ModelCostRates::default();
    for dimension in &config.model_dimensions {
        let Ok(kind) = ModelDimensionKind::try_from(dimension.kind) else { continue };
        if !matches!(kind, ModelDimensionKind::Cost | ModelDimensionKind::CostFuzzy) {
            continue;
        }
        // Dimension values arrive as protobuf floats (0.1 decodes as 0.10000000149...); round at
        // sub-cent precision.
        let per_million = ((dimension.value as f64) * 1_000_000.0 / denominator_tokens(&dimension.denominator) * 1e6)
            .round()
            / 1e6;
        match dimension.label.trim().to_lowercase().as_str() {
            "input" => cost.input = per_million,
            "cached input" => cost.cache_read = per_million,
            "output" => cost.output = per_million,
            _ => {}
        }
    }
    crate::types::ModelCost { input: cost.input, output: cost.output, cache_read: cost.cache_read, cache_write: cost.cache_write, ..Default::default() }
}

fn denominator_tokens(denominator: &str) -> f64 {
    let Some(captures) = regex::Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*([kmb])?").ok().and_then(|re| {
        re.captures(denominator).map(|captures| {
            (captures.get(1).map(|m| m.as_str().to_owned()), captures.get(2).map(|m| m.as_str().to_lowercase()))
        })
    }) else {
        return 1_000_000.0;
    };
    let (Some(number), scale) = captures else { return 1_000_000.0 };
    let scale = match scale.as_deref() {
        Some("k") => 1_000.0,
        Some("m") => 1_000_000.0,
        Some("b") => 1_000_000_000.0,
        _ => 1.0,
    };
    let tokens = number.parse::<f64>().unwrap_or(0.0) * scale;
    if tokens > 0.0 { tokens } else { 1_000_000.0 }
}
