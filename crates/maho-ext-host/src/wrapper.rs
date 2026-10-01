use std::sync::Arc;
use maho_ext_api::*;

pub type ToolContextFactory = Arc<dyn Fn() -> Result<ExtensionContext, ExtensionFailure> + Send + Sync>;

pub fn wrap_registered_tool(registered: RegisteredTool, runtime: ExtensionRuntime, context_factory: ToolContextFactory) -> AgentTool {
    let context = Arc::clone(&context_factory);
    let mut definition = registered.definition;
    let execute_definition = Arc::clone(&definition.execute);
    definition.execute = Arc::new(move |call| {
        let context = Arc::clone(&context);
        let execute = Arc::clone(&execute_definition);
        Box::pin(async move {
            let context = context().map_err(|error| maho_ext_api::ToolError::Message(error.message))?;
            execute(maho_ext_api::ToolCall { id: call.id, params: call.params, signal: call.signal, on_update: call.on_update, context: Some(&context) }).await
        })
    });
    let mut tool = wrap_tool_definition(definition, None);
    let execute = Arc::clone(&tool.execute);
    tool.execute = Arc::new(move |id, params, signal, on_update| {
        let execute = Arc::clone(&execute);
        let runtime = runtime.clone();
        Box::pin(async move {
            let actions = match runtime.session_actions() {
                Ok(actions) => actions,
                Err(error) => { let mut result = AgentToolResult::text(error.message); result.is_error = Some(true); return result; }
            };
            let active_before = match actions.get_active_tools() {
                Ok(names) => names,
                Err(error) => { let mut result = AgentToolResult::text(error.message); result.is_error = Some(true); return result; }
            };
            let mut result = execute(id, params, signal, on_update).await;
            let active_after = match actions.get_active_tools() {
                Ok(names) => names,
                Err(error) => { result.is_error = Some(true); result.content.push(ContentBlock::text(error.message)); return result; }
            };
            if active_before.iter().all(|name| active_after.contains(name)) {
                let added: Vec<_> = active_after.into_iter().filter(|name| !active_before.contains(name)).collect();
                if !added.is_empty() {
                    let names = result.added_tool_names.get_or_insert_with(Vec::new);
                    for name in added { if !names.contains(&name) { names.push(name); } }
                }
            }
            result
        })
    });
    tool
}

pub fn wrap_registered_tools(registered: Vec<RegisteredTool>, runtime: ExtensionRuntime, context_factory: ToolContextFactory) -> Vec<AgentTool> {
    registered.into_iter().map(|tool| wrap_registered_tool(tool, runtime.clone(), Arc::clone(&context_factory))).collect()
}
