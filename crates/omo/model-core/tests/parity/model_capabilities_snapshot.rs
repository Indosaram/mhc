use model_core::FetchResponse;
use model_core::ModelCapabilitiesSnapshotEntry;
use model_core::SnapshotFetchError;
use model_core::SnapshotFetcher;
use model_core::SnapshotLimit;
use model_core::SnapshotModalities;
use model_core::build_model_capabilities_snapshot_from_models_dev;
use model_core::fetch_model_capabilities_snapshot;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support::strings;

#[test]
fn builds_a_normalized_snapshot_from_models_dev_provider_data() {
    let raw = json!({
        "openai": {
            "models": {
                "gpt-5.4": {
                    "id": "gpt-5.4",
                    "family": "gpt",
                    "reasoning": true,
                    "temperature": false,
                    "tool_call": true,
                    "modalities": { "input": ["text", "image"], "output": ["text"] },
                    "limit": { "context": 1_050_000, "output": 128_000 },
                },
            },
        },
    });

    let snapshot = build_model_capabilities_snapshot_from_models_dev(&raw);

    assert_eq!(snapshot.source_url, "https://models.dev/api.json");
    assert_eq!(
        snapshot.models.get("gpt-5.4"),
        Some(&ModelCapabilitiesSnapshotEntry {
            id: "gpt-5.4".to_string(),
            family: Some("gpt".to_string()),
            reasoning: Some(true),
            temperature: Some(false),
            tool_call: Some(true),
            modalities: Some(SnapshotModalities {
                input: Some(strings(&["text", "image"])),
                output: Some(strings(&["text"])),
            }),
            limit: Some(SnapshotLimit {
                context: Some(1_050_000),
                input: None,
                output: Some(128_000),
            }),
        })
    );
}

#[test]
fn ignores_malformed_provider_entries_and_missing_fields() {
    let raw = json!({
        "invalidProvider": null,
        "anthropic": {
            "models": {
                "claude-sonnet-4-6": { "reasoning": true },
                "bad-model": "invalid",
            },
        },
        "openai": {
            "models": {
                "gpt-5.4": { "id": "GPT-5.4", "modalities": { "input": ["text", 1] } },
            },
        },
    });

    let snapshot = build_model_capabilities_snapshot_from_models_dev(&raw);

    assert_eq!(
        snapshot.models.get("claude-sonnet-4-6"),
        Some(&ModelCapabilitiesSnapshotEntry {
            id: "claude-sonnet-4-6".to_string(),
            reasoning: Some(true),
            ..Default::default()
        })
    );
    assert_eq!(
        snapshot.models.get("gpt-5.4"),
        Some(&ModelCapabilitiesSnapshotEntry {
            id: "GPT-5.4".to_string(),
            modalities: Some(SnapshotModalities {
                input: Some(strings(&["text"])),
                output: None,
            }),
            ..Default::default()
        })
    );
    assert_eq!(snapshot.models.get("bad-model"), None);
}

struct FixtureFetcher {
    body: String,
}

impl SnapshotFetcher for FixtureFetcher {
    async fn fetch(&self, _url: &str) -> Result<FetchResponse, SnapshotFetchError> {
        Ok(FetchResponse {
            status: 200,
            body: self.body.clone(),
        })
    }
}

#[test]
fn fetches_snapshot_using_injected_fetch_implementation() {
    let source_url = "https://fixture.local/models.json";
    let fetcher = FixtureFetcher {
        body: json!({
            "openai": { "models": { "gpt-5.4": { "id": "gpt-5.4", "limit": { "output": 128_000 } } } },
        })
        .to_string(),
    };

    let snapshot = futures::executor::block_on(fetch_model_capabilities_snapshot(
        &fetcher,
        Some(source_url),
    ))
    .expect("fetch succeeds");

    assert_eq!(snapshot.source_url, source_url);
    assert_eq!(
        snapshot
            .models
            .get("gpt-5.4")
            .and_then(|entry| entry.limit.as_ref())
            .and_then(|limit| limit.output),
        Some(128_000)
    );
}
