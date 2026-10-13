//! `dag/execution-mode.test.ts`.

use std::collections::BTreeMap;

use omo_config_core::internal::validate::safe_parse;
use omo_config_core::schema::omo_config_schema;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::agents::builtin_agent;

fn quick() -> DagRoute {
    DagRoute::Category {
        category: "quick".to_string(),
    }
}

fn agent_route(agent: &str) -> DagRoute {
    DagRoute::Agent {
        agent: agent.to_string(),
        model: None,
    }
}

#[test]
fn given_no_sources_at_all_when_resolved_then_the_default_is_in_process() {
    let agents = BTreeMap::new();

    let mode = resolve_dag_node_execution_mode(&DagExecutionModeSources {
        route: &quick(),
        agents: &agents,
        config_mode: None,
        config_is_auto: false,
        auto_mode: None,
    });

    assert_eq!(mode, Some(ExecutionMode::InProcess));
}

#[test]
fn given_an_omo_json_task_default_execution_mode_when_resolved_then_the_config_mode_is_honored() {
    let agents = BTreeMap::new();

    let mode = resolve_dag_node_execution_mode(&DagExecutionModeSources {
        route: &quick(),
        agents: &agents,
        config_mode: Some(ExecutionMode::Process),
        config_is_auto: false,
        auto_mode: None,
    });

    assert_eq!(mode, Some(ExecutionMode::Process));
}

#[test]
fn given_a_per_agent_execution_mode_when_resolved_then_the_agent_mode_wins_over_config() {
    let mut agents = BTreeMap::new();
    agents.insert(
        "writer".to_string(),
        AgentDefinition {
            execution_mode: Some("process".to_string()),
            ..AgentDefinition::named("writer")
        },
    );

    let mode = resolve_dag_node_execution_mode(&DagExecutionModeSources {
        route: &agent_route("writer"),
        agents: &agents,
        config_mode: Some(ExecutionMode::InProcess),
        config_is_auto: false,
        auto_mode: None,
    });

    assert_eq!(mode, Some(ExecutionMode::Process));
}

#[test]
fn given_a_curated_read_only_agent_configured_for_process_when_resolved_then_it_is_forced_in_process()
 {
    let mut agents = BTreeMap::new();
    let explore = builtin_agent("explore").expect("explore builtin").clone();
    agents.insert(
        "explore".to_string(),
        AgentDefinition {
            execution_mode: Some("process".to_string()),
            ..explore
        },
    );

    let mode = resolve_dag_node_execution_mode(&DagExecutionModeSources {
        route: &agent_route("explore"),
        agents: &agents,
        config_mode: Some(ExecutionMode::Process),
        config_is_auto: false,
        auto_mode: None,
    });

    assert_eq!(mode, Some(ExecutionMode::InProcess));
}

#[test]
fn given_agent_mode_under_unresolved_auto_when_resolved_then_agent_mode_wins() {
    let mut agents = BTreeMap::new();
    agents.insert(
        "writer".to_string(),
        AgentDefinition {
            execution_mode: Some("process".to_string()),
            ..AgentDefinition::named("writer")
        },
    );
    assert_eq!(
        resolve_dag_node_execution_mode(&DagExecutionModeSources {
            route: &agent_route("writer"), agents: &agents, config_mode: None,
            config_is_auto: true, auto_mode: None,
        }),
        Some(ExecutionMode::Process)
    );
}

#[test]
fn given_resolved_auto_without_agent_mode_when_resolved_then_auto_mode_is_used() {
    let agents = BTreeMap::new();
    assert_eq!(
        resolve_dag_node_execution_mode(&DagExecutionModeSources {
            route: &quick(), agents: &agents, config_mode: None,
            config_is_auto: true, auto_mode: Some(ExecutionMode::Process),
        }),
        Some(ExecutionMode::Process)
    );
}

#[test]
fn given_unresolved_auto_without_agent_mode_when_resolved_then_result_is_none() {
    let agents = BTreeMap::new();
    assert_eq!(
        resolve_dag_node_execution_mode(&DagExecutionModeSources {
            route: &quick(), agents: &agents, config_mode: None,
            config_is_auto: true, auto_mode: None,
        }),
        None
    );
}

#[test]
fn given_a_config_carrying_task_dag_default_execution_mode_when_parsed_then_the_strict_schema_rejects_it()
 {
    let config = json!({
        "task": { "dag": { "max_nodes_per_run": 8, "default_execution_mode": "process" } }
    });

    let result = safe_parse(&omo_config_schema(), &config);

    assert!(result.is_err());
}
