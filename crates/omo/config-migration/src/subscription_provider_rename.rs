use serde_json::{Map, Value};

use crate::transform_types::ConfigMigrationTransformResult;

pub const SUBSCRIPTION_PROVIDER_RENAME_MIGRATION_ID: &str = "2026-09-subscription-provider-rename";

/// Legacy subscription provider ids and the canonical id each became.
///
/// The metered API-key lanes `openai` and `anthropic` are deliberately absent:
/// they were never renamed, and mapping them here would silently move a user
/// off the lane they are paying per-token for.
const LEGACY_PROVIDER_IDS: [(&str, &str); 2] = [
    ("claude-sdk-oauth", "anthropic-subscription"),
    ("openai-codex", "chatgpt-subscription"),
];

/// This migration is COSMETIC CONVERGENCE ONLY. Correctness already comes from
/// the engine normalizing legacy ids on read, so a config this never touches
/// still resolves. It therefore rewrites provider-bearing VALUES and nothing
/// else: unknown keys, custom categories and user comments are carried through
/// untouched, and a value it does not recognise is returned as-is.
fn rename_provider_id(provider_id: &str) -> Option<&'static str> {
    LEGACY_PROVIDER_IDS
        .iter()
        .find(|(legacy, _)| *legacy == provider_id)
        .map(|(_, canonical)| *canonical)
}

fn rename_model_reference(model_reference: &str) -> Option<String> {
    let provider_separator = model_reference.find('/')?;
    if provider_separator == 0 {
        return None;
    }
    let canonical_provider = rename_provider_id(&model_reference[..provider_separator])?;
    Some(format!(
        "{canonical_provider}{}",
        &model_reference[provider_separator..]
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionProviderRewrite {
    pub from: String,
    pub path: String,
    pub to: String,
}

fn rewrite_value(
    value: &Value,
    path: &str,
    rewrites: &mut Vec<SubscriptionProviderRewrite>,
) -> Value {
    match value {
        Value::String(text) => {
            let renamed = rename_model_reference(text)
                .or_else(|| rename_provider_id(text).map(str::to_string));
            match renamed {
                None => value.clone(),
                Some(renamed) => {
                    rewrites.push(SubscriptionProviderRewrite {
                        from: text.clone(),
                        path: path.to_string(),
                        to: renamed.clone(),
                    });
                    Value::String(renamed)
                }
            }
        }
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    rewrite_value(entry, &format!("{path}[{index}]"), rewrites)
                })
                .collect(),
        ),
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, entry) in map {
                let canonical_key = rename_model_reference(key)
                    .or_else(|| rename_provider_id(key).map(str::to_string));
                let next_key = canonical_key.clone().unwrap_or_else(|| key.clone());
                if let Some(canonical) = &canonical_key {
                    rewrites.push(SubscriptionProviderRewrite {
                        from: key.clone(),
                        path: format!("{path}.{key}"),
                        to: canonical.clone(),
                    });
                }
                result.insert(
                    next_key.clone(),
                    rewrite_value(entry, &format!("{path}.{next_key}"), rewrites),
                );
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

pub fn has_legacy_subscription_provider_ids(document: &Value) -> bool {
    let mut rewrites = Vec::new();
    rewrite_value(document, "$", &mut rewrites);
    !rewrites.is_empty()
}

pub fn transform_subscription_provider_rename(document: &Value) -> ConfigMigrationTransformResult {
    let mut rewrites = Vec::new();
    let rewritten = rewrite_value(document, "$", &mut rewrites);
    let next = match rewritten {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    ConfigMigrationTransformResult {
        diagnostics: rewrites
            .iter()
            .map(|rewrite| {
                format!(
                    "{}: {} renamed to {}",
                    rewrite.path, rewrite.from, rewrite.to
                )
            })
            .collect(),
        document: next,
    }
}
