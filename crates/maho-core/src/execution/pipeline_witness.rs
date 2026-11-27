//! Port of senpi packages/coding-agent/src/core/execution/pipeline-witness.ts.

use std::sync::{Arc, Mutex};

use maho_agent::Agent;
use maho_agent::execution::tool_host::ExecutionScope;

use super::pipeline_config::{ExecutionPipelineConfig, resolve_execution_pipeline_from_env};

pub fn bind_pipeline_witness(
    agent: &Agent,
    session_id_reader: Arc<dyn Fn() -> String + Send + Sync>,
    root: Arc<dyn Fn() -> String + Send + Sync>,
    config: Option<ExecutionPipelineConfig>,
) -> Result<(), String> {
    let config = match config {
        Some(c) => c,
        None => resolve_execution_pipeline_from_env()?,
    };
    let state: Mutex<Option<(String, ExecutionScope)>> = Mutex::new(None);
    agent.set_get_execution_scope(Some(Arc::new(move || {
        let current = session_id_reader();
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((owner, scope)) = guard.as_ref()
            && owner == &current
        {
            return scope.clone();
        }
        let scope = ExecutionScope {
            id: uuid::Uuid::new_v4().to_string(),
            root: root(),
        };
        *guard = Some((current, scope.clone()));
        scope
    })));
    if config.named && agent.tool_host().is_some() {
        return Err(
            "SENPI_EXECUTION_PIPELINE=prepared cannot replace the ToolHost this session was built with"
                .to_owned(),
        );
    }
    Ok(())
}
