//! Port of senpi packages/ai/src/providers/opengateway-catalog.ts (senpi v2026.10.27-2).
//!
//! OpenGateway's public catalog data, shared by the build-time generator and the runtime refresh
//! (`opengateway_refresh.rs`). Both endpoints are public and need no API key.
//!
//! - GET https://apis.opengateway.ai/v1/models lists every model the gateway serves: lifecycle
//!   status, modalities, endpoints, the provider routes in preference order, and (for most models)
//!   context window and max output.
//! - GET https://opengateway.ai/api/model-prices is the price table the gateway bills from, keyed
//!   per provider route, with the effective (discounted) per-token price and long-context tiers.

use crate::types::{ModelCost, ModelCostTier};
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;

pub const OPENGATEWAY_BASE_URL: &str = "https://apis.opengateway.ai/v1";
pub const OPENGATEWAY_MODELS_URL: &str = "https://apis.opengateway.ai/v1/models";
pub const OPENGATEWAY_PRICES_URL: &str = "https://opengateway.ai/api/model-prices";

const SERVING_TIER_SUFFIXES: &[(&str, &str)] = &[("-ultrafast", "Ultrafast")];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenGatewayListedModel {
    pub id: String,
    pub status: Option<String>,
    pub input_modalities: Vec<String>,
    pub endpoints: Vec<String>,
    /// Provider route ids in the gateway's preference order.
    pub routes: Vec<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

/// Published per-million-token prices; a field is `None` when the gateway does not publish it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenGatewayPrice {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub tiers: Vec<ModelCostTier>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriceRoute {
    pub route: String,
    pub model_id: String,
    pub price: OpenGatewayPrice,
}

pub type OpenGatewayPriceTable = Vec<PriceRoute>;

fn positive_integer(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(integer) = value.as_u64() {
        return (integer > 0).then_some(integer);
    }
    let float = value.as_f64()?;
    (float.fract() == 0.0 && float > 0.0 && float <= u64::MAX as f64).then_some(float as u64)
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_open_gateway_listing(value: &Value) -> Result<Vec<OpenGatewayListedModel>, String> {
    let data = value
        .as_object()
        .and_then(|object| object.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| "OpenGateway model listing is not a { data: [...] } list".to_owned())?;
    let mut models = Vec::new();
    for entry in data {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(id) = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| id.contains('/'))
        else {
            continue;
        };
        let modalities = entry.get("modalities").and_then(Value::as_object);
        let routes = entry
            .get("providers")
            .and_then(Value::as_array)
            .map(|routes| {
                routes
                    .iter()
                    .filter_map(|route| route.as_object()?.get("id")?.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        models.push(OpenGatewayListedModel {
            id: id.to_owned(),
            status: entry
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_owned),
            input_modalities: string_list(modalities.and_then(|m| m.get("input"))),
            endpoints: string_list(entry.get("endpoints")),
            routes,
            context_window: positive_integer(entry.get("context_window")),
            max_output_tokens: positive_integer(entry.get("max_output_tokens")),
        });
    }
    if models.is_empty() {
        return Err("OpenGateway model listing has no models".into());
    }
    Ok(models)
}

/// Chat-completions models that can still be called. Retired models are listed but rejected.
pub fn is_servable_chat_model(model: &OpenGatewayListedModel) -> bool {
    model
        .endpoints
        .iter()
        .any(|endpoint| endpoint == "chat_completions")
        && model.status.as_deref() != Some("retired")
}

/// `z-ai/glm-5.3-ultrafast` -> its base `z-ai/glm-5.3` plus the tier label, or `None`.
pub fn serving_tier_base(id: &str) -> Option<(String, &'static str)> {
    SERVING_TIER_SUFFIXES.iter().find_map(|(suffix, label)| {
        (id.ends_with(suffix) && id.len() > suffix.len())
            .then(|| (id[..id.len() - suffix.len()].to_owned(), *label))
    })
}

fn per_million(value: Option<&Value>) -> Option<f64> {
    let value = value?.as_f64()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    // Per-token decimals (2e-7) scale to float noise (0.19999999999999998); 12 significant digits
    // remove it, like `Number(x.toPrecision(12))`.
    format!("{:.11e}", value * 1_000_000.0).parse().ok()
}

static AXIS_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^AxisKey\(threshold=(\d+), tier=null\)$").expect("valid regex"));

fn parse_tiers(axis_prices: Option<&Value>) -> Vec<ModelCostTier> {
    let Some(axis_prices) = axis_prices.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut tiers = Vec::new();
    for (key, value) in axis_prices {
        // Keys look like "AxisKey(threshold=272000, tier=null)"; only plain context thresholds are
        // input tiers.
        let Some(threshold) = AXIS_KEY
            .captures(key)
            .and_then(|m| m[1].parse::<u64>().ok())
        else {
            continue;
        };
        let Some(value) = value.as_object() else {
            continue;
        };
        if value.get("isEmpty") == Some(&Value::Bool(true)) {
            continue;
        }
        let (Some(input), Some(output)) = (
            per_million(value.get("input")),
            per_million(value.get("output")),
        ) else {
            continue;
        };
        tiers.push(ModelCostTier {
            input_tokens_above: threshold,
            input,
            output,
            cache_read: per_million(value.get("cacheRead")).unwrap_or(0.0),
            cache_write: per_million(value.get("cacheCreation")).unwrap_or(0.0),
        });
    }
    tiers.sort_by_key(|tier| tier.input_tokens_above);
    tiers
}

fn is_price_route_entry(value: &Value) -> bool {
    value.as_object().is_some_and(|entry| {
        entry.get("provider").and_then(Value::as_str).is_some()
            && entry.get("modelOwner").and_then(Value::as_str).is_some()
            && entry.get("modelName").and_then(Value::as_str).is_some()
    })
}

/// Parse either the current metadata envelope or the older flat route table.
pub fn parse_open_gateway_price_table(value: &Value) -> Result<OpenGatewayPriceTable, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "OpenGateway price table is not an object".to_owned())?;
    // An envelope's `entries` holds the route table; a legacy flat table whose route key happens to
    // be `entries` holds one route there, and its `currency`/`unit` keys are routes, not metadata.
    let envelope = object
        .get("entries")
        .is_some_and(|entries| !is_price_route_entry(entries));
    if envelope {
        if let Some(currency) = object.get("currency")
            && currency.as_str() != Some("USD")
        {
            return Err(format!(
                "OpenGateway price table has unsupported currency: {} (expected USD)",
                js_string(currency)
            ));
        }
        if let Some(unit) = object.get("unit")
            && unit.as_str() != Some("USD_PER_TOKEN")
        {
            return Err(format!(
                "OpenGateway price table has unsupported unit: {} (expected USD_PER_TOKEN)",
                js_string(unit)
            ));
        }
    }
    let entries = if envelope { &object["entries"] } else { value };
    let entries = entries
        .as_object()
        .ok_or_else(|| "OpenGateway price table entries is not an object".to_owned())?;
    let mut routes = Vec::new();
    for entry in entries.values() {
        let Some(row) = entry.as_object() else {
            continue;
        };
        let (Some(provider), Some(owner), Some(model)) = (
            row.get("provider").and_then(Value::as_str),
            row.get("modelOwner").and_then(Value::as_str),
            row.get("modelName").and_then(Value::as_str),
        ) else {
            continue;
        };
        // The effective price already carries the gateway's discount; the list price is the fallback.
        let effective: &Map<String, Value> = row
            .get("pricing")
            .and_then(Value::as_object)
            .and_then(|pricing| pricing.get("current"))
            .and_then(Value::as_object)
            .and_then(|current| current.get("effectivePrice"))
            .and_then(Value::as_object)
            .unwrap_or(row);
        routes.push(PriceRoute {
            route: provider.to_owned(),
            model_id: format!("{owner}/{model}"),
            price: OpenGatewayPrice {
                input: per_million(effective.get("inputCostPerToken")),
                output: per_million(effective.get("outputCostPerToken")),
                cache_read: per_million(effective.get("cacheReadInputTokenCost")),
                cache_write: per_million(effective.get("cacheCreationInputTokenCost")),
                tiers: parse_tiers(effective.get("axisPrices")),
            },
        });
    }
    if routes.is_empty() {
        return Err("OpenGateway price table has no priced routes".into());
    }
    Ok(routes)
}

