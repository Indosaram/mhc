//! Port of senpi packages/ai/src/providers/opengateway-refresh.ts (senpi v2026.10.27-2).
//!
//! Runtime refresh for the OpenGateway provider. A successful gateway listing is authoritative for
//! availability; the shipped catalog (generated from the gateway plus models.dev) supplies metadata
//! and is the offline fallback:
//!
//! - a servable model the shipped catalog lacks is added, built from its shipped serving-tier base
//!   when there is one, priced from the gateway price table;
//! - a shipped model the gateway retired, or no longer lists, is removed;
//! - shipped rows keep their generated metadata (input caps, thinking maps, prices). Correcting
//!   those is the scheduled catalog regeneration's job.
//!
//! Any fetch or parse failure keeps the last good list and surfaces as a refresh error.

use super::opengateway_catalog::{
    OPENGATEWAY_BASE_URL, OPENGATEWAY_MODELS_URL, OPENGATEWAY_PRICES_URL, OpenGatewayListedModel,
    OpenGatewayPriceTable, is_servable_chat_model, open_gateway_price, parse_open_gateway_listing,
    parse_open_gateway_price_table, serving_tier_base, with_gateway_price,
};
use crate::model::ModelCompat;
use crate::models::{ModelsError, ModelsErrorCode, ModelsPublication, RefreshModelsContext};
use crate::models_store::ModelsStoreEntry;
use crate::types::{InputModality, Model, ModelCost};
use crate::utils::abort::AbortSignal;
use crate::utils::diagnostics::now_ms;
use crate::utils::oauth_refresh_error::OAuthTokenEndpointError;
use crate::utils::openai_input_cap::apply_openai_input_cap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const OPENGATEWAY_REFRESH_INTERVAL_MS: i64 = 60 * 60 * 1000;
const REQUEST_TIMEOUT_MS: u64 = 15_000;
/// Max output for an added model the gateway publishes no limit for and that has no shipped base.
const UNPUBLISHED_MAX_OUTPUT_TOKENS: u64 = 32_768;

fn zero_cost() -> ModelCost {
    ModelCost::default()
}

fn added_model(
    item: &OpenGatewayListedModel,
    shipped_by_id: &HashMap<&str, &Model>,
    prices: &OpenGatewayPriceTable,
) -> Option<Model> {
    let price = open_gateway_price(item, prices)?;
    // An unpriced model would bill as free in usage accounting; wait for the regeneration instead.
    if price.input.is_none() || price.output.is_none() {
        return None;
    }
    let tier = serving_tier_base(&item.id);
    let base = tier
        .as_ref()
        .and_then(|(base_id, _)| shipped_by_id.get(base_id.as_str()).copied());
    let context_window = item
        .context_window
        .or(base.map(|base| base.context_window))?;
    let mut model = match base {
        Some(base) => {
            let mut template = base.clone();
            template.name = format!(
                "{} {}",
                base.name,
                tier.as_ref().map_or("", |(_, label)| label)
            );
            template
        }
        None => {
            let mut compat = Map::new();
            compat.insert("supportsDeveloperRole".into(), Value::Bool(false));
            serde_json::from_value::<Model>(serde_json::json!({
                "id": item.id, "name": item.id, "api": "openai-completions", "provider": "opengateway",
                "baseUrl": OPENGATEWAY_BASE_URL, "reasoning": false, "input": ["text"],
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                "contextWindow": context_window, "maxTokens": UNPUBLISHED_MAX_OUTPUT_TOKENS
            }))
            .map(|mut model| {
                model.compat = Some(ModelCompat(compat));
                model
            })
            .ok()?
        }
    };
    let template_max_tokens = model.max_tokens;
    model.id = item.id.clone();
    model.input = if item.input_modalities.iter().any(|m| m == "image") {
        vec![InputModality::Text, InputModality::Image]
    } else {
        vec![InputModality::Text]
    };
    model.cost = with_gateway_price(&zero_cost(), Some(price));
    model.context_window = context_window;
    model.max_tokens = item
        .max_output_tokens
        .unwrap_or(template_max_tokens)
        .min(context_window);
    apply_openai_input_cap(&mut model);
    Some(model)
}

/// Servable gateway models the shipped catalog lacks; only these need the price table.
pub fn unshipped_servable_models<'a>(
    shipped: &[Model],
    listing: &'a [OpenGatewayListedModel],
) -> Vec<&'a OpenGatewayListedModel> {
    let shipped_ids: HashSet<&str> = shipped.iter().map(|model| model.id.as_str()).collect();
    listing
        .iter()
        .filter(|item| is_servable_chat_model(item) && !shipped_ids.contains(item.id.as_str()))
        .collect()
}

pub fn overlay_open_gateway_catalog(
    shipped: &[Model],
    listing: &[OpenGatewayListedModel],
    prices: &OpenGatewayPriceTable,
) -> Vec<Model> {
    let servable: HashSet<&str> = listing
        .iter()
        .filter(|item| is_servable_chat_model(item))
        .map(|item| item.id.as_str())
        .collect();
    let shipped_by_id: HashMap<&str, &Model> = shipped
        .iter()
        .map(|model| (model.id.as_str(), model))
        .collect();
    let added: Vec<Model> = unshipped_servable_models(shipped, listing)
        .into_iter()
        .filter_map(|item| added_model(item, &shipped_by_id, prices))
        .collect();
    shipped
        .iter()
        .filter(|model| servable.contains(model.id.as_str()))
        .cloned()
        .chain(added)
        .collect()
}

