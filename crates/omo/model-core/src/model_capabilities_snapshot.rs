use std::future::Future;

use chrono::SecondsFormat;
use chrono::Utc;
use indexmap::IndexMap;
use serde_json::Map;
use serde_json::Value;

use crate::model_capabilities::ModelCapabilitiesSnapshot;
use crate::model_capabilities::ModelCapabilitiesSnapshotEntry;
use crate::model_capabilities::SnapshotLimit;
use crate::model_capabilities::SnapshotModalities;

pub const MODELS_DEV_SOURCE_URL: &str = "https://models.dev/api.json";

type Record = Map<String, Value>;

fn read_string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let result: Vec<String> = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn normalize_snapshot_entry(
    raw_model_id: &str,
    raw_model: &Value,
) -> Option<ModelCapabilitiesSnapshotEntry> {
    let raw_model = raw_model.as_object()?;
    let modalities = raw_model
        .get("modalities")
        .and_then(Value::as_object)
        .and_then(|raw| {
            let input = read_string_array(raw.get("input"));
            let output = read_string_array(raw.get("output"));
            (input.is_some() || output.is_some()).then_some(SnapshotModalities { input, output })
        });
    let limit = raw_model
        .get("limit")
        .and_then(Value::as_object)
        .and_then(|raw| {
            let number = |key: &str| raw.get(key).and_then(Value::as_i64);
            let limit = SnapshotLimit {
                context: number("context"),
                input: number("input"),
                output: number("output"),
            };
            (limit != SnapshotLimit::default()).then_some(limit)
        });
    let boolean = |key: &str| raw_model.get(key).and_then(Value::as_bool);

    Some(ModelCapabilitiesSnapshotEntry {
        id: raw_model
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(raw_model_id)
            .to_string(),
        family: raw_model
            .get("family")
            .and_then(Value::as_str)
            .filter(|family| !family.is_empty())
            .map(str::to_string),
        reasoning: boolean("reasoning"),
        temperature: boolean("temperature"),
        tool_call: boolean("tool_call"),
        modalities,
        limit,
    })
}

/// Field-wise `{ ...existing, ...incoming }` with nested modalities/limit merged the same way.
fn merge_snapshot_entries(
    existing: Option<&ModelCapabilitiesSnapshotEntry>,
    incoming: ModelCapabilitiesSnapshotEntry,
) -> ModelCapabilitiesSnapshotEntry {
    let Some(existing) = existing else {
        return incoming;
    };
    let modalities = match (&existing.modalities, incoming.modalities) {
        (None, None) => None,
        (existing_modalities, incoming_modalities) => {
            let existing_modalities = existing_modalities.clone().unwrap_or_default();
            let incoming_modalities = incoming_modalities.unwrap_or_default();
            Some(SnapshotModalities {
                input: incoming_modalities.input.or(existing_modalities.input),
                output: incoming_modalities.output.or(existing_modalities.output),
            })
        }
    };
    let limit = match (&existing.limit, incoming.limit) {
        (None, None) => None,
        (existing_limit, incoming_limit) => {
            let existing_limit = existing_limit.clone().unwrap_or_default();
            let incoming_limit = incoming_limit.unwrap_or_default();
            Some(SnapshotLimit {
                context: incoming_limit.context.or(existing_limit.context),
                input: incoming_limit.input.or(existing_limit.input),
                output: incoming_limit.output.or(existing_limit.output),
            })
        }
    };
    ModelCapabilitiesSnapshotEntry {
        id: incoming.id,
        family: incoming.family.or_else(|| existing.family.clone()),
        reasoning: incoming.reasoning.or(existing.reasoning),
        temperature: incoming.temperature.or(existing.temperature),
        tool_call: incoming.tool_call.or(existing.tool_call),
        modalities,
        limit,
    }
}

/// Flattens models.dev `{ provider: { models: { id: raw } } }` into a snapshot keyed by lowercase id.
#[must_use]
pub fn build_model_capabilities_snapshot_from_models_dev(raw: &Value) -> ModelCapabilitiesSnapshot {
    let mut models: IndexMap<String, ModelCapabilitiesSnapshotEntry> = IndexMap::new();
    let empty = Record::new();
    let providers = raw.as_object().unwrap_or(&empty);

    for provider_value in providers.values() {
        let Some(provider_models) = provider_value.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (raw_model_id, raw_model) in provider_models {
            let Some(entry) = normalize_snapshot_entry(raw_model_id, raw_model) else {
                continue;
            };
            let key = entry.id.to_lowercase();
            let merged = merge_snapshot_entries(models.get(&key), entry);
            models.insert(key, merged);
        }
    }

    ModelCapabilitiesSnapshot {
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        source_url: MODELS_DEV_SOURCE_URL.to_string(),
        models,
    }
}

/// HTTP response surface needed by [`fetch_model_capabilities_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotFetchError {
    #[error("{0}")]
    Transport(String),
    #[error("models.dev fetch failed with {0}")]
    Status(u16),
    #[error("models.dev response is not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Injected HTTP GET used to download the models.dev catalog.
///
/// Implementations perform one request and return its status and body; transport failures map to
/// [`SnapshotFetchError::Transport`].
pub trait SnapshotFetcher {
    fn fetch(
        &self,
        url: &str,
    ) -> impl Future<Output = Result<FetchResponse, SnapshotFetchError>> + Send;
}

/// Downloads and normalizes a snapshot; `source_url` defaults to [`MODELS_DEV_SOURCE_URL`].
pub async fn fetch_model_capabilities_snapshot(
    fetcher: &impl SnapshotFetcher,
    source_url: Option<&str>,
) -> Result<ModelCapabilitiesSnapshot, SnapshotFetchError> {
    let source_url = source_url.unwrap_or(MODELS_DEV_SOURCE_URL);
    let response = fetcher.fetch(source_url).await?;
    if !(200..300).contains(&response.status) {
        return Err(SnapshotFetchError::Status(response.status));
    }
    let raw: Value = serde_json::from_str(&response.body)?;
    Ok(ModelCapabilitiesSnapshot {
        source_url: source_url.to_string(),
        ..build_model_capabilities_snapshot_from_models_dev(&raw)
    })
}
