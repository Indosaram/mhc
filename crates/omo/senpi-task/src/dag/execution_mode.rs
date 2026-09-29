//! `dag/execution-mode.ts`: resolve a DAG node's execution mode through the task chain verbatim.

use std::collections::BTreeMap;

use crate::agents::{AgentDefinition, curated_readonly_agent_names};
use crate::dag::types::DagRoute;
use crate::manager::execution_mode::{ExecutionMode, ExecutionModeSources, resolve_execution_mode};

pub struct DagExecutionModeSources<'a> {
    pub route: &'a DagRoute,
    pub agents: &'a BTreeMap<String, AgentDefinition>,
    /// omo.json `task.default_execution_mode`.
    pub config_mode: Option<ExecutionMode>,
}

/// `agentDef.executionMode ?? task.default_execution_mode ?? "in-process"`. There is no
/// `dag.default_execution_mode` knob. Curated read-only agents are forced in-process.
pub fn resolve_dag_node_execution_mode(sources: &DagExecutionModeSources<'_>) -> ExecutionMode {
    let agent_name = match sources.route {
        DagRoute::Agent { agent, .. } => Some(agent.as_str()),
        DagRoute::Category { .. } => None,
    };
    let agent_mode = agent_name.and_then(|name| {
        if curated_readonly_agent_names().contains(name) {
            Some(ExecutionMode::InProcess)
        } else {
            sources
                .agents
                .get(name)
                .and_then(|definition| definition.execution_mode.as_deref())
                .and_then(ExecutionMode::parse)
        }
    });
    resolve_execution_mode(ExecutionModeSources {
        spec_mode: None,
        agent_mode,
        config_mode: sources.config_mode,
    })
}

#[cfg(test)]
#[path = "execution_mode_tests.rs"]
mod tests;