async fn fetch_json(
    client: &reqwest::Client,
    url: &str,
    signal: &AbortSignal,
) -> Result<Value, String> {
    let request = client
        .get(url)
        .header("accept", "application/json")
        .timeout(Duration::from_millis(REQUEST_TIMEOUT_MS))
        .send();
    let response = tokio::select! {
        biased;
        () = signal.cancelled() => return Err("OpenGateway catalog request aborted".into()),
        response = request => response.map_err(|error| error.to_string())?,
    };
    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(OAuthTokenEndpointError::new(
            format!("OpenGateway catalog request failed: {url} returned {status}"),
            status,
        )
        .to_string());
    }
    tokio::select! {
        biased;
        () = signal.cancelled() => Err("OpenGateway catalog request aborted".into()),
        body = response.json::<Value>() => body.map_err(|error| error.to_string()),
    }
}

async fn fetch_refreshed_catalog(
    client: &reqwest::Client,
    shipped: &[Model],
    signal: &AbortSignal,
) -> Result<Vec<Model>, String> {
    let listing =
        parse_open_gateway_listing(&fetch_json(client, OPENGATEWAY_MODELS_URL, signal).await?)?;
    let prices = if unshipped_servable_models(shipped, &listing).is_empty() {
        Vec::new()
    } else {
        parse_open_gateway_price_table(&fetch_json(client, OPENGATEWAY_PRICES_URL, signal).await?)?
    };
    let refreshed = overlay_open_gateway_catalog(shipped, &listing, &prices);
    if refreshed.is_empty() {
        return Err(
            "OpenGateway listing has no servable chat models; keeping the last good catalog".into(),
        );
    }
    Ok(refreshed)
}

fn is_open_gateway_chat_model(model: &Model) -> bool {
    model.provider == "opengateway" && model.api == "openai-completions"
}

/// Catalog state for the provider: `models()` is synchronous, `refresh()` restores the persisted
/// list and revalidates it against the gateway at most hourly. A persisted list older than the
/// shipped catalog is ignored, so an upgrade never resurrects metadata the new release corrected.
#[derive(Clone)]
pub struct OpenGatewayCatalog {
    shipped: Arc<Vec<Model>>,
    shipped_generated_at: Option<i64>,
    current: Arc<Mutex<Vec<Model>>>,
    client: reqwest::Client,
}

pub fn create_open_gateway_catalog(
    shipped: Vec<Model>,
    shipped_generated_at: Option<i64>,
) -> OpenGatewayCatalog {
    OpenGatewayCatalog {
        current: Arc::new(Mutex::new(shipped.clone())),
        shipped: Arc::new(shipped),
        shipped_generated_at,
        client: reqwest::Client::new(),
    }
}

fn source_error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::ModelSource, message)
}

impl OpenGatewayCatalog {
    pub fn models(&self) -> Vec<Model> {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn update(&self, models: Vec<Model>) -> Box<dyn FnOnce() + Send> {
        let current = self.current.clone();
        Box::new(move || {
            *current
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = models;
        })
    }

    pub async fn refresh(&self, context: RefreshModelsContext) -> Result<(), ModelsError> {
        let stored = context.stored.clone();
        let stored_checked_at = stored.as_ref().and_then(|stored| stored.checked_at);
        let usable = stored_checked_at.is_some_and(|checked_at| {
            self.shipped_generated_at
                .is_none_or(|generated_at| checked_at > generated_at)
        });
        if usable && let Some(stored) = &stored {
            let restored: Vec<Model> = stored
                .models
                .iter()
                .filter(|model| is_open_gateway_chat_model(model))
                .cloned()
                .collect();
            let published = context
                .publish(ModelsPublication {
                    persist: None,
                    update: Some(self.update(restored)),
                })
                .await
                .map_err(|reason| source_error(reason.message))?;
            if !published {
                return Ok(());
            }
        }
        if !context.allow_network || context.signal.aborted() {
            return Ok(());
        }
        let age = now_ms() - stored_checked_at.unwrap_or(0);
        if context.force != Some(true)
            && usable
            && (0..OPENGATEWAY_REFRESH_INTERVAL_MS).contains(&age)
        {
            return Ok(());
        }
        let refreshed = fetch_refreshed_catalog(&self.client, &self.shipped, &context.signal)
            .await
            .map_err(source_error)?;
        if context.signal.aborted() {
            return Ok(());
        }
        context
            .publish(ModelsPublication {
                persist: Some(Some(ModelsStoreEntry {
                    models: refreshed.clone(),
                    checked_at: Some(now_ms()),
                    ..ModelsStoreEntry::default()
                })),
                update: Some(self.update(refreshed)),
            })
            .await
            .map_err(|reason| source_error(reason.message))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "opengateway_refresh_tests.rs"]
mod tests;
