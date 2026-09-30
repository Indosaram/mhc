//! Port of senpi packages/ai/src/providers/radius.ts.

use crate::models::{
    ModelsError, ModelsErrorCode, ModelsPublication, Provider, RefreshModelsContext,
};
use crate::models_store::ModelsStoreEntry;
use crate::providers::builtin_api_streams;
use crate::providers::radius_config::{
    DEFAULT_RADIUS_GATEWAY, get_radius_models, get_radius_models_from_config, load_radius_gateway_config,
    normalize_radius_gateway_url,
};
use crate::types::{AssistantMessageEventStream, BoxFuture, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions};
use crate::utils::diagnostics::now_ms;
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct RadiusProviderOptions {
    pub id: Option<String>,
    pub name: Option<String>,
    pub gateway: Option<String>,
    pub fetch: Option<reqwest::Client>,
}

struct RadiusProvider {
    id: String,
    name: String,
    gateway: String,
    models: Arc<Mutex<Vec<Model>>>,
    streams: Arc<dyn ProviderStreams>,
    fetch: reqwest::Client,
}

impl Provider for RadiusProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    fn supports_refresh(&self) -> bool {
        true
    }

    fn refresh_models<'a>(&'a self, context: RefreshModelsContext) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async move {
            let stored = context.stored.clone();
            if let Some(stored) = &stored {
                let restored: Vec<Model> =
                    stored.models.iter().filter(|model| model.provider == self.id).cloned().collect();
                let models = self.models.clone();
                let published = context
                    .publish(ModelsPublication {
                        persist: None,
                        update: Some(Box::new(move || {
                            *models.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = restored;
                        })),
                    })
                    .await
                    .map_err(|reason| ModelsError::new(ModelsErrorCode::ModelSource, reason.message))?;
                if !published {
                    return Ok(());
                }
            }

            // Import catalogs cached by the pre-ModelsStore Radius implementation.
            if stored.is_none() && context.credential.as_ref().and_then(|c| c.get("type")).and_then(Value::as_str) == Some("oauth") {
                let legacy = get_radius_models(&self.id, context.credential.as_ref());
                if !legacy.is_empty() {
                    let models = self.models.clone();
                    let published = context
                        .publish(ModelsPublication {
                            persist: Some(Some(ModelsStoreEntry {
                                models: legacy.clone(),
                                checked_at: Some(now_ms()),
                                ..ModelsStoreEntry::default()
                            })),
                            update: Some(Box::new(move || {
                                *models.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = legacy;
                            })),
                        })
                        .await
                        .map_err(|reason| ModelsError::new(ModelsErrorCode::ModelSource, reason.message))?;
                    if !published {
                        return Ok(());
                    }
                }
            }

            if !context.allow_network || context.signal.aborted() {
                return Ok(());
            }
            let credential = context.credential.as_ref();
            let api_key = credential
                .and_then(|credential| {
                    if credential.get("type").and_then(Value::as_str) == Some("oauth") {
                        credential.get("access")
                    } else {
                        credential.get("key")
                    }
                })
                .and_then(Value::as_str);
            let config = load_radius_gateway_config(&self.gateway, api_key, &context.signal, &self.fetch)
                .await
                .map_err(|message| ModelsError::new(ModelsErrorCode::ModelSource, message))?;
            if context.signal.aborted() {
                return Ok(());
            }
            let refreshed = get_radius_models_from_config(&self.id, &config);
            let models = self.models.clone();
            let _ = context
                .publish(ModelsPublication {
                    persist: Some(Some(ModelsStoreEntry {
                        models: refreshed.clone(),
                        checked_at: Some(now_ms()),
                        ..ModelsStoreEntry::default()
                    })),
                    update: Some(Box::new(move || {
                        *models.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = refreshed;
                    })),
                })
                .await;
            Ok(())
        })
    }

    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        self.streams.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.streams.stream_simple(model, context, options)
    }
}

/// Radius gateway provider with a persisted, dynamically refreshed catalog.
pub fn radius_provider(options: Option<RadiusProviderOptions>) -> Arc<dyn Provider> {
    let options = options.unwrap_or_default();
    let id = options.id.clone().unwrap_or_else(|| "radius".to_owned());
    let name = options.name.clone().unwrap_or_else(|| "Radius".to_owned());
    let gateway = normalize_radius_gateway_url(options.gateway.as_deref().unwrap_or(DEFAULT_RADIUS_GATEWAY));
    Arc::new(RadiusProvider {
        models: Arc::new(Mutex::new(get_radius_models(&id, None))),
        id,
        name,
        gateway,
        streams: builtin_api_streams("pi-messages"),
        fetch: options.fetch.clone().unwrap_or_default(),
    })
}
