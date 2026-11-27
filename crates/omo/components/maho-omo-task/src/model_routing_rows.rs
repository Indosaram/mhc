//! `model-routing-rows.ts`: per-category and per-agent model routing snapshot for `omo.modelRouting.resolve`.

use std::collections::{BTreeSet, HashSet};

use senpi_task::{
    agents::{
        AgentDefinition, AgentResolutionResult, ResolveAgentOptions, canonical_agent_name,
        resolve_agent,
    },
    category::{
        BUILTIN_CATEGORY_DEFAULTS, CategoryResolutionResult, ModelSelectionSource,
        ResolveCategoryOptions, resolve_category,
    },
    host::SenpiModelRegistry,
    manager::types::ResolvedChildPlan,
    model_exclusion::is_excluded_model,
};
use serde_json::Value;

use crate::fast_mode_inheritance::inherit_parent_fast_mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRoutingRowKind {
    Category,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModelRoutingSource {
    #[serde(rename = "override")]
    Override,
    #[serde(rename = "provider-fallback")]
    ProviderFallback,
    #[serde(rename = "unresolved")]
    Unresolved,
}

/// One lane of `omo.modelRouting.resolve`: the model a spawn of it runs first, as the spawn
/// resolvers pick it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelRoutingRow {
    pub kind: ModelRoutingRowKind,
    pub name: String,
    pub effective: Option<String>,
    pub source: ModelRoutingSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

/// What the planner resolves a spawn against besides the config: its agent table and the parent's
/// service tier.
#[derive(Debug, Clone, Default)]
pub struct ModelRoutingPlannerInputs {
    pub agents: Vec<(String, AgentDefinition)>,
    pub parent_service_tier: Option<String>,
}

fn excluded_models(omo_config: &Value) -> Vec<String> {
    omo_config
        .get("exclude_models")
        .and_then(Value::as_array)
        .map(|patterns| {
            patterns
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn routed(
    kind: ModelRoutingRowKind,
    name: String,
    effective: String,
    source: ModelSelectionSource,
    variant: Option<String>,
) -> ModelRoutingRow {
    ModelRoutingRow {
        kind,
        name,
        effective: Some(effective),
        source: match source {
            ModelSelectionSource::User => ModelRoutingSource::Override,
            ModelSelectionSource::Builtin => ModelRoutingSource::ProviderFallback,
        },
        variant,
    }
}

fn unresolved(kind: ModelRoutingRowKind, name: String) -> ModelRoutingRow {
    ModelRoutingRow {
        kind,
        name,
        effective: None,
        source: ModelRoutingSource::Unresolved,
        variant: None,
    }
}

/// Every builtin and configured category, then every task agent, each under its canonical name and
/// resolved by `resolve_category` / `resolve_agent` exactly as the planner resolves a spawn: against
/// the planner's own agent table, then the parent fast-tier step. `variant` is the thinking level
/// the child runs at, in the planner's precedence (`reasoning`, then `reasoning_effort`, then
/// `variant`). The round-robin distributor is not consulted, so a read never advances a rotation: a
/// `round_robin` lane shows its first entry.
pub fn resolve_model_routing_rows(
    omo_config: &Value,
    registry: &dyn SenpiModelRegistry,
    planner: &ModelRoutingPlannerInputs,
) -> Vec<ModelRoutingRow> {
    let excluded = excluded_models(omo_config);
    let effective_model = |model: &str| -> String {
        let fast = inherit_parent_fast_mode(
            ResolvedChildPlan {
                model: model.to_owned(),
                ..ResolvedChildPlan::default()
            },
            Some(registry),
            planner.parent_service_tier.as_deref(),
        )
        .model;
        if is_excluded_model(&fast, &excluded) {
            model.to_owned()
        } else {
            fast
        }
    };

    let mut category_names: BTreeSet<String> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| definition.name.to_owned())
        .collect();
    if let Some(configured) = omo_config.get("categories").and_then(Value::as_object) {
        for key in configured.keys() {
            category_names.insert(key.clone());
        }
    }

    let mut rows = Vec::new();
    let mut seen_categories = HashSet::new();
    for name in category_names {
        match resolve_category(
            &name,
            omo_config,
            registry,
            &ResolveCategoryOptions::default(),
        ) {
            Ok(CategoryResolutionResult::Resolved {
                category,
                spec,
                model_selection,
                ..
            }) => {
                if !seen_categories.insert(category.clone()) {
                    continue;
                }
                let variant = spec.reasoning.or(spec.reasoning_effort).or(spec.variant);
                let model = format!("{}/{}", spec.provider, spec.model_id);
                rows.push(routed(
                    ModelRoutingRowKind::Category,
                    category,
                    effective_model(&model),
                    model_selection.source,
                    variant,
                ));
            }
            Ok(
                CategoryResolutionResult::Disabled { category, .. }
                | CategoryResolutionResult::NotFound { category, .. },
            ) => {
                if seen_categories.insert(category.clone()) {
                    rows.push(unresolved(ModelRoutingRowKind::Category, category));
                }
            }
            Ok(CategoryResolutionResult::ModelUnavailable(unavailable)) => {
                if seen_categories.insert(unavailable.category.clone()) {
                    rows.push(unresolved(
                        ModelRoutingRowKind::Category,
                        unavailable.category,
                    ));
                }
            }
            Err(_) => {
                if seen_categories.insert(name.clone()) {
                    rows.push(unresolved(ModelRoutingRowKind::Category, name));
                }
            }
        }
    }

    // A retired alias key (omo-senpi-*) resolves to its canonical omo-native-* agent, so it shares that row.
    let mut sorted_agents = planner.agents.clone();
    sorted_agents.sort_by(|(left, _), (right, _)| left.cmp(right));
    let agent_options = ResolveAgentOptions {
        omo_config: Some(omo_config.clone()),
        ..ResolveAgentOptions::default()
    };
    let mut seen_agents = HashSet::new();
    for (name, _) in &sorted_agents {
        let resolution = resolve_agent(name, &planner.agents, Some(registry), &agent_options);
        match resolution {
            AgentResolutionResult::Resolved(resolved) => {
                let canonical = canonical_agent_name(&resolved.agent);
                if !seen_agents.insert(canonical.clone()) {
                    continue;
                }
                let variant = resolved.resolved_model.as_ref().and_then(|record| {
                    record
                        .reasoning
                        .clone()
                        .or_else(|| record.reasoning_effort.clone())
                        .or_else(|| record.variant.clone())
                });
                rows.push(routed(
                    ModelRoutingRowKind::Agent,
                    canonical,
                    effective_model(&resolved.model),
                    resolved.model_source,
                    variant,
                ));
            }
            AgentResolutionResult::NotFound { agent, .. }
            | AgentResolutionResult::ModelUnavailable { agent, .. } => {
                let canonical = canonical_agent_name(&agent);
                if seen_agents.insert(canonical.clone()) {
                    rows.push(unresolved(ModelRoutingRowKind::Agent, canonical));
                }
            }
        }
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use senpi_task::host::HostError;
    use serde_json::json;

    struct FakeRegistry(Vec<Value>);

    impl SenpiModelRegistry for FakeRegistry {
        fn get_available(&self) -> Result<Value, HostError> {
            Ok(Value::Array(self.0.clone()))
        }
        fn find(&self, provider: &str, model_id: &str) -> Option<Value> {
            self.0
                .iter()
                .find(|entry| {
                    entry.get("provider").and_then(Value::as_str) == Some(provider)
                        && entry.get("id").and_then(Value::as_str) == Some(model_id)
                })
                .cloned()
        }
    }

    fn model_entry(provider: &str, id: &str) -> Value {
        json!({ "provider": provider, "id": id })
    }

    #[test]
    fn resolves_override_fallback_unresolved_fast_tier_and_non_advancing_rotation() {
        let registry = FakeRegistry(vec![
            model_entry("openai", "gpt-5"),
            json!({
                "provider": "openai",
                "id": "gpt-5-fast",
                "serviceTier": "priority",
                "upstreamModelId": "gpt-5"
            }),
            model_entry("anthropic", "claude-fable-5-2"),
            model_entry("google", "gemini-4-argon"),
        ]);
        let config = json!({
            "categories": {
                "quick": {
                    "model": "openai/gpt-5",
                    "reasoning": "medium"
                },
                "custom-rr": {
                    "models": [
                        "openai/gpt-5",
                        "anthropic/claude-fable-5-2"
                    ],
                    "models_distribution": "round_robin"
                }
            }
        });
        let agents = vec![
            (
                "custom-agent".to_string(),
                AgentDefinition {
                    model: Some("anthropic/claude-fable-5-2".to_string()),
                    variant: Some("high".to_string()),
                    ..AgentDefinition::default()
                },
            ),
            (
                "missing-agent".to_string(),
                AgentDefinition {
                    model: Some("missing/nope".to_string()),
                    ..AgentDefinition::default()
                },
            ),
        ];
        let planner = ModelRoutingPlannerInputs {
            agents,
            parent_service_tier: Some("priority".to_string()),
        };

        let first = resolve_model_routing_rows(&config, &registry, &planner);
        let second = resolve_model_routing_rows(&config, &registry, &planner);
        assert_eq!(first, second, "round_robin read must not advance");

        let quick = first
            .iter()
            .find(|row| row.kind == ModelRoutingRowKind::Category && row.name == "quick")
            .expect("quick row");
        assert_eq!(quick.effective.as_deref(), Some("openai/gpt-5-fast"));
        assert_eq!(quick.source, ModelRoutingSource::Override);
        assert_eq!(quick.variant.as_deref(), Some("medium"));

        let rr = first
            .iter()
            .find(|row| row.kind == ModelRoutingRowKind::Category && row.name == "custom-rr")
            .expect("custom-rr row");
        assert_eq!(rr.effective.as_deref(), Some("openai/gpt-5-fast"));
        assert_eq!(rr.source, ModelRoutingSource::Override);

        let agent = first
            .iter()
            .find(|row| row.kind == ModelRoutingRowKind::Agent && row.name == "custom-agent")
            .expect("custom-agent row");
        assert_eq!(
            agent.effective.as_deref(),
            Some("anthropic/claude-fable-5-2")
        );
        assert_eq!(agent.source, ModelRoutingSource::Override);
        assert_eq!(agent.variant.as_deref(), Some("high"));

        let missing = first
            .iter()
            .find(|row| row.kind == ModelRoutingRowKind::Agent && row.name == "missing-agent")
            .expect("missing-agent row");
        assert_eq!(missing.effective, None);
        assert_eq!(missing.source, ModelRoutingSource::Unresolved);
    }

    #[test]
    fn excluded_fast_sibling_keeps_base_model_on_routing_row() {
        let registry = FakeRegistry(vec![
            model_entry("openai", "gpt-5"),
            json!({
                "provider": "openai",
                "id": "gpt-5-fast",
                "serviceTier": "priority",
                "upstreamModelId": "gpt-5"
            }),
        ]);
        let config = json!({
            "exclude_models": ["openai/gpt-5-fast"],
            "categories": {
                "quick": { "model": "openai/gpt-5" }
            }
        });
        let rows = resolve_model_routing_rows(
            &config,
            &registry,
            &ModelRoutingPlannerInputs {
                agents: Vec::new(),
                parent_service_tier: Some("priority".to_string()),
            },
        );
        let quick = rows
            .iter()
            .find(|row| row.kind == ModelRoutingRowKind::Category && row.name == "quick")
            .expect("quick row");
        assert_eq!(quick.effective.as_deref(), Some("openai/gpt-5"));
    }
}
