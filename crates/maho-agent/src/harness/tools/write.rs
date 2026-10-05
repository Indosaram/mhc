use super::{
    file_mutation_queue::with_file_mutation_queue,
    path_utils::resolve_tool_path,
    post_mutate::{append_post_mutate_note, run_post_mutate},
    tool_context::{HasExecutionToolContext, MutationTool, PostMutateContext},
};
use crate::{harness::types::AgentHarnessTool, types::AgentToolResult};
use maho_ai::types::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteToolInput {
    pub path: String,
    pub content: String,
}
pub fn create_write_tool<T: HasExecutionToolContext>() -> AgentHarnessTool<T> {
    AgentHarnessTool {
        label: "write".into(), prepare_arguments: None, replay: None,
        tool: Tool { name: "write".into(), description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".into(), parameters: json!({"type":"object","properties":{"path":{"type":"string","description":"Path to the file to write (relative or absolute)"},"content":{"type":"string","description":"Content to write to the file"}},"required":["path","content"]}), freeform: None, constrained_sampling: None },
        execute: Arc::new(|_, input, _, turn: T, _, context| Box::pin(async move {
            let input: WriteToolInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            let tool_context = turn.execution_tool_context();
            let env = &tool_context.env;
            let absolute = resolve_tool_path(env.as_ref(), &input.path, &context).await?;
            with_file_mutation_queue(env, &absolute, || async {
                if context.is_aborted() { return Err("Operation aborted".into()); }
                env.write_file(&absolute, input.content.as_bytes(), &context).await.map_err(|e| e.to_string())?;
                if context.is_aborted() { return Err("Operation aborted".into()); }
                let outcome = run_post_mutate(tool_context.post_mutate.as_ref(), PostMutateContext { tool: MutationTool::Write, path: absolute.clone(), signal: context.abort_signal() }).await;
                if context.is_aborted() { return Err("Operation aborted".into()); }
                let mut result = AgentToolResult::text(append_post_mutate_note(&format!("Successfully wrote to {}", input.path), &[outcome.note]));
                result.details = serde_json::Value::Null;
                Ok(result)
            }, &context).await
        })),
    }
}
