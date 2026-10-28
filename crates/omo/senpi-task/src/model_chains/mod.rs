//! Model-chain diagnostics resolved through the same registry as task spawns.
pub mod agent_chain;
pub mod category_chain;
pub mod rungs;
pub mod types;
use crate::agents::{
    BUILTIN_AGENT_DEFAULTS, curated_readonly_agent_names, map_omo_config_agents,
    ulw_reviewer_agent_names,
};
use crate::category::BUILTIN_CATEGORY_DEFAULTS;
use crate::host::{HostError, SenpiModelRegistry, parse_available_models};
use crate::category::exclude_model_patterns;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub use types::*;

pub fn describe_model_chains(
    config: &Value,
    registry: &dyn SenpiModelRegistry,
) -> Result<ModelChainsReport, HostError> {
    let available: BTreeSet<String> = parse_available_models(&registry.get_available()?)
        .ok_or_else(|| HostError {
            message: "the model registry did not return a model list".into(),
        })?
        .into_iter()
        .collect();
    let names: BTreeSet<String> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|category| category.name.to_string())
        .chain(
            config
                .get("categories")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|categories| categories.keys().cloned()),
        )
        .collect();
    let mut agents: BTreeMap<_, _> = BUILTIN_AGENT_DEFAULTS
        .iter()
        .map(|agent| (agent.name.clone(), agent.clone()))
        .collect();
    for (name, definition) in map_omo_config_agents(config) {
        let merged = agents.remove(&name).map_or_else(
            || definition.clone(),
            |base| base.overlaid_with(definition.clone()),
        );
        agents.insert(name, merged);
    }
    for name in curated_readonly_agent_names()
        .into_iter()
        .chain(ulw_reviewer_agent_names())
    {
        if let Some(definition) = agents.get_mut(name) {
            definition.execution_mode = Some("in-process".into());
        }
    }
    let agents: Vec<_> = agents.into_iter().collect();
    Ok(ModelChainsReport {
        exclude_models: exclude_model_patterns(config).unwrap_or_default(),
        categories: names
            .iter()
            .map(|name| category_chain::describe_category_chain(name, config, registry, &available))
            .collect::<Result<_, _>>()?,
        agents: agents
            .iter()
            .map(|(name, _)| {
                agent_chain::describe_agent_chain(name, &agents, config, registry, &available)
            })
            .collect::<Result<_, _>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::rungs::*;
    use super::*;
    #[test]
    fn classifies_runtime_exclusions_login_and_unreached_in_declaration_order() {
        let available = ["p/a", "p/b", "p/c", "p/d"]
            .map(String::from)
            .into_iter()
            .collect();
        let declared = ["p/a", "p/b", "p/c", "p/d", "p/e", "p/a"]
            .map(|model| DeclaredRung::new(model.into(), "omo.json"))
            .into_iter()
            .collect();
        let rungs = classify_rungs(
            declared,
            &["p/b".into(), "p/c".into()],
            &available,
            &["d".into()],
        );
        assert_eq!(
            rungs
                .iter()
                .map(|rung| rung.state.clone())
                .collect::<Vec<_>>(),
            vec![
                ChainRungState::NotReached,
                ChainRungState::Primary,
                ChainRungState::Fallback,
                ChainRungState::Excluded,
                ChainRungState::NoLogin
            ]
        );
        assert_eq!(rungs[3].excluded_by.as_deref(), Some("d"));
    }
    #[test]
    fn report_resolves_user_category_and_marks_exclusions() {
        use crate::host::fake::{FakeRegistry, model};
        let registry = FakeRegistry {
            available: Ok(serde_json::json!([model("p", "a"), model("p", "b")])),
            catalog: vec![model("p", "a"), model("p", "b")],
            find_override: None,
        };
        let config = serde_json::json!({"categories":{"custom":{"models":["p/a","p/b"]}},"exclude_models":["a"]});
        let report = describe_model_chains(&config, &registry).unwrap();
        let custom = report
            .categories
            .iter()
            .find(|category| category.name == "custom")
            .unwrap();
        assert_eq!(custom.primary.as_deref(), Some("p/b"));
        assert_eq!(custom.rungs[0].state, ChainRungState::Excluded);
    }
}

#[cfg(test)]
mod upstream_cases {
    use super::*;
    use crate::host::fake::{FakeRegistry, model};
    fn report(config: Value) -> ModelChainsReport {
        let models = vec![model("anthropic", "claude-fable-5-1"), model("anthropic", "claude-opus-5-5"), model("kimi-for-coding", "k3"), model("kimi-for-coding", "kimi-for-coding-highspeed")];
        let registry = FakeRegistry { available: Ok(Value::Array(models.clone())), catalog: models, find_override: None };
        describe_model_chains(&config, &registry).unwrap()
    }
    #[test]
    fn builtin_category_runtime_uses_declared_rung_order() {
        let report = report(serde_json::json!({}));
        let entry = report.categories.iter().find(|entry| entry.name == "artistry").unwrap();
        assert_eq!(entry.runtime, vec!["anthropic/claude-fable-5-1", "anthropic/claude-opus-5-5", "kimi-for-coding/k3"]);
        assert_eq!(entry.rungs.iter().map(|rung| &rung.state).collect::<Vec<_>>(), vec![&ChainRungState::Primary, &ChainRungState::Fallback, &ChainRungState::Fallback]);
    }
    #[test]
    fn later_override_leaves_earlier_builtin_not_reached() {
        let report = report(serde_json::json!({"categories":{"artistry":{"model":"anthropic/claude-opus-5-5"}}}));
        let entry = report.categories.iter().find(|entry| entry.name == "artistry").unwrap();
        assert_eq!(entry.runtime, vec!["anthropic/claude-opus-5-5", "kimi-for-coding/k3"]);
        assert_eq!(entry.rungs[0].source, "omo.json");
        assert_eq!(entry.rungs[1].state, ChainRungState::NotReached);
    }
    #[test]
    fn exclusion_promotes_next_builtin_rung() {
        let report = report(serde_json::json!({"exclude_models":["claude-fable-*"]}));
        let entry = report.categories.iter().find(|entry| entry.name == "artistry").unwrap();
        assert_eq!(entry.primary.as_deref(), Some("anthropic/claude-opus-5-5"));
        assert_eq!(entry.rungs[0].state, ChainRungState::Excluded);
        assert_eq!(entry.rungs[0].excluded_by.as_deref(), Some("claude-fable-*"));
    }
    #[test]
    fn agent_no_login_rungs_retain_provider_choices() {
        let report = report(serde_json::json!({}));
        let entry = report.agents.iter().find(|entry| entry.name == "explore").unwrap();
        assert_eq!(entry.runtime, vec!["kimi-for-coding/kimi-for-coding-highspeed"]);
        assert_eq!(entry.rungs[1].state, ChainRungState::NoLogin);
        assert_eq!(entry.rungs[1].providers, Some(vec!["chatgpt-subscription".into(),"openai".into()]));
    }
    #[test]
    fn configured_agent_source_and_disabled_category() {
        let report = report(serde_json::json!({"agents":{"scout":{"models":["anthropic/claude-opus-5-5"]}},"categories":{"artistry":{"disable":true}}}));
        let scout = report.agents.iter().find(|entry| entry.name == "scout").unwrap();
        assert_eq!(scout.rungs.len(), 1);
        assert_eq!(scout.rungs[0].source, "omo.json");
        assert_eq!(scout.rungs[0].state, ChainRungState::Primary);
        let artistry = report.categories.iter().find(|entry| entry.name == "artistry").unwrap();
        assert_eq!(artistry.status, "disabled");
        assert!(artistry.runtime.is_empty());
    }
}
