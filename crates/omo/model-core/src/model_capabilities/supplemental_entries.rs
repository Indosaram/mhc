//! Entries missing from models.dev, layered over the bundled snapshot (values copied verbatim).

use std::sync::LazyLock;

use indexmap::IndexMap;

use super::types::ModelCapabilitiesSnapshotEntry;
use super::types::SnapshotLimit;
use super::types::SnapshotModalities;

fn strings(values: &[&str]) -> Option<Vec<String>> {
    Some(values.iter().map(|value| (*value).to_string()).collect())
}

pub static SUPPLEMENTAL_MODEL_CAPABILITIES: LazyLock<
    IndexMap<String, ModelCapabilitiesSnapshotEntry>,
> = LazyLock::new(|| {
    IndexMap::from([
        (
            "kimi-k3".to_string(),
            ModelCapabilitiesSnapshotEntry {
                id: "kimi-k3".to_string(),
                family: Some("kimi".to_string()),
                reasoning: Some(true),
                temperature: Some(true),
                tool_call: Some(true),
                modalities: Some(SnapshotModalities {
                    input: strings(&["text", "image", "video"]),
                    output: strings(&["text"]),
                }),
                limit: Some(SnapshotLimit {
                    context: Some(262144),
                    input: None,
                    output: Some(262144),
                }),
            },
        ),
        (
            "kimi-k2.6".to_string(),
            ModelCapabilitiesSnapshotEntry {
                id: "kimi-k2.6".to_string(),
                family: Some("kimi".to_string()),
                reasoning: Some(true),
                temperature: Some(true),
                tool_call: Some(true),
                modalities: Some(SnapshotModalities {
                    input: strings(&["text", "image", "video"]),
                    output: strings(&["text"]),
                }),
                limit: Some(SnapshotLimit {
                    context: Some(262144),
                    input: None,
                    output: Some(262144),
                }),
            },
        ),
        (
            "gpt-5.6-sol".to_string(),
            ModelCapabilitiesSnapshotEntry {
                id: "gpt-5.6-sol".to_string(),
                family: Some("gpt".to_string()),
                reasoning: Some(true),
                temperature: Some(false),
                tool_call: Some(true),
                modalities: Some(SnapshotModalities {
                    input: strings(&["text", "image", "pdf"]),
                    output: strings(&["text"]),
                }),
                limit: Some(SnapshotLimit {
                    context: Some(1050000),
                    input: Some(922000),
                    output: Some(128000),
                }),
            },
        ),
        (
            "gpt-5.6-terra".to_string(),
            ModelCapabilitiesSnapshotEntry {
                id: "gpt-5.6-terra".to_string(),
                family: Some("gpt-mini".to_string()),
                reasoning: Some(true),
                temperature: Some(false),
                tool_call: Some(true),
                modalities: Some(SnapshotModalities {
                    input: strings(&["text", "image", "pdf"]),
                    output: strings(&["text"]),
                }),
                limit: Some(SnapshotLimit {
                    context: Some(1050000),
                    input: Some(922000),
                    output: Some(128000),
                }),
            },
        ),
        (
            "gpt-5.6-luna".to_string(),
            ModelCapabilitiesSnapshotEntry {
                id: "gpt-5.6-luna".to_string(),
                family: Some("gpt-nano".to_string()),
                reasoning: Some(true),
                temperature: Some(false),
                tool_call: Some(true),
                modalities: Some(SnapshotModalities {
                    input: strings(&["text", "image", "pdf"]),
                    output: strings(&["text"]),
                }),
                limit: Some(SnapshotLimit {
                    context: Some(1050000),
                    input: Some(922000),
                    output: Some(128000),
                }),
            },
        ),
        (
            "gpt-5.5".to_string(),
            ModelCapabilitiesSnapshotEntry {
                id: "gpt-5.5".to_string(),
                family: Some("gpt".to_string()),
                reasoning: Some(true),
                temperature: Some(false),
                tool_call: Some(true),
                modalities: Some(SnapshotModalities {
                    input: strings(&["text", "image", "pdf"]),
                    output: strings(&["text"]),
                }),
                limit: Some(SnapshotLimit {
                    context: Some(400000),
                    input: Some(272000),
                    output: Some(128000),
                }),
            },
        ),
        (
            "gpt-5.6-luna-fast".to_string(),
            ModelCapabilitiesSnapshotEntry {
                id: "gpt-5.6-luna-fast".to_string(),
                family: Some("gpt-mini".to_string()),
                reasoning: Some(true),
                temperature: Some(false),
                tool_call: Some(true),
                modalities: Some(SnapshotModalities {
                    input: strings(&["text", "image"]),
                    output: strings(&["text"]),
                }),
                limit: Some(SnapshotLimit {
                    context: Some(400000),
                    input: Some(272000),
                    output: Some(128000),
                }),
            },
        ),
    ])
});
