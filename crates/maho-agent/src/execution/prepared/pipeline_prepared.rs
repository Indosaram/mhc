//! Port of senpi `packages/agent/src/execution/prepared/pipeline-prepared.ts`.

use std::sync::Arc;

use maho_ai::types::{BoxFuture, ToolResultMessage};

use crate::execution::tool_host::{
    ExecutionPipelineWitness, ToolHost, ToolHostCall, ToolHostOptions, ToolHostResult,
};
use crate::execution::tool_outcome_events::{
    create_tool_result_message, emit_tool_result_message, should_terminate_tool_batch,
};
use crate::types::{AgentToolCallOutcome, BatchStoreFactory, ToolExecutionMode};

use super::executor_pipeline::PreparationSource;
use super::pipeline_prepared_batch::{PreparedBatchRequest, run_prepared_batch};

struct PreparedToolHost {
    source: PreparationSource,
    open_store: BatchStoreFactory,
}

impl ToolHost for PreparedToolHost {
    fn execute(
        &self,
        call: ToolHostCall,
        options: ToolHostOptions,
    ) -> BoxFuture<'_, Result<ToolHostResult, String>> {
        let source = self.source.clone();
        let open_store = Arc::clone(&self.open_store);
        Box::pin(async move {
            let ToolHostOptions {
                signal,
                emit,
                record_witness,
                local,
            } = options;
            let Some(local) = local else {
                return Err("the prepared pipeline runs only in the loop's own process".to_owned());
            };
            if let Some(recorder) = &record_witness {
                for raw in &call.calls {
                    recorder(ExecutionPipelineWitness {
                        pipeline: "prepared",
                        tool_call_id: raw.id.clone(),
                    })
                    .await;
                }
            }

            let local_context = Arc::new(local.context);
            let assistant_message = local.assistant_message;
            let mut outcomes: Vec<AgentToolCallOutcome> = Vec::new();
            let mut messages: Vec<ToolResultMessage> = Vec::new();

            async fn report(
                finished: Vec<AgentToolCallOutcome>,
                outcomes: &mut Vec<AgentToolCallOutcome>,
                messages: &mut Vec<ToolResultMessage>,
                emit: &Arc<
                    dyn Fn(crate::types::AgentEvent) -> BoxFuture<'static, ()> + Send + Sync,
                >,
            ) {
                for outcome in finished {
                    let message = create_tool_result_message(&outcome);
                    emit_tool_result_message(&message, emit).await;
                    outcomes.push(outcome);
                    messages.push(message);
                }
            }

            if call.mode == ToolExecutionMode::Sequential {
                for raw in call.calls {
                    let finished = run_prepared_batch(PreparedBatchRequest {
                        source: source.clone(),
                        calls: vec![raw],
                        scope: call.scope.clone(),
                        context: Arc::clone(&local_context),
                        assistant_message: assistant_message.clone(),
                        emit: Arc::clone(&emit),
                        signal: signal.clone(),
                        open_store: Arc::clone(&open_store),
                    })
                    .await?;
                    report(finished, &mut outcomes, &mut messages, &emit).await;
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        break;
                    }
                }
            } else {
                let finished = run_prepared_batch(PreparedBatchRequest {
                    source,
                    calls: call.calls,
                    scope: call.scope,
                    context: local_context,
                    assistant_message,
                    emit: Arc::clone(&emit),
                    signal,
                    open_store,
                })
                .await?;
                report(finished, &mut outcomes, &mut messages, &emit).await;
            }

            Ok(ToolHostResult {
                messages,
                terminate: should_terminate_tool_batch(&outcomes),
            })
        })
    }
}

pub fn create_prepared_tool_host(
    source: PreparationSource,
    open_store: BatchStoreFactory,
) -> Arc<dyn ToolHost> {
    Arc::new(PreparedToolHost { source, open_store })
}
