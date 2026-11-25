//! Chain and pinned model refusal messages (`model-error-text.ts`, `#12759`, `#12634`).

use senpi_task::{
    DelegateFallbackEntry, agents::AgentDefinition, manager::types::PlanResolutionError,
};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainFailureTarget<'a> {
    Category(&'a str),
    Agent(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKey {
    Model,
    Models,
}

impl<'a> ChainFailureTarget<'a> {
    fn kind(self) -> &'static str {
        match self {
            Self::Category(_) => "Category",
            Self::Agent(_) => "Agent",
        }
    }

    fn name(self) -> &'a str {
        match self {
            Self::Category(name) | Self::Agent(name) => name,
        }
    }

    fn chain_fix_key(self) -> String {
        match self {
            Self::Category(category) => format!("categories.{category}.model"),
            Self::Agent(agent) => format!("agents.{agent}.model"),
        }
    }

    fn pin_fix_key(self, key: PinKey) -> String {
        match key {
            PinKey::Model => self.chain_fix_key(),
            PinKey::Models => match self {
                Self::Category(category) => format!("categories.{category}.models"),
                Self::Agent(agent) => format!("agents.{agent}.models"),
            },
        }
    }
}

fn chain_entries_text(chain: &[DelegateFallbackEntry]) -> String {
    chain
        .iter()
        .map(|rung| format!("{} ({})", rung.model, rung.providers.join(" or ")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn unlisted_opt_in_text(unlisted_model: Option<&str>, target: ChainFailureTarget<'_>) -> String {
    let Some(unlisted_model) = unlisted_model else {
        return String::new();
    };
    let Some((gateway, model)) = unlisted_model.split_once('/') else {
        return String::new();
    };
    if gateway.is_empty() {
        return String::new();
    }
    format!(
        " Only {gateway} serves {model}, and omo never bills a builtin {} to an unlisted provider; set {} = \"{unlisted_model}\" to opt in.",
        target.kind().to_ascii_lowercase(),
        target.chain_fix_key()
    )
}

/// Spawn-time refusal for a target whose entire chain needs unconnected providers: names EVERY
/// rung in order, missing providers, and both fixes. Nothing is substituted and no child spawns.
pub fn chain_unavailable_text(
    target: ChainFailureTarget<'_>,
    chain: &[DelegateFallbackEntry],
    missing_providers: &[String],
    unlisted_model: Option<&str>,
) -> String {
    let providers = if missing_providers.is_empty() {
        "none".to_string()
    } else {
        missing_providers.join(", ")
    };
    let models = if chain.is_empty() {
        "none".to_string()
    } else {
        chain_entries_text(chain)
    };
    format!(
        "{} \"{}\" has no usable model: its chain needs providers this machine has not connected ({providers}). Chain, in order: {models}.{} Fix: connect one of those providers, or set {} in Settings to a model you already run. No child was spawned.",
        target.kind(),
        target.name(),
        unlisted_opt_in_text(unlisted_model, target),
        target.chain_fix_key()
    )
}

/// Refusal for a target pinned to a model the live registry does not serve.
pub fn pinned_model_unavailable_text(
    target: ChainFailureTarget<'_>,
    pinned_model: &str,
    pin_key: PinKey,
) -> String {
    format!(
        "{} \"{}\" is pinned to \"{pinned_model}\", which this machine's model registry does not serve: that provider is not connected, or it does not list that model id. Fix: connect the provider, or re-pick {} in Settings. No child was spawned.",
        target.kind(),
        target.name(),
        target.pin_fix_key(pin_key)
    )
}

fn first_configured_pin_from_value(config: &Value) -> Option<(String, PinKey)> {
    if let Some(model) = config.get("model").and_then(Value::as_str) {
        return Some((model.to_string(), PinKey::Model));
    }
    let first = config.get("models").and_then(Value::as_array)?.first()?;
    let model = first
        .as_str()
        .or_else(|| first.get("model").and_then(Value::as_str))?;
    Some((model.to_string(), PinKey::Models))
}

fn first_configured_pin_from_agent(definition: &AgentDefinition) -> Option<(String, PinKey)> {
    if let Some(model) = &definition.model {
        return Some((model.clone(), PinKey::Model));
    }
    let first = definition.models.as_ref()?.first()?;
    Some((first.model().to_string(), PinKey::Models))
}

pub fn category_model_failure_text(
    error: &PlanResolutionError,
    omo_config: &Value,
) -> Option<String> {
    let category = error.category.as_deref()?;
    if let Some(chain) = &error.attempted_chain {
        return Some(chain_unavailable_text(
            ChainFailureTarget::Category(category),
            chain,
            error.missing_providers.as_deref().unwrap_or(&[]),
            error.unlisted_provider_model.as_deref(),
        ));
    }
    let (pinned, key) = first_configured_pin_from_value(&omo_config["categories"][category])?;
    Some(pinned_model_unavailable_text(
        ChainFailureTarget::Category(category),
        &pinned,
        key,
    ))
}

pub fn agent_model_failure_text(
    requested_name: &str,
    canonical_name: Option<&str>,
    attempted_model: Option<&str>,
    attempted_chain: Option<&[DelegateFallbackEntry]>,
    missing_providers: Option<&[String]>,
    agents: &[(String, AgentDefinition)],
) -> String {
    let agent = canonical_name.unwrap_or(requested_name);
    if let Some((_, definition)) = agents.iter().find(|(name, _)| name == agent)
        && let Some((pinned, key)) = first_configured_pin_from_agent(definition)
    {
        return pinned_model_unavailable_text(ChainFailureTarget::Agent(agent), &pinned, key);
    }
    if let Some(chain) = attempted_chain {
        return chain_unavailable_text(
            ChainFailureTarget::Agent(agent),
            chain,
            missing_providers.unwrap_or(&[]),
            None,
        );
    }
    format!(
        "No available model for agent \"{agent}\" (attempted {}).",
        attempted_model.unwrap_or("none")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dead_chain_names_every_rung_and_fix_key_and_pinned_models_names_models_key() {
        let chain = vec![
            DelegateFallbackEntry {
                providers: vec!["openai".to_string()],
                model: "gpt-5.4".to_string(),
                variant: Some("xhigh".to_string()),
            },
            DelegateFallbackEntry {
                providers: vec!["anthropic".to_string()],
                model: "claude-opus-4-6".to_string(),
                variant: None,
            },
        ];
        let text = chain_unavailable_text(
            ChainFailureTarget::Category("ultrabrain"),
            &chain,
            &["openai".to_string(), "anthropic".to_string()],
            None,
        );
        assert!(text.contains("gpt-5.4 (openai), claude-opus-4-6 (anthropic)"));
        assert!(text.contains("categories.ultrabrain.model in Settings"));
        assert!(text.contains("No child was spawned."));

        let mut err = PlanResolutionError::new(
            senpi_task::manager::types::PlanResolutionCode::ModelUnavailable,
            "raw",
        );
        err.category = Some("quick".to_string());
        let msg = category_model_failure_text(
            &err,
            &json!({ "categories": { "quick": { "models": ["openai/missing"] } } }),
        )
        .expect("pinned message");
        assert!(msg.contains("categories.quick.models in Settings"));
        assert!(msg.contains("openai/missing"));

        let mut def = AgentDefinition::named("explore");
        def.models = Some(vec![senpi_task::agents::AgentModelEntry::Model(
            "anthropic/missing".to_string(),
        )]);
        let agent_msg = agent_model_failure_text(
            "explore",
            Some("explore"),
            None,
            Some(&chain),
            Some(&["openai".to_string()]),
            &[("explore".to_string(), def)],
        );
        assert!(agent_msg.contains("agents.explore.models in Settings"));
    }
}
