//! Port of senpi `packages/agent/src/execution/prepared/executor-body.ts`.

use std::sync::{Arc, Mutex};

use maho_ai::types::{AssistantMessage, ContentBlock};
use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;

use crate::invocation_input::{InvocationInput, invoke_with_input};
use crate::types::{
    AfterToolCallContext, AgentContext, AgentTool, AgentToolCall, AgentToolResult,
    AgentToolUpdateCallback,
};

use super::executor_types::PreparationPipeline;

#[derive(Clone)]
pub struct PreparedBody {
    pub tool: AgentTool,
    pub tool_call: AgentToolCall,
    pub args: Value,
    pub assistant_message: AssistantMessage,
    pub context: Arc<AgentContext>,
    pub requested_name: Option<String>,
    pub input: InvocationInput,
}

#[derive(Debug, Clone)]
pub struct BodyOutcome {
    pub result: AgentToolResult,
    pub is_error: bool,
}

pub fn error_result(message: impl Into<String>) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::text(message)],
        details: Value::Object(Default::default()),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

pub async fn run_body(
    body: &PreparedBody,
    signal: Option<AbortSignal>,
    on_update: AgentToolUpdateCallback,
) -> BodyOutcome {
    let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel::<AgentToolResult>();
    let update_sender = Arc::new(Mutex::new(Some(update_tx)));
    let on_update_drainer = Arc::clone(&on_update);
    let update_drainer = tokio::spawn(async move {
        while let Some(partial) = update_rx.recv().await {
            on_update_drainer(partial);
        }
    });

    let sender_cb = Arc::clone(&update_sender);
    let wrapped_update: AgentToolUpdateCallback = Arc::new(move |partial| {
        if let Some(tx) = sender_cb.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            let _ = tx.send(partial);
        }
    });

    let execution = invoke_with_input(body.input.clone(), || {
        (body.tool.execute)(
            body.tool_call.id.clone(),
            body.args.clone(),
            signal.clone(),
            Some(wrapped_update),
        )
    });

    let outcome = tokio::select! {
        settled = execution => {
            let is_error = settled.is_error == Some(true);
            BodyOutcome { result: settled, is_error }
        }
        _ = async {
            match &signal {
                Some(sig) => sig.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => {
            BodyOutcome {
                result: error_result("Tool execution aborted"),
                is_error: true,
            }
        }
    };

    drop(
        update_sender
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take(),
    );
    let _ = update_drainer.await;
    outcome
}

pub async fn finalize_body(
    body: &PreparedBody,
    executed: BodyOutcome,
    pipeline: &dyn PreparationPipeline,
    signal: Option<AbortSignal>,
) -> BodyOutcome {
    let BodyOutcome { result, is_error } = executed;
    let after = match invoke_with_input(body.input.clone(), || {
        pipeline.after_tool_call(
            AfterToolCallContext {
                assistant_message: body.assistant_message.clone(),
                tool_call: body.tool_call.clone(),
                args: body.args.clone(),
                result: result.clone(),
                is_error,
                context: Arc::clone(&body.context),
            },
            signal,
        )
    })
    .await
    {
        Ok(after) => after,
        Err(err) => {
            return BodyOutcome {
                result: error_result(err),
                is_error: true,
            };
        }
    };

    let Some(after) = after else {
        return BodyOutcome { result, is_error };
    };

    let structured_content = after.structured_content.or_else(|| {
        if after.content.is_some() {
            None
        } else {
            result.structured_content
        }
    });
    let merged = AgentToolResult {
        content: after.content.unwrap_or(result.content),
        details: after.details.unwrap_or(result.details),
        structured_content,
        usage: after.usage.or(result.usage),
        nested_calls: result.nested_calls,
        added_tool_names: result.added_tool_names,
        terminate: after.terminate.or(result.terminate),
        is_error: result.is_error,
    };
    BodyOutcome {
        result: merged,
        is_error: after.is_error.unwrap_or(is_error),
    }
}
