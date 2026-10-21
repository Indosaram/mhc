//! Port of senpi packages/ai/src/providers/bai.ts.

use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::RefreshModelsContext;
use crate::models::{
    CreateProviderOptions, FetchModels, ModelsError, ModelsErrorCode, Provider, ProviderApi,
    RestoreModels, create_provider,
};
use crate::providers::{bai_stream::bai_responses_streams, builtin_api_streams};
use crate::types::Model;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

pub const BAI_BASE_URL: &str = "https://api.b.ai/v1";

#[derive(Clone, Default)]
pub struct BaiProviderOptions {
    pub base_url: Option<String>,
    pub models: Option<Vec<Model>>,
    pub fetch: Option<reqwest::Client>,
}

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_owned()
}

fn model_id_aliases(id: &str) -> Vec<String> {
    let dashed = id.replace('.', "-");
    if dashed == id {
        vec![id.to_owned()]
    } else {
        vec![id.to_owned(), dashed]
    }
}

fn index_catalog(catalog: &[Model]) -> HashMap<String, Model> {
    let mut index = HashMap::new();
    for model in catalog {
        for alias in model_id_aliases(&model.id) {
            index.entry(alias).or_insert_with(|| model.clone());
        }
    }
    index
}

fn remap_bai_models(models: &[Model], catalog_by_id: &HashMap<String, Model>) -> Vec<Model> {
    models
        .iter()
        .filter_map(|model| catalog_by_id.get(&model.id).cloned())
        .collect()
}

async fn fetch_bai_models(
    base_url: &str,
    catalog_by_id: &HashMap<String, Model>,
    fetch: &reqwest::Client,
    credential: Option<crate::models::Credential>,
    signal: &crate::utils::abort::AbortSignal,
) -> Result<Vec<Model>, ModelsError> {
    let api_key = credential
        .as_ref()
        .filter(|credential| credential.get("type").and_then(Value::as_str) == Some("api_key"))
        .and_then(|credential| credential.get("key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let Some(api_key) = api_key else {
        return Ok(Vec::new());
    };

    let request = fetch
        .get(format!("{base_url}/models"))
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {api_key}"));
    let response = tokio::select! {
        biased;
        _ = signal.cancelled() => return Err(ModelsError::new(ModelsErrorCode::ModelSource, "B.AI model catalog request aborted")),
        response = request.send() => response,
    }
        .map_err(|error| ModelsError::new(ModelsErrorCode::ModelSource, format!("Could not load B.AI model catalog: {error}")))?;
    if !response.status().is_success() {
        return Err(ModelsError::new(
            ModelsErrorCode::ModelSource,
            format!(
                "Could not load B.AI model catalog: {}",
                response.status().as_u16()
            ),
        ));
    }
    let payload: Value = tokio::select! {
        biased;
        _ = signal.cancelled() => return Err(ModelsError::new(ModelsErrorCode::ModelSource, "B.AI model catalog request aborted")),
        payload = response.json() => payload,
    }.map_err(|error| {
        ModelsError::new(ModelsErrorCode::ModelSource, format!("Invalid B.AI model catalog response: {error}"))
    })?;
    if payload.get("success").and_then(Value::as_bool) == Some(false) {
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty());
        return Err(ModelsError::new(
            ModelsErrorCode::ModelSource,
            format!(
                "Could not load B.AI model catalog: {}",
                message.unwrap_or("request rejected")
            ),
        ));
    }
    let Some(data) = payload.get("data").and_then(Value::as_array) else {
        return Err(ModelsError::new(
            ModelsErrorCode::ModelSource,
            "Invalid B.AI model catalog response",
        ));
    };

    let mut discovered: Vec<Model> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in data {
        let id = entry.get("id").and_then(Value::as_str).map(str::trim);
        let Some(model) = id.and_then(|id| catalog_by_id.get(id)) else {
            continue;
        };
        if seen.insert(model.id.clone()) {
            discovered.push(model.clone());
        }
    }
    Ok(discovered)
}

pub fn bai_provider(options: Option<BaiProviderOptions>) -> Result<Arc<dyn Provider>, crate::models::ModelsError> {
    let options = options.unwrap_or_default();
    let base_url = normalize_base_url(options.base_url.as_deref().unwrap_or(BAI_BASE_URL));
    let anthropic_base_url = base_url.strip_suffix("/v1").unwrap_or(&base_url).to_owned();
    let catalog: Vec<Model> = options
        .models
        .clone()
        .unwrap_or_else(super::bai_models::bai_models)
        .into_iter()
        .map(|model| {
            let base_url = if model.api == "anthropic-messages" {
                anthropic_base_url.clone()
            } else {
                base_url.clone()
            };
            Model { base_url, ..model }
        })
        .collect();
    let catalog_by_id = Arc::new(index_catalog(&catalog));
    let fetch = options.fetch.clone().unwrap_or_default();

    let restore: RestoreModels = {
        let catalog_by_id = catalog_by_id.clone();
        Arc::new(move |models| Ok(remap_bai_models(&models, &catalog_by_id)))
    };
    let fetch_models: FetchModels = {
        let base_url = base_url.clone();
        Arc::new(move |context: &RefreshModelsContext| {
            let base_url = base_url.clone();
            let catalog_by_id = catalog_by_id.clone();
            let fetch = fetch.clone();
            let credential = context.credential.clone();
            let signal = context.signal.clone();
            Box::pin(async move {
                fetch_bai_models(&base_url, &catalog_by_id, &fetch, credential, &signal).await
            })
        })
    };

    let mut api = IndexMap::new();
    api.insert(
        "openai-responses".to_owned(),
        bai_responses_streams(builtin_api_streams("openai-responses")),
    );
    api.insert(
        "openai-completions".to_owned(),
        builtin_api_streams("openai-completions"),
    );
    api.insert(
        "anthropic-messages".to_owned(),
        builtin_api_streams("anthropic-messages"),
    );

    create_provider(CreateProviderOptions {
        id: "bai".into(),
        name: Some("B.AI".into()),
        base_url: Some(base_url),
        headers: None,
        auth: Some(ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth("B.AI API key", &["BAI_API_KEY"]))),
            oauth: None,
        }),
        models: Vec::new(),
        fetch_models: Some(fetch_models),
        restore_models: Some(restore),
        filter_models: None,
        images: indexmap::IndexMap::new(),
        api: ProviderApi::ByApi(api),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortController;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn abort_cancels_an_in_flight_catalog_request() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let controller = AbortController::new();
        let signal = controller.signal();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            started_tx.send(()).unwrap();
            release_rx.await.unwrap();
        });
        let request = tokio::spawn(async move {
            fetch_bai_models(
                &base_url,
                &HashMap::new(),
                &reqwest::Client::new(),
                Some(serde_json::json!({"type":"api_key", "key":"test"})),
                &signal,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), started_rx)
            .await
            .unwrap()
            .unwrap();
        controller.abort(None);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), request)
            .await
            .unwrap()
            .unwrap();
        release_tx.send(()).unwrap();
        server.await.unwrap();
        assert!(result.is_err());
    }
}
