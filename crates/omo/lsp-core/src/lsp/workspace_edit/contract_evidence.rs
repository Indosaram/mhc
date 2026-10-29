//! Port of TS `workspace-edit-contract-evidence.ts`.

use crate::lsp::workspace_apply_edit_failure::canonicalize_workspace_apply_edit_failure_reason;
use serde_json::Map;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;

/// TS `normalizeWorkspaceEditContractEvidence`: sorted keys, `<workspace>` paths.
pub fn normalize_workspace_edit_contract_evidence(value: &Value, workspace: &str) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace(workspace, "<workspace>")),
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| normalize_workspace_edit_contract_evidence(entry, workspace))
                .collect(),
        ),
        Value::Object(record) => {
            let mut keys: Vec<&String> = record.keys().collect();
            keys.sort();
            let mut normalized = Map::new();
            for key in keys {
                let entry = normalize_workspace_edit_contract_evidence(&record[key], workspace);
                let entry = match (key.as_str(), entry) {
                    ("failureReason", Value::String(reason)) => {
                        Value::String(canonicalize_workspace_apply_edit_failure_reason(&reason))
                    }
                    (_, entry) => entry,
                };
                normalized.insert(key.clone(), entry);
            }
            Value::Object(normalized)
        }
        other => other.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEditContractFailureEvidence {
    pub normalized: Value,
    pub sha256: String,
}

/// TS `workspaceEditContractFailureEvidence`.
pub fn workspace_edit_contract_failure_evidence(
    value: &Value,
    workspace: &str,
) -> WorkspaceEditContractFailureEvidence {
    let normalized = normalize_workspace_edit_contract_evidence(value, workspace);
    let serialized = serde_json::to_string(&normalized).expect("JSON values always serialize");
    WorkspaceEditContractFailureEvidence {
        sha256: hex::encode(Sha256::digest(serialized.as_bytes())),
        normalized,
    }
}