/// `String(value)` for the metadata values the error messages quote.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".into(),
        Value::Array(_) => value
            .as_array()
            .map(|items| items.iter().map(js_string).collect::<Vec<_>>().join(","))
            .unwrap_or_default(),
        Value::Object(_) => "[object Object]".into(),
        other => other.to_string(),
    }
}

/// The price billed for a listed model: its preferred route's price, else any route's.
pub fn open_gateway_price<'a>(
    model: &OpenGatewayListedModel,
    table: &'a [PriceRoute],
) -> Option<&'a OpenGatewayPrice> {
    let candidates: Vec<&PriceRoute> = table
        .iter()
        .filter(|entry| entry.model_id == model.id)
        .collect();
    for route in &model.routes {
        if let Some(preferred) = candidates.iter().find(|entry| &entry.route == route) {
            return Some(&preferred.price);
        }
    }
    candidates.first().map(|entry| &entry.price)
}

/// Overlay published gateway prices on a fallback cost; unpublished fields keep the fallback.
pub fn with_gateway_price(fallback: &ModelCost, price: Option<&OpenGatewayPrice>) -> ModelCost {
    let Some(price) = price else {
        return fallback.clone();
    };
    let mut cost = ModelCost {
        input: price.input.unwrap_or(fallback.input),
        output: price.output.unwrap_or(fallback.output),
        cache_read: price.cache_read.unwrap_or(fallback.cache_read),
        cache_write: price.cache_write.unwrap_or(fallback.cache_write),
        ..ModelCost::default()
    };
    // A published route price owns its tiers: no gateway tiers means flat billing.
    if price.input.is_some() && price.output.is_some() {
        if !price.tiers.is_empty() {
            cost.tiers = Some(price.tiers.clone());
        }
    } else if fallback.tiers.is_some() {
        cost.tiers = fallback.tiers.clone();
    }
    cost
}

#[cfg(test)]
#[path = "opengateway_catalog_tests.rs"]
mod tests;
