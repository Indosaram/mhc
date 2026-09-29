use std::collections::HashSet;

use serde_json::{Map, Value};

use super::fallback_chains::category_fallback_chain;
use super::prompts;
use crate::delegate_adapter::DelegateFallbackEntry;

/// The routing half of a builtin category's `omo.json` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinCategoryConfig {
    pub model: &'static str,
    pub variant: Option<&'static str>,
}

impl BuiltinCategoryConfig {
    /// The config as the `categories.<name>` JSON object it stands in for.
    pub fn to_value(self) -> Value {
        let mut object = Map::new();
        object.insert("model".into(), Value::from(self.model));
        if let Some(variant) = self.variant {
            object.insert("variant".into(), Value::from(variant));
        }
        Value::Object(object)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinCategoryDefinition {
    pub name: &'static str,
    pub config: BuiltinCategoryConfig,
    pub description: &'static str,
    pub prompt_append: &'static str,
    pub resolve_prompt_append: Option<fn(Option<&str>) -> &'static str>,
    pub requires_model: Option<&'static str>,
}

const fn config(model: &'static str, variant: Option<&'static str>) -> BuiltinCategoryConfig {
    BuiltinCategoryConfig { model, variant }
}

fn is_gpt_5_5_or_later_model(model: &str) -> bool {
    let model_name = model.rsplit('/').next().unwrap_or(model).to_lowercase();
    ["gpt-5.5", "gpt-5-5", "gpt-5.6", "gpt-5-6"]
        .iter()
        .any(|marker| model_name.contains(marker))
}

pub fn resolve_deep_category_prompt_append(model: Option<&str>) -> &'static str {
    match model {
        Some(model) if !model.is_empty() && is_gpt_5_5_or_later_model(model) => {
            prompts::DEEP_PROMPT_APPEND_GPT_5_5
        }
        _ => prompts::DEEP_PROMPT_APPEND,
    }
}

/// Declared in the TypeScript order: google, openai, anthropic, then kimi categories.
pub const BUILTIN_CATEGORY_DEFAULTS: [BuiltinCategoryDefinition; 9] = [
    BuiltinCategoryDefinition {
        name: "visual-engineering",
        config: config("anthropic/claude-opus-5", Some("max")),
        description: prompts::VISUAL_ENGINEERING_DESCRIPTION,
        prompt_append: prompts::VISUAL_ENGINEERING_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: None,
    },
    BuiltinCategoryDefinition {
        name: "artistry",
        config: config("anthropic/claude-fable-5", Some("xhigh")),
        description: prompts::ARTISTRY_DESCRIPTION,
        prompt_append: prompts::ARTISTRY_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: None,
    },
    BuiltinCategoryDefinition {
        name: "ultrabrain",
        config: config("openai/gpt-5.6-sol", Some("max")),
        description: prompts::ULTRABRAIN_DESCRIPTION,
        prompt_append: prompts::ULTRABRAIN_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: Some("gpt-5.6-sol"),
    },
    BuiltinCategoryDefinition {
        name: "deep",
        config: config("openai/gpt-5.6-sol", Some("medium")),
        description: prompts::DEEP_DESCRIPTION,
        prompt_append: prompts::DEEP_PROMPT_APPEND,
        resolve_prompt_append: Some(resolve_deep_category_prompt_append),
        requires_model: Some("gpt-5.6-sol"),
    },
    BuiltinCategoryDefinition {
        name: "quick",
        config: config("kimi-coding/kimi-for-coding-highspeed", None),
        description: prompts::QUICK_DESCRIPTION,
        prompt_append: prompts::QUICK_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: None,
    },
    BuiltinCategoryDefinition {
        name: "unspecified-low",
        config: config("xai/grok-4.6", Some("xhigh")),
        description: prompts::UNSPECIFIED_LOW_DESCRIPTION,
        prompt_append: prompts::UNSPECIFIED_LOW_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: None,
    },
    BuiltinCategoryDefinition {
        name: "architect",
        config: config("anthropic/claude-fable-5", Some("xhigh")),
        description: prompts::ARCHITECT_DESCRIPTION,
        prompt_append: prompts::ARCHITECT_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: Some("claude-fable-5"),
    },
    BuiltinCategoryDefinition {
        name: "unspecified-high",
        config: config("kimi-coding/k3", Some("max")),
        description: prompts::UNSPECIFIED_HIGH_DESCRIPTION,
        prompt_append: prompts::UNSPECIFIED_HIGH_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: None,
    },
    BuiltinCategoryDefinition {
        name: "writing",
        config: config("kimi-coding/k3", Some("low")),
        description: prompts::WRITING_DESCRIPTION,
        prompt_append: prompts::WRITING_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: None,
    },
];

pub fn builtin_category(name: &str) -> Option<&'static BuiltinCategoryDefinition> {
    BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .find(|definition| definition.name == name)
}

/// `BUILTIN_CATEGORY_REQUIRES_MODEL` as ordered `(category, gate model)` pairs.
pub fn builtin_category_requires_model() -> Vec<(&'static str, &'static str)> {
    BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .filter_map(|definition| {
            definition
                .requires_model
                .map(|model| (definition.name, model))
        })
        .collect()
}

pub fn category_gate_model(category_name: &str) -> Option<&'static str> {
    builtin_category(category_name).and_then(|definition| definition.requires_model)
}

pub fn is_category_gate_satisfied(
    category_name: &str,
    has_explicit_user_config: bool,
    available_model_ids: &HashSet<String>,
) -> bool {
    match category_gate_model(category_name) {
        Some(gate_model) if !has_explicit_user_config => available_model_ids.contains(gate_model),
        _ => true,
    }
}

fn transform_chain_model_id<'a>(provider: &str, model: &'a str) -> &'a str {
    match (provider, model) {
        ("kimi-coding" | "kimi-for-coding", "kimi-k3") => "k3",
        ("kimi-coding" | "kimi-for-coding", "kimi-k3-256k") => "k3-256k",
        _ => model,
    }
}

/// A rung resolves when the registry exposes its model id directly (including a gateway-unwrapped
/// id) or through the provider-specific id transform (`kimi-coding/k3` satisfies `kimi-k3`).
pub fn is_category_chain_rung_resolvable(
    entry: &DelegateFallbackEntry,
    available_model_ids: &HashSet<String>,
) -> bool {
    available_model_ids.contains(&entry.model)
        || entry.providers.iter().any(|provider| {
            available_model_ids.contains(transform_chain_model_id(provider, &entry.model))
        })
}

/// A builtin-only category is usable only when at least one fallback rung resolves; any explicit
/// `categories.<name>` entry opts out of the check.
pub fn is_category_chain_viable(
    category_name: &str,
    has_explicit_user_config: bool,
    available_model_ids: &HashSet<String>,
) -> bool {
    if has_explicit_user_config {
        return true;
    }
    match category_fallback_chain(category_name) {
        Some(chain) if !chain.is_empty() => chain
            .iter()
            .any(|rung| is_category_chain_rung_resolvable(rung, available_model_ids)),
        _ => true,
    }
}
