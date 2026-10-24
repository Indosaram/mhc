use maho_ext_api::*;
use std::sync::Arc;

pub type ToolContextFactory =
    Arc<dyn Fn() -> Result<ExtensionContext, ExtensionFailure> + Send + Sync>;

/// An invocation owns its context and releases host capabilities on every exit path.
pub struct ToolInvocation {
    pub context: ExtensionContext,
    dispose: Option<Box<dyn FnOnce() + Send>>,
}
impl ToolInvocation {
    pub fn new(context: ExtensionContext, dispose: impl FnOnce() + Send + 'static) -> Self {
        Self {
            context,
            dispose: Some(Box::new(dispose)),
        }
    }
}
impl Drop for ToolInvocation {
    fn drop(&mut self) {
        if let Some(dispose) = self.dispose.take() {
            dispose();
        }
    }
}
pub type ToolInvocationFactory =
    Arc<dyn Fn(Option<AbortSignal>) -> Result<ToolInvocation, ExtensionFailure> + Send + Sync>;
struct AbortSubscription(
    maho_ai::utils::abort::AbortSignal,
    maho_ai::utils::abort::ListenerId,
);
impl Drop for AbortSubscription {
    fn drop(&mut self) {
        self.0.remove_abort_listener(self.1);
    }
}

/// A dispatcher that always runs the definition's own body in this process (hosts with no tool child).
pub struct InProcessRunDispatch;

impl ToolRunDispatch for InProcessRunDispatch {
    fn run_tool<'a>(
        &'a self,
        call: ToolDispatchCall<'a>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<ToolResult, Box<dyn std::error::Error + Send + Sync>>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let result = (call.definition.execute)(maho_ext_api::ToolCall {
                id: &call.tool_call_id,
                params: call.params,
                signal: call.signal.unwrap_or_default(),
                on_update: call.on_update,
                context: call.ctx,
            })
            .await?;
            Ok(result)
        })
    }
}

/// `wrapRegisteredTool(registered, runner, dispatcher)`: the tool's body runs through `dispatcher` with `placement`, which
/// decides between this process (the definition's own body, with its extension context) and the session's tool child.
pub fn wrap_registered_tool(
    registered: RegisteredTool,
    runtime: ExtensionRuntime,
    context_factory: ToolContextFactory,
    dispatcher: Arc<dyn ToolRunDispatch>,
    placement: ToolPlacementInfo,
) -> AgentTool {
    wrap_registered_tool_with_invocation(
        registered,
        runtime,
        Arc::new(move |signal| {
            let mut context = context_factory()?;
            context.signal = signal;
            Ok(ToolInvocation::new(context, || {}))
        }),
        dispatcher,
        placement,
    )
}

pub fn wrap_registered_tool_with_invocation(
    registered: RegisteredTool,
    runtime: ExtensionRuntime,
    context_factory: ToolInvocationFactory,
    dispatcher: Arc<dyn ToolRunDispatch>,
    placement: ToolPlacementInfo,
) -> AgentTool {
    let extension_path = registered.source_info.path.clone();
    let tool_name = registered.definition.name.clone();
    let invocation_factory = context_factory.clone();
    let context = Arc::clone(&context_factory);
    let mut definition = registered.definition;
    let execute_definition = Arc::clone(&definition.execute);
    // The in-process body: what the dispatcher runs for a server-placed call, with the invocation's context the outer
    // body made (the same context a child job's `args` reads).
    let mut local_definition = definition.clone();
    local_definition.execute = execute_definition;
    let local_definition = Arc::new(local_definition);
    let source_path = extension_path.clone();
    definition.execute = Arc::new(move |call| {
        let (dispatcher, placement, local_definition, source_path) = (
            Arc::clone(&dispatcher),
            placement.clone(),
            Arc::clone(&local_definition),
            source_path.clone(),
        );
        let context = Arc::clone(&context);
        Box::pin(async move {
            let invocation = context(Some(call.signal.clone()))
                .map_err(|error| maho_ext_api::ToolError::Message(error.message))?;
            dispatcher
                .run_tool(ToolDispatchCall {
                    definition: local_definition.as_ref(),
                    placement,
                    tool_call_id: call.id.to_owned(),
                    params: call.params,
                    signal: Some(call.signal),
                    on_update: call.on_update,
                    ctx: Some(&invocation.context),
                    source_path: Some(source_path),
                })
                .await
                .map_err(|error| match error.downcast::<maho_ext_api::ToolError>() {
                    Ok(error) => *error,
                    Err(error) => maho_ext_api::ToolError::Message(error.to_string()),
                })
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
                Err(error) => {
                    let mut result = AgentToolResult::text(error.message);
                    result.is_error = Some(true);
                    return result;
                }
            };
            let active_before = match actions.get_active_tools() {
                Ok(names) => names,
                Err(error) => {
                    let mut result = AgentToolResult::text(error.message);
                    result.is_error = Some(true);
                    return result;
                }
            };
            let mut result = if let Some(extension_execute) = extension_execute {
                let local = AbortSignal::default();
                let listener = signal.as_ref().map(|signal| {
                    let cancellation = local.clone();
                    let listener = signal.add_abort_listener(move |_| cancellation.abort());
                    if signal.aborted() {
                        local.abort();
                    }
                    AbortSubscription(signal.clone(), listener)
                });
                let result = match invocation_factory(Some(local)) {
                    Ok(invocation) => {
                        extension_execute(
                            &id,
                            params,
                            signal.clone(),
                            on_update,
                            &invocation.context,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                drop(listener);
                match result {
                    Ok(result) => result,
                    Err(error) => {
                        let mut result = AgentToolResult::text(error.message);
                        result.is_error = Some(true);
                        result
                    }
                }
            } else {
                execute(id, params, signal, on_update).await
            };
            if let Err(error) = runtime.assert_active() {
                result.is_error = Some(true);
                result.content.push(ContentBlock::text(error.message));
                return result;
            }
            let active_after = match actions.get_active_tools() {
                Ok(names) => names,
                Err(error) => {
                    result.is_error = Some(true);
                    result.content.push(ContentBlock::text(error.message));
                    return result;
                }
            };
            if active_before.iter().all(|name| active_after.contains(name)) {
                let added: Vec<_> = active_after
                    .into_iter()
                    .filter(|name| !active_before.contains(name))
                    .collect();
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

pub fn wrap_registered_tools(
    registered: Vec<RegisteredTool>,
    runtime: ExtensionRuntime,
    context_factory: ToolContextFactory,
    dispatcher: Arc<dyn ToolRunDispatch>,
) -> Vec<AgentTool> {
    registered
        .into_iter()
        .map(|tool| {
            let placement = ToolPlacementInfo::from_definition(&tool.definition);
            wrap_registered_tool(
                tool,
                runtime.clone(),
                Arc::clone(&context_factory),
                Arc::clone(&dispatcher),
                placement,
            )
        })
        .collect()
}
