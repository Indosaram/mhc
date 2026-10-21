//! Port of senpi packages/ai/src/auth/oauth/github-copilot-model-catalog.ts.

use serde_json::{Map, Value};

fn as_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GitHubCopilotModelCatalog {
    pub available_model_ids: Vec<String>,
    pub policy_model_ids: Vec<String>,
    pub model_limits:
        Vec<crate::providers::github_copilot::github_copilot_limits::GitHubCopilotModelLimit>,
}

fn is_known_model(model_id: &str) -> bool {
    crate::models_generated::MODELS
        .get("github-copilot")
        .is_some_and(|models| models.contains_key(model_id))
}

pub fn parse_github_copilot_model_catalog(
    raw: &Value,
    allow_policy_fallback: bool,
) -> anyhow::Result<GitHubCopilotModelCatalog> {
    let data = as_record(raw)
        .and_then(|record| record.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Invalid Copilot models response"))?;

    let mut account_models: Vec<(String, bool, Option<String>)> = Vec::new();
    let mut model_limits = Vec::new();
    for raw_item in data {
        let Some(item) = as_record(raw_item) else {
            continue;
        };
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let supports = item
            .get("capabilities")
            .and_then(as_record)
            .and_then(|capabilities| capabilities.get("supports"))
            .and_then(as_record);
        if supports.and_then(|supports| supports.get("tool_calls")) == Some(&Value::Bool(false)) {
            continue;
        }
        if let Some(limits) = item.get("capabilities").and_then(|capabilities| capabilities.get("limits"))
            && let Some(limit) = crate::providers::github_copilot::github_copilot_limits::parse_github_copilot_model_limit(id, limits)
        {
            model_limits.push(limit);
        }
        let picker_enabled = item.get("model_picker_enabled") == Some(&Value::Bool(true));
        let policy_state = item
            .get("policy")
            .and_then(as_record)
            .and_then(|policy| policy.get("state"))
            .and_then(Value::as_str)
            .map(str::to_string);
        account_models.push((id.to_string(), picker_enabled, policy_state));
    }

    let picker_model_ids: Vec<String> = account_models
        .iter()
        .filter(|(_, picker_enabled, policy_state)| {
            *picker_enabled && policy_state.as_deref() != Some("disabled")
        })
        .map(|(id, _, _)| id.clone())
        .collect();
    let use_policy_fallback = allow_policy_fallback && picker_model_ids.is_empty();
    let available_model_ids = if !picker_model_ids.is_empty() || !allow_policy_fallback {
        picker_model_ids
    } else {
        account_models
            .iter()
            .filter(|(_, _, policy_state)| policy_state.as_deref() == Some("enabled"))
            .map(|(id, _, _)| id.clone())
            .collect()
    };
    let policy_model_ids = account_models
        .iter()
        .filter(|(id, picker_enabled, policy_state)| {
            policy_state.as_deref() == Some("unconfigured")
                && is_known_model(id)
                && (*picker_enabled || use_policy_fallback)
        })
        .map(|(id, _, _)| id.clone())
        .collect();
    Ok(GitHubCopilotModelCatalog {
        available_model_ids,
        policy_model_ids,
        model_limits,
    })
}
