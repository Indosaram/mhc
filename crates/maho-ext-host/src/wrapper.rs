use std::sync::Arc;
use maho_ext_api::*;

pub type ToolContextFactory = Arc<dyn Fn() -> Result<ExtensionContext, ExtensionFailure> + Send + Sync>;

/// An invocation owns its context and releases host capabilities on every exit path.
pub struct ToolInvocation {
    pub context: ExtensionContext,
    dispose: Option<Box<dyn FnOnce() + Send>>,
}
impl ToolInvocation {
    pub fn new(context: ExtensionContext, dispose: impl FnOnce() + Send + 'static) -> Self {
        Self { context, dispose: Some(Box::new(dispose)) }
    }
}
impl Drop for ToolInvocation {
    fn drop(&mut self) {
        if let Some(dispose) = self.dispose.take() { dispose(); }
    }
}
pub type ToolInvocationFactory = Arc<dyn Fn(Option<AbortSignal>) -> Result<ToolInvocation, ExtensionFailure> + Send + Sync>;
struct AbortSubscription(maho_ai::utils::abort::AbortSignal, maho_ai::utils::abort::ListenerId);
impl Drop for AbortSubscription { fn drop(&mut self) { self.0.remove_abort_listener(self.1); } }

pub fn wrap_registered_tool(registered: RegisteredTool, runtime: ExtensionRuntime, context_factory: ToolContextFactory) -> AgentTool {
    wrap_registered_tool_with_invocation(registered, runtime, Arc::new(move |signal| {
        let mut context = context_factory()?;
        context.signal = signal;
        Ok(ToolInvocation::new(context, || {}))
    }))
}

pub fn wrap_registered_tool_with_invocation(registered: RegisteredTool, runtime: ExtensionRuntime, context_factory: ToolInvocationFactory) -> AgentTool {
    let extension_path = registered.source_info.path.clone();
    let tool_name = registered.definition.name.clone();
    let invocation_factory = context_factory.clone();
    let context = Arc::clone(&context_factory);
    let mut definition = registered.definition;
    let execute_definition = Arc::clone(&definition.execute);
    definition.execute = Arc::new(move |call| {
        let context = Arc::clone(&context);
        let execute = Arc::clone(&execute_definition);
        Box::pin(async move {
            let invocation = context(Some(call.signal.clone())).map_err(|error| maho_ext_api::ToolError::Message(error.message))?;
            execute(maho_ext_api::ToolCall { id: call.id, params: call.params, signal: call.signal, on_update: call.on_update, context: Some(&invocation.context) }).await
        })
    });
    let mut tool = wrap_tool_definition(definition, None);
    let execute = Arc::clone(&tool.execute);
    tool.execute = Arc::new(move |id, params, signal, on_update| {
        let execute = Arc::clone(&execute);
        let runtime = runtime.clone();
        let extension_execute = runtime.extension_tool_executor(&extension_path, &tool_name);
        let invocation_factory = invocation_factory.clone();
        Box::pin(async move {
            let actions = match runtime.session_actions() {
                Ok(actions) => actions,
                Err(error) => { let mut result = AgentToolResult::text(error.message); result.is_error = Some(true); return result; }
            };
            let active_before = match actions.get_active_tools() {
                Ok(names) => names,
                Err(error) => { let mut result = AgentToolResult::text(error.message); result.is_error = Some(true); return result; }
            };
            let mut result = if let Some(extension_execute) = extension_execute {
                let local = AbortSignal::default();
                let listener = signal.as_ref().map(|signal| {
                    let cancellation = local.clone();
                    let listener = signal.add_abort_listener(move |_| cancellation.abort());
                    if signal.aborted() { local.abort(); }
                    AbortSubscription(signal.clone(), listener)
                });
                let result = match invocation_factory(Some(local)) {
                    Ok(invocation) => extension_execute(&id, params, signal.clone(), on_update, &invocation.context).await,
                    Err(error) => Err(error),
                };
                drop(listener);
                match result { Ok(result) => result, Err(error) => { let mut result = AgentToolResult::text(error.message); result.is_error = Some(true); result } }
            } else { execute(id, params, signal, on_update).await };
            if let Err(error) = runtime.assert_active() {
                result.is_error = Some(true);
                result.content.push(ContentBlock::text(error.message));
                return result;
            }
            let active_after = match actions.get_active_tools() {
                Ok(names) => names,
                Err(error) => { result.is_error = Some(true); result.content.push(ContentBlock::text(error.message)); return result; }
            };
            if active_before.iter().all(|name| active_after.contains(name)) {
                let added: Vec<_> = active_after.into_iter().filter(|name| !active_before.contains(name)).collect();
                if !added.is_empty() {
                    let names = result.added_tool_names.get_or_insert_with(Vec::new);
                    names.extend(added);
                    let mut seen = std::collections::HashSet::new();
                    names.retain(|name| seen.insert(name.clone()));
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
