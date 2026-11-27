use model_core::latest_models::{CLAUDE_FABLE, GPT_ASTRA, GPT_SOL};
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
    pub requires_model: &'static [&'static str],
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
pub const BUILTIN_CATEGORY_DEFAULTS: [BuiltinCategoryDefinition; 10] = [
    BuiltinCategoryDefinition {
        name: "visual-engineering",
        config: config("google/gemini-4-argon", Some("low")),
        description: prompts::VISUAL_ENGINEERING_DESCRIPTION,
        prompt_append: prompts::VISUAL_ENGINEERING_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
    },
    BuiltinCategoryDefinition {
        name: "artistry",
        config: config("anthropic/claude-fable-5-2", Some("max")),
        description: prompts::ARTISTRY_DESCRIPTION,
        prompt_append: prompts::ARTISTRY_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
    },
    BuiltinCategoryDefinition {
        name: "ultrabrain",
        config: config("chatgpt-subscription/gpt-6.2-astra", Some("max")),
        description: prompts::ULTRABRAIN_DESCRIPTION,
        prompt_append: prompts::ULTRABRAIN_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[GPT_ASTRA],
    },
    BuiltinCategoryDefinition {
        name: "deep-low",
        config: config("chatgpt-subscription/gpt-6.2-sol", Some("medium")),
        description: "Default deep lane: one goal, one deliverable, decisions the child can settle from what it reads; preferred over deep-high for 3D graphics, computer/browser use, CAPTCHA, multimodal, backend, logic, and algorithm work. Multiple goals fan out as parallel calls.",
        prompt_append: prompts::DEEP_PROMPT_APPEND,
        resolve_prompt_append: Some(resolve_deep_category_prompt_append),
        requires_model: &[GPT_SOL],
    },
    BuiltinCategoryDefinition {
        name: "deep-high",
        config: config("chatgpt-subscription/gpt-6.2-astra", Some("high")),
        description: "Escalation deep lane: a goal whose central decision cannot be settled from evidence alone. Same one-goal, one-deliverable contract as deep-low.",
        prompt_append: prompts::DEEP_PROMPT_APPEND,
        resolve_prompt_append: Some(resolve_deep_category_prompt_append),
        requires_model: &[GPT_ASTRA],
    },
    BuiltinCategoryDefinition {
        name: "quick",
        config: config("chatgpt-subscription/gpt-6-luna-fast", Some("low")),
        description: prompts::QUICK_DESCRIPTION,
        prompt_append: prompts::QUICK_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
    },
    BuiltinCategoryDefinition {
        name: "unspecified-low",
        config: config("anthropic/claude-sonnet-5-5", Some("medium")),
        description: prompts::UNSPECIFIED_LOW_DESCRIPTION,
        prompt_append: prompts::UNSPECIFIED_LOW_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
    },
    BuiltinCategoryDefinition {
        name: "unspecified-high",
        config: config("anthropic/claude-opus-5-5", Some("medium")),
        description: prompts::UNSPECIFIED_HIGH_DESCRIPTION,
        prompt_append: prompts::UNSPECIFIED_HIGH_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
    },
    BuiltinCategoryDefinition {
        name: "architect",
        config: config("anthropic/claude-fable-5-2", Some("xhigh")),
        description: prompts::ARCHITECT_DESCRIPTION,
        prompt_append: prompts::ARCHITECT_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[CLAUDE_FABLE, "claude-fable-5-1"],
    },
    BuiltinCategoryDefinition {
        name: "writing",
        config: config("google/gemini-4-argon", Some("low")),
        description: prompts::WRITING_DESCRIPTION,
        prompt_append: prompts::WRITING_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
    },
];

pub fn builtin_category(name: &str) -> Option<&'static BuiltinCategoryDefinition> {
    BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .find(|definition| definition.name == name)
}

pub fn category_gate_models(category_name: &str) -> &'static [&'static str] {
    builtin_category(category_name).map_or(&[], |definition| definition.requires_model)
}

fn provider_serves_model(provider: &str, model: &str, available: &HashSet<String>) -> bool {
    available.contains(&format!("{provider}/{model}"))
        || available.contains(&format!(
            "{provider}/{}",
            crate::delegate_adapter::transform_model_for_provider(provider, model)
        ))
}

pub fn is_category_gate_satisfied(
    category_name: &str,
    has_explicit_user_config: bool,
    available: &HashSet<String>,
) -> bool {
    let gates = category_gate_models(category_name);
    has_explicit_user_config
        || gates.is_empty()
        || category_fallback_chain(category_name)
            .into_iter()
            .flatten()
            .any(|rung| {
                gates.contains(&rung.model.as_str())
                    && is_category_chain_rung_resolvable(rung, available)
            })
}

pub fn is_category_chain_rung_resolvable(
    entry: &DelegateFallbackEntry,
    available: &HashSet<String>,
) -> bool {
    entry
        .providers
        .iter()
        .any(|provider| provider_serves_model(provider, &entry.model, available))
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
