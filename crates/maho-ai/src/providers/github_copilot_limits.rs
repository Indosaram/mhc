//! Port of senpi packages/ai/src/providers/github-copilot-limits.ts.

use crate::types::Model;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubCopilotModelLimit {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_window_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

fn positive_integer(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_f64()?;
    if number > 0.0 && number.fract() == 0.0 && number < u64::MAX as f64 {
        Some(number as u64)
    } else {
        None
    }
}

pub fn parse_github_copilot_model_limit(id: &str, raw: &Value) -> Option<GitHubCopilotModelLimit> {
    parse_limit(
        id,
        raw,
        [
            "max_context_window_tokens",
            "max_prompt_tokens",
            "max_output_tokens",
        ],
    )
}

fn parse_limit(id: &str, raw: &Value, names: [&str; 3]) -> Option<GitHubCopilotModelLimit> {
    let raw = raw.as_object()?;
    let limit = GitHubCopilotModelLimit {
        id: id.to_owned(),
        max_context_window_tokens: positive_integer(raw.get(names[0])),
        max_prompt_tokens: positive_integer(raw.get(names[1])),
        max_output_tokens: positive_integer(raw.get(names[2])),
    };
    (limit.max_context_window_tokens.is_some()
        || limit.max_prompt_tokens.is_some()
        || limit.max_output_tokens.is_some())
    .then_some(limit)
}

pub fn apply_github_copilot_model_limits(models: Vec<Model>, credential: &Value) -> Vec<Model> {
    if credential.get("type").and_then(Value::as_str) != Some("oauth") {
        return models;
    }
    let Some(entries) = credential
        .get("copilotModelLimits")
        .and_then(Value::as_array)
    else {
        return models;
    };
    let limits: HashMap<String, GitHubCopilotModelLimit> = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            parse_limit(
                id,
                entry,
                [
                    "maxContextWindowTokens",
                    "maxPromptTokens",
                    "maxOutputTokens",
                ],
            )
            .map(|limit| (id.to_owned(), limit))
        })
        .collect();
    models
        .into_iter()
        .map(|mut model| {
            if let Some(limit) = limits.get(&model.id) {
                let prompt = limit.max_prompt_tokens.or(limit.max_context_window_tokens);
                if let Some(prompt) = prompt {
                    model.context_window = limit
                        .max_context_window_tokens
                        .map_or(prompt, |context| context.min(prompt));
                }
                if let Some(output) = limit.max_output_tokens {
                    model.max_tokens = output;
                }
            }
            model
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_only_positive_integral_catalog_limits() {
        // Given invalid prompt/output values alongside a valid context cap.
        let raw = json!({"max_context_window_tokens": 4096, "max_prompt_tokens": 1.5, "max_output_tokens": -1});
        // When parsing the catalog entry.
        let limit = parse_github_copilot_model_limit("model", &raw).expect("context cap");
        // Then invalid fields do not become account caps.
        assert_eq!(
            (
                limit.max_context_window_tokens,
                limit.max_prompt_tokens,
                limit.max_output_tokens
            ),
            (Some(4096), None, None)
        );
    }

    #[test]
    fn applies_prompt_context_minimum_and_output_override() {
        // Given account caps which differ from the catalog, with malformed siblings.
        let mut model = crate::providers::github_copilot_models::github_copilot_models().remove(0);
        model.id = "model".into();
        model.context_window = 1000;
        model.max_tokens = 100;
        let credential = json!({"type":"oauth", "copilotModelLimits":[
            {"id":"model", "maxContextWindowTokens":8192, "maxPromptTokens":4096, "maxOutputTokens":512},
            {"id":"model", "maxPromptTokens":0}, {"id": false, "maxPromptTokens":1}
        ]});
        // When applying stored account limits.
        let models = apply_github_copilot_model_limits(vec![model], &credential);
        // Then the prompt is bounded by both caps and output uses the account override.
        assert_eq!(
            (models[0].context_window, models[0].max_tokens),
            (4096, 512)
        );
    }
}
