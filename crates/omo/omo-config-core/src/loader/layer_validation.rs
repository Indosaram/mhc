//! Per-layer validation with unknown-key stripping and surgical pruning.
//!
//! Port of `packages/omo-config-core/src/loader/layer-validation.ts`. Gate order: (1) unsafe own
//! keys are dropped and reported like other unknown keys; (2) schema-unknown keys are stripped with
//! the same `unknown-keys` diagnostic; (3) every remaining invalid value is pruned with its own
//! `invalid-value` diagnostic. A root that is not an object, an issue on the root, an exhausted
//! prune bound, or a file with nothing valid left rejects the file with its validation diagnostic.

use serde_json::{Map, Value};

use crate::internal::plain_object::is_unsafe_object_key;
use crate::internal::validate::safe_parse;
use crate::issue::{Issue, IssueCode, Issues};
use crate::loader::prune_invalid_leaves::{
    MAX_PRUNE_PASSES, PruneResult, PrunedConfigPath, prune_invalid_config_paths, value_at_mut,
};
use crate::loader::types::{
    DIAGNOSTIC_INVALID_VALUE, DIAGNOSTIC_UNKNOWN_KEYS, DIAGNOSTIC_VALIDATION, OmoConfigDiagnostic,
};
use crate::schema::config::omo_config_layer_schema;

pub enum OmoConfigLayerValidation {
    Loaded {
        diagnostics: Vec<OmoConfigDiagnostic>,
        value: Map<String, Value>,
    },
    NotLoaded {
        diagnostics: Vec<OmoConfigDiagnostic>,
    },
}

pub fn validation_diagnostic(path: &str, issues: &[Issue]) -> OmoConfigDiagnostic {
    let issue_paths: Vec<String> = issues.iter().map(Issue::path_string).collect();
    OmoConfigDiagnostic {
        kind: DIAGNOSTIC_VALIDATION,
        message: format!("Invalid omo config at {path}: {}", issue_paths.join(", ")),
        path: path.to_string(),
        issue_paths,
    }
}

pub fn invalid_value_diagnostics(
    path: &str,
    dropped: &[PrunedConfigPath],
) -> Vec<OmoConfigDiagnostic> {
    dropped
        .iter()
        .map(|entry| OmoConfigDiagnostic {
            kind: DIAGNOSTIC_INVALID_VALUE,
            message: format!(
                "Ignored invalid value in {path}: {}: {}",
                entry.key, entry.message
            ),
            path: path.to_string(),
            issue_paths: vec![entry.key.clone()],
        })
        .collect()
}

struct UnrecognizedKeyIssue {
    keys: Vec<String>,
    path: Vec<String>,
}

fn joined_path(path: &[String], segment: &str) -> String {
    let mut next = path.to_vec();
    next.push(segment.to_string());
    next.join(".")
}

fn unrecognized_key_issues(issues: &[Issue]) -> Vec<UnrecognizedKeyIssue> {
    issues
        .iter()
        .filter(|issue| issue.code == IssueCode::UnrecognizedKeys)
        .map(|issue| UnrecognizedKeyIssue {
            keys: issue.keys.clone(),
            path: issue.path.clone(),
        })
        .collect()
}

fn unknown_keys_diagnostic(path: &str, issue_paths: &[String]) -> OmoConfigDiagnostic {
    OmoConfigDiagnostic {
        kind: DIAGNOSTIC_UNKNOWN_KEYS,
        message: format!("Ignored unknown keys in {path}: {}", issue_paths.join(", ")),
        path: path.to_string(),
        issue_paths: issue_paths.to_vec(),
    }
}

fn sanitize_unsafe_keys(value: &Value, path: &[String]) -> (Vec<UnrecognizedKeyIssue>, Value) {
    match value {
        Value::Array(items) => {
            let mut issues = Vec::new();
            let mut sanitized = Vec::with_capacity(items.len());
            for (index, entry) in items.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(index.to_string());
                let (nested, cleaned) = sanitize_unsafe_keys(entry, &child_path);
                issues.extend(nested);
                sanitized.push(cleaned);
            }
            (issues, Value::Array(sanitized))
        }
        Value::Object(map) => {
            let mut issues = Vec::new();
            let mut sanitized = Map::new();
            for (key, entry) in map {
                if is_unsafe_object_key(key) {
                    issues.push(UnrecognizedKeyIssue {
                        keys: vec![key.clone()],
                        path: path.to_vec(),
                    });
                    continue;
                }
                let mut child_path = path.to_vec();
                child_path.push(key.clone());
                let (nested, cleaned) = sanitize_unsafe_keys(entry, &child_path);
                issues.extend(nested);
                sanitized.insert(key.clone(), cleaned);
            }
            (issues, Value::Object(sanitized))
        }
        other => (Vec::new(), other.clone()),
    }
}

