use super::category_chain::category_runtime_chain;
use super::rungs::{DeclaredRung, builtin_rungs, classify_rungs};
use super::types::ModelChainEntry;
use crate::agents::{
    AgentDefinition, AgentResolutionResult, ResolveAgentOptions, agent_fallback_chain,
    agent_model_candidates, map_omo_config_agents, resolve_agent,
};
use crate::category::{ResolveCategoryOptions, resolve_category};
use crate::host::{HostError, SenpiModelRegistry};
use crate::category::exclude_model_patterns;
use crate::model_exclusion::without_excluded_models;
use serde_json::Value;
use std::collections::BTreeSet;

pub fn describe_agent_chain(
    name: &str,
    agents: &[(String, AgentDefinition)],
    config: &Value,
    registry: &dyn SenpiModelRegistry,
    available: &BTreeSet<String>,
) -> Result<ModelChainEntry, HostError> {
    let definition = agents
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, definition)| definition);
    if definition.is_some_and(|definition| definition.disable == Some(true)) {
        return Ok(ModelChainEntry {
            kind: "agent".into(),
            name: name.into(),
            status: "disabled".into(),
            primary: None,
            runtime: vec![],
            rungs: vec![],
            reason: Some("disabled by omo.json".into()),
        });
    }
    let own: BTreeSet<String> = map_omo_config_agents(config)
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, definition)| {
            agent_model_candidates(
                definition.model.as_deref(),
                definition.models.as_deref(),
                None,
                None,
            )
        })
        .unwrap_or_default()
        .into_iter()
        .map(|candidate| candidate.model)
        .collect();
    let mut declared = Vec::new();
    if let Some(definition) = definition {
        for candidate in agent_model_candidates(
            definition.model.as_deref(),
            definition.models.as_deref(),
            None,
            None,
        ) {
            let source = if own.contains(&candidate.model) {
                "omo.json"
            } else {
                "built-in"
            };
            declared.push(DeclaredRung::new(candidate.model, source));
        }
        for category in definition.categories.iter().flatten() {
            let result = resolve_category(
                category,
                config,
                registry,
                &ResolveCategoryOptions::default(),
            )?;
            declared.extend(
                category_runtime_chain(&result)
                    .into_iter()
                    .map(|model| DeclaredRung::new(model, &format!("category:{category}"))),
            );
        }
    }
    declared.extend(builtin_rungs(agent_fallback_chain(name), available));
    let exclusions = exclude_model_patterns(config).unwrap_or_default();
    let filtered = without_excluded_models(registry, Some(&exclusions));
    let result = resolve_agent(
        name,
        agents,
        Some(&filtered),
        &ResolveAgentOptions {
            omo_config: Some(config.clone()),
            ..Default::default()
        },
    );
    let (runtime, reason) = match result {
        AgentResolutionResult::Resolved(result) => (
            std::iter::once(result.model)
                .chain(
                    result
                        .fallback_models
                        .into_iter()
                        .flatten()
                        .map(|model| model.display),
                )
                .collect::<Vec<_>>(),
            None,
        ),
        AgentResolutionResult::ModelUnavailable {
            attempted_model, ..
        } => (
            vec![],
            Some(attempted_model.map_or_else(
                || "no rung is usable".into(),
                |model| format!("{model} is not usable"),
            )),
        ),
        AgentResolutionResult::NotFound { .. } => (vec![], None),
    };
    Ok(ModelChainEntry {
        kind: "agent".into(),
        name: name.into(),
        status: if runtime.is_empty() {
            "unavailable"
        } else {
            "resolved"
        }
        .into(),
        primary: runtime.first().cloned(),
        rungs: classify_rungs(declared, &runtime, available, &exclusions),
        runtime,
        reason,
    })
}
