//! Port of senpi packages/coding-agent/src/core/execution/pipeline-witness-sink.ts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_agent::execution::tool_host::{ExecutionPipelineWitness, WitnessRecorder};
use serde_json::json;

pub type ExecutionPipelineWitnessSink = WitnessRecorder;

/// A witness sink outside the session: appends one JSON line per receipt to `path`.
pub fn create_witness_file_sink(path: impl AsRef<Path>) -> ExecutionPipelineWitnessSink {
    let path: PathBuf = path.as_ref().to_path_buf();
    Arc::new(move |witness: ExecutionPipelineWitness| {
        let path = path.clone();
        Box::pin(async move {
            let line = json!({
                "pipeline": witness.pipeline,
                "toolCallId": witness.tool_call_id,
            });
            let write_res = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(error) = write_res {
                crate::output_guard::maho_write_stderr(&format!(
                    "ExecutionPipelineWitnessWarning: execution-pipeline witness not written to {}: {error}\n",
                    path.display()
                ));
            }
        })
    })
}

pub fn resolve_witness_sink(
    configured: Option<ExecutionPipelineWitnessSink>,
    env_path: Option<&str>,
) -> Option<ExecutionPipelineWitnessSink> {
    if configured.is_some() {
        return configured;
    }
    let env_owned = if env_path.is_none() {
        std::env::var("SENPI_EXECUTION_PIPELINE_WITNESS").ok()
    } else {
        None
    };
    let path = env_path.or(env_owned.as_deref())?;
    if path.is_empty() {
        None
    } else {
        Some(create_witness_file_sink(path))
    }
}
