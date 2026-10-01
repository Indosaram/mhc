use std::sync::Arc;
use maho_agent::{AgentTool,AgentToolResult,ToolExecutionMode as AgentExecutionMode};
use maho_ai::types::{ContentBlock,ImageContent,TextAudience,TextContent};
use crate::definition::*;
pub type ContextFactory = Arc<dyn Fn() -> Arc<dyn ToolContext> + Send + Sync>;
fn into_agent_result(result: ToolResult) -> AgentToolResult {
    AgentToolResult { content:result.content.into_iter().map(|part| match part {
        ToolContent::Text { text,audience } => ContentBlock::Text(TextContent { text,audience:audience.map(|_| TextAudience::Model),text_signature:None }),
        ToolContent::Image { data,mime_type } => ContentBlock::Image(ImageContent { data,mime_type }),
    }).collect(),details:result.details.unwrap_or(serde_json::Value::Null),usage:None,added_tool_names:None,terminate:None,is_error:None }
}
fn from_agent_result(result: AgentToolResult) -> Result<ToolResult,ToolError> {
    if result.is_error == Some(true) {
        let message = result.content.iter().filter_map(|part| match part { ContentBlock::Text(text) => Some(text.text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
        return Err(ToolError::Message(message));
    }
    let mut content = Vec::new();
    for part in result.content {
        match part {
            ContentBlock::Text(text) => content.push(ToolContent::Text { text:text.text,audience:text.audience.map(|_| "model".into()) }),
            ContentBlock::Image(image) => content.push(ToolContent::Image { data:image.data,mime_type:image.mime_type }),
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => return Err(ToolError::Message("Tool results must contain text or image content".into())),
        }
    }
    Ok(ToolResult { content,details:(!result.details.is_null()).then_some(result.details) })
}
pub fn wrap_tool_definition(definition: ToolDefinition,context_factory: Option<ContextFactory>) -> AgentTool {
    let prepare = definition.prepare_arguments.clone().map(|prepare| Arc::new(move |args| match prepare(args) { Ok(args) => args, Err(error) => serde_json::json!({"error":error.to_string()}) }) as maho_agent::ToolArgumentShim);
    let execute = definition.execute;
    AgentTool { label:definition.label,prepare_arguments:prepare,replay:None,execution_mode:definition.execution_mode.map(|mode| match mode { ToolExecutionMode::Sequential => AgentExecutionMode::Sequential,ToolExecutionMode::Parallel => AgentExecutionMode::Parallel }),
        tool:maho_ai::types::Tool { name:definition.name,description:definition.description,parameters:definition.parameters,freeform:definition.freeform,constrained_sampling:definition.constrained_sampling },
        execute:Arc::new(move |id,params,signal,on_update| {
            let execute = Arc::clone(&execute); let context = context_factory.as_ref().map(|factory| factory());
            Box::pin(async move {
                let local_signal = AbortSignal::default();
                let listener = signal.as_ref().map(|signal| { let local = local_signal.clone(); let listener = signal.add_abort_listener(move |_| local.abort()); if signal.aborted() { local_signal.abort(); } listener });
                let on_update = on_update.map(|update| Arc::new(move |result| { update(into_agent_result(result)); Ok(()) }) as ToolUpdateCallback);
                let result = execute(ToolCall { id:&id,params,signal:local_signal,on_update,context:context.as_deref() }).await;
                if let (Some(signal),Some(listener)) = (signal,listener) { signal.remove_abort_listener(listener); }
                match result { Ok(result) => into_agent_result(result),Err(error) => { let mut result = AgentToolResult::text(error.to_string()); result.is_error = Some(true); result } }
            })
        }) }
}
pub fn wrap_tool_definitions(definitions: Vec<ToolDefinition>,context_factory: Option<ContextFactory>) -> Vec<AgentTool> {
    definitions.into_iter().map(|definition| wrap_tool_definition(definition,context_factory.clone())).collect()
}
pub fn create_tool_definition_from_agent_tool(tool: AgentTool) -> ToolDefinition {
    let execute = tool.execute; let shim = tool.prepare_arguments;
    let mut definition = ToolDefinition::new(&tool.tool.name,&tool.tool.description,tool.tool.parameters,Arc::new(move |call| {
        let execute = Arc::clone(&execute);
        Box::pin(async move {
            let controller = maho_ai::utils::abort::AbortController::new();
            if call.signal.is_aborted() { controller.abort(None); }
            let update = call.on_update.map(|update| Arc::new(move |result| { match from_agent_result(result).and_then(|result| update(result)) { Ok(()) => {},Err(error) => eprintln!("Tool update failed: {error}") } }) as maho_agent::AgentToolUpdateCallback);
            let operation = execute(call.id.into(),call.params,Some(controller.signal()),update);
            tokio::pin!(operation);
            tokio::select! {
                result = &mut operation => from_agent_result(result),
                () = call.signal.cancelled() => { controller.abort(None); from_agent_result(operation.await) },
            }
        })
    }));
    definition.label = tool.label; definition.freeform = tool.tool.freeform; definition.constrained_sampling = tool.tool.constrained_sampling;
    definition.prepare_arguments = shim.map(|shim| Arc::new(move |args| Ok(shim(args))) as PrepareArguments);
    definition.execution_mode = tool.execution_mode.map(|mode| match mode { AgentExecutionMode::Sequential => ToolExecutionMode::Sequential,AgentExecutionMode::Parallel => ToolExecutionMode::Parallel }); definition
}
