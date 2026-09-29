//! `dag/fingerprint.ts`: canonical-JSON sha256 fingerprints of submitted DAG definitions.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::dag::types::{DagNodeId, DagRoute};

/// Fixed scheduler contract folded into every definition fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DagSchedulerContract {
    pub wave_admission: &'static str,
    pub failure_policy: &'static str,
    pub dependency_data: &'static str,
}

pub const DAG_SCHEDULER_CONTRACT: DagSchedulerContract = DagSchedulerContract {
    wave_admission: "strict-barrier",
    failure_policy: "continue-independent",
    dependency_data: "filesystem-only",
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DagNodeFingerprintInputV1 {
    pub node_id: DagNodeId,
    pub label: String,
    pub depends_on: Vec<DagNodeId>,
    /// The original prompt as submitted: never effectivePrompt or skill content.
    pub prompt: String,
    pub route: DagRoute,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub child_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DagDefinitionFingerprintInputV1 {
    pub name: String,
    pub scheduler: DagSchedulerContract,
    pub nodes: Vec<DagNodeFingerprintInputV1>,
}

fn json_string(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// Sorted-key canonical JSON; absent (`None`-skipped) fields never appear.
fn canonicalize(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::String(text) => json_string(text),
        Value::Array(entries) => {
            let parts: Vec<String> = entries.iter().map(canonicalize).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(record) => {
            let mut keys: Vec<&String> = record.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|key| format!("{}:{}", json_string(key), canonicalize(&record[key])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

pub fn dag_fingerprint(value: &Value) -> String {
    let digest = Sha256::digest(canonicalize(value).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn node_fingerprint_input(input: &DagNodeFingerprintInputV1) -> DagNodeFingerprintInputV1 {
    let mut normalized = input.clone();
    normalized.depends_on.sort();
    normalized
}

pub fn dag_definition_fingerprint(input: &DagDefinitionFingerprintInputV1) -> String {
    let mut nodes: Vec<DagNodeFingerprintInputV1> =
        input.nodes.iter().map(node_fingerprint_input).collect();
    nodes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    let normalized = DagDefinitionFingerprintInputV1 {
        name: input.name.clone(),
        scheduler: input.scheduler,
        nodes,
    };
    // Serializing plain owned strings and derived structs into a Value cannot fail.
    let value = serde_json::to_value(&normalized).unwrap_or(Value::Null);
    dag_fingerprint(&value)
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod tests;
