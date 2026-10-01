use super::registry::JsonRpcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedSearchParams {
    pub search_term: String,
    pub cursor: Option<String>,
    pub limit: u32,
    pub sort_key: String,
    pub sort_direction: String,
    pub source_kinds: Vec<String>,
    pub archived: bool,
}
fn invalid(message: &str) -> JsonRpcError {
    JsonRpcError::new(-32600, message)
}
fn optional<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object()?.get(key).filter(|v| !v.is_null())
}
pub fn parse_search_params(value: &Value) -> Result<ParsedSearchParams, JsonRpcError> {
    let term = optional(value, "searchTerm")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("thread/search requires a non-empty searchTerm"))?;
    let cursor = match optional(value, "cursor") {
        None => None,
        Some(Value::String(cursor)) => Some(cursor.clone()),
        _ => return Err(invalid("thread/search received an invalid cursor")),
    };
    let limit = match optional(value, "limit") {
        None => 25,
        Some(v) => {
            let n = v
                .as_f64()
                .filter(|n| n.fract() == 0.0 && *n >= 0.0 && *n <= f64::from(u32::MAX))
                .ok_or_else(|| invalid("thread/search received an invalid limit"))?;
            n.clamp(1.0, 100.0) as u32
        }
    };
    let sort_key = match optional(value, "sortKey") {
        None => "created_at",
        Some(Value::String(s))
            if matches!(s.as_str(), "created_at" | "updated_at" | "recency_at") =>
        {
            s
        }
        _ => return Err(invalid("thread/search received an invalid sortKey")),
    };
    let sort_direction = match optional(value, "sortDirection") {
        None => "desc",
        Some(Value::String(s)) if matches!(s.as_str(), "asc" | "desc") => s,
        _ => return Err(invalid("thread/search received an invalid sortDirection")),
    };
    let source_kinds = match optional(value, "sourceKinds") {
        None => vec!["cli".into(), "vscode".into()],
        Some(Value::Array(values)) if values.is_empty() => vec!["cli".into(), "vscode".into()],
        Some(Value::Array(values)) => {
            let mut sources = BTreeSet::new();
            for value in values {
                let s = value
                    .as_str()
                    .filter(|s| {
                        matches!(
                            *s,
                            "cli"
                                | "vscode"
                                | "exec"
                                | "appServer"
                                | "subAgent"
                                | "subAgentReview"
                                | "subAgentCompact"
                                | "subAgentThreadSpawn"
                                | "subAgentOther"
                                | "unknown"
                        )
                    })
                    .ok_or_else(|| invalid("thread/search received an invalid sourceKinds"))?;
                sources.insert(s.to_owned());
            }
            sources.into_iter().collect()
        }
        _ => return Err(invalid("thread/search received an invalid sourceKinds")),
    };
    let archived = match optional(value, "archived") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(invalid("thread/search received an invalid archived flag")),
    };
    Ok(ParsedSearchParams {
        search_term: term.to_lowercase(),
        cursor,
        limit,
        sort_key: sort_key.into(),
        sort_direction: sort_direction.into(),
        source_kinds,
        archived,
    })
}
