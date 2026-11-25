//! Port of senpi packages/coding-agent/src/core/execution/pipeline-config.ts.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionPipelineConfig {
    /// The flag named `prepared`; unset or `default` leaves the session's ToolHost alone.
    pub named: bool,
}

/// Resolves `SENPI_EXECUTION_PIPELINE`. Since S36 the loop has one pipeline, the prepared one:
/// unset, empty, or `default` runs it, and `prepared` names it.
pub fn resolve_execution_pipeline(value: Option<&str>) -> Result<ExecutionPipelineConfig, String> {
    match value {
        None | Some("") | Some("default") => Ok(ExecutionPipelineConfig { named: false }),
        Some("prepared") => Ok(ExecutionPipelineConfig { named: true }),
        Some(other) => Err(format!(
            "SENPI_EXECUTION_PIPELINE must be prepared or default, got {other:?}"
        )),
    }
}

pub fn resolve_execution_pipeline_from_env() -> Result<ExecutionPipelineConfig, String> {
    let val = std::env::var("SENPI_EXECUTION_PIPELINE").ok();
    resolve_execution_pipeline(val.as_deref())
}