fn container_at_mut<'a>(
    node: &'a mut Value,
    path: &[String],
) -> Option<&'a mut Map<String, Value>> {
    match value_at_mut(node, path) {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn strip_unrecognized_keys(
    record: &Map<String, Value>,
    issues: &[UnrecognizedKeyIssue],
) -> (Vec<String>, Map<String, Value>) {
    let mut stripped = Value::Object(record.clone());
    let mut issue_paths: Vec<String> = Vec::new();
    for issue in issues {
        let Some(container) = container_at_mut(&mut stripped, &issue.path) else {
            continue;
        };
        for key in &issue.keys {
            container.remove(key);
            issue_paths.push(joined_path(&issue.path, key));
        }
    }
    match stripped {
        Value::Object(map) => (issue_paths, map),
        _ => (issue_paths, Map::new()),
    }
}

fn parse_layer_record(record: &Map<String, Value>) -> Result<Value, Issues> {
    safe_parse(&omo_config_layer_schema(), &Value::Object(record.clone()))
}

pub fn validate_config_layer(path: &str, data: &Value) -> OmoConfigLayerValidation {
    let (unsafe_issues, sanitized) = sanitize_unsafe_keys(data, &[]);
    let record = match &sanitized {
        Value::Object(map) => Some(map.clone()),
        _ => None,
    };
    let mut unsafe_issue_paths: Vec<String> = Vec::new();
    for issue in &unsafe_issues {
        for key in &issue.keys {
            unsafe_issue_paths.push(joined_path(&issue.path, key));
        }
    }
    let mut diagnostics: Vec<OmoConfigDiagnostic> = if unsafe_issue_paths.is_empty() {
        Vec::new()
    } else {
        vec![unknown_keys_diagnostic(path, &unsafe_issue_paths)]
    };

    match safe_parse(&omo_config_layer_schema(), &sanitized) {
        Ok(_) => match record {
            Some(record) => OmoConfigLayerValidation::Loaded {
                diagnostics,
                value: record,
            },
            None => OmoConfigLayerValidation::NotLoaded {
                diagnostics: vec![OmoConfigDiagnostic {
                    kind: DIAGNOSTIC_VALIDATION,
                    message: format!("Invalid omo config at {path}: root must be an object"),
                    path: path.to_string(),
                    issue_paths: Vec::new(),
                }],
            },
        },
        Err(issues) => {
            let rejected = vec![validation_diagnostic(path, &issues)];
            let unknown_issues = unrecognized_key_issues(&issues);
            let Some(record) = record else {
                return OmoConfigLayerValidation::NotLoaded {
                    diagnostics: rejected,
                };
            };
            let mut candidate = record;
            let mut pending = issues;
            if !unknown_issues.is_empty() {
                let (issue_paths, stripped) = strip_unrecognized_keys(&candidate, &unknown_issues);
                if !issue_paths.is_empty() {
                    diagnostics.push(unknown_keys_diagnostic(path, &issue_paths));
                }
                match parse_layer_record(&stripped) {
                    Ok(_) => {
                        return OmoConfigLayerValidation::Loaded {
                            diagnostics,
                            value: stripped,
                        };
                    }
                    Err(next) => {
                        candidate = stripped;
                        pending = next;
                    }
                }
            }
            let validate = |record: &Map<String, Value>| parse_layer_record(record).map(|_| ());
            match prune_invalid_config_paths(&candidate, &pending, &validate, MAX_PRUNE_PASSES) {
                PruneResult::Ok { config, dropped } => {
                    diagnostics.extend(invalid_value_diagnostics(path, &dropped));
                    OmoConfigLayerValidation::Loaded {
                        diagnostics,
                        value: config,
                    }
                }
                PruneResult::NotOk { .. } => OmoConfigLayerValidation::NotLoaded {
                    diagnostics: rejected,
                },
            }
        }
    }
}
