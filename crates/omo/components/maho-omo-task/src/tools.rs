use std::{collections::BTreeMap, sync::Arc};
use maho_ext_api::{ExtensionApi, ToolContent, ToolDefinition, ToolError, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use senpi_task::{agents::AgentDefinition, manager::TaskManager, tools::{control::{cancel::{TASK_CANCEL_DESCRIPTION, TaskCancelInput, run_task_cancel, task_cancel_params_schema}, send::{TASK_SEND_DESCRIPTION, run_task_send}, send_schema::{TaskSendInput, task_send_params_schema}, send_shutdown::TaskSendTeamRouting}, output::{output::{TASK_OUTPUT_DESCRIPTION, TaskOutputInput, run_task_output, task_output_params_schema}, types::TaskOutputDeps}, task::{argument_normalization::normalize_task_tool_arguments, description::{DescriptionInput, TASK_PROMPT_GUIDELINES, TASK_PROMPT_SNIPPET, build_task_tool_description}, execute::build_task_execute, execute_single::TaskExecuteDeps, execute_spec::TaskToolDeps, foreground_wait::{ForegroundWaitOptions, TaskToolContext}, params::TASK_TOOL_PARAMS, spawn_policy::SpawnPolicyDeps, validation::{SpawnItemInput, SpawnParamsInput}}}};

#[derive(Clone)]
pub struct TaskToolsDeps {
    pub manager: Arc<TaskManager>,
    pub state_dir: String,
    pub omo_config: Value,
    pub agents: BTreeMap<String, AgentDefinition>,
    pub spawn: TaskToolDeps,
    pub policy: Arc<dyn SpawnPolicyDeps + Send + Sync>,
    pub team_routing: Arc<std::sync::Mutex<Option<TaskSendTeamRouting>>>,
}
fn native_result(result: impl Serialize) -> Result<ToolResult, ToolError> { Ok(serde_json::from_value(serde_json::to_value(result)?)?) }

#[derive(Deserialize, Default)]
struct SpawnInput {
    prompt: Option<String>, category: Option<String>, subagent_type: Option<String>, model: Option<String>,
    task_summary: Option<String>, description: Option<String>, name: Option<String>, load_skills: Option<Vec<String>>,
    isolated: Option<bool>, apply: Option<bool>, merge: Option<senpi_task::state::IsolationMergeMode>,
    run_in_background: Option<bool>, tasks: Option<Vec<SpawnItem>>,
}
#[derive(Deserialize)]
struct SpawnItem {
    prompt: String, category: Option<String>, subagent_type: Option<String>, model: Option<String>,
    task_summary: Option<String>, description: Option<String>, name: Option<String>, load_skills: Option<Vec<String>>,
    isolated: Option<bool>, apply: Option<bool>, merge: Option<senpi_task::state::IsolationMergeMode>,
}
impl From<SpawnInput> for SpawnParamsInput {
    fn from(input: SpawnInput) -> Self {
        Self { prompt: input.prompt, category: input.category, subagent_type: input.subagent_type, model: input.model, task_summary: input.task_summary, description: input.description, name: input.name, load_skills: input.load_skills, isolated: input.isolated, apply: input.apply, merge: input.merge, run_in_background: input.run_in_background, tasks: input.tasks.map(|items| items.into_iter().map(|item| SpawnItemInput { prompt: item.prompt, category: item.category, subagent_type: item.subagent_type, model: item.model, task_summary: item.task_summary, description: item.description, name: item.name, load_skills: item.load_skills, isolated: item.isolated, apply: item.apply, merge: item.merge }).collect()) }
    }
}
pub fn register_task_tools(api: &mut ExtensionApi, deps: TaskToolsDeps) {
    register_task_tools_with_sync(api, deps, Arc::new(|| {}));
}
pub fn register_task_tools_with_sync(api: &mut ExtensionApi, deps: TaskToolsDeps, sync: Arc<dyn Fn() + Send + Sync>) {
    let deps = Arc::new(deps);
    let task_deps = deps.clone();
    let description = build_task_tool_description(&DescriptionInput { omo_config: &deps.omo_config, agents: &deps.agents });
    let mut task = ToolDefinition::new("task", &description, TASK_TOOL_PARAMS.clone(), Arc::new(move |call| {
        let deps = task_deps.clone();
        Box::pin(async move {
            let params = SpawnParamsInput::from(serde_json::from_value::<SpawnInput>(call.params)?);
            let context = call.context.ok_or_else(|| ToolError::Message("task requires a session context".into()))?;
            let ctx = TaskToolContext { cwd: context.cwd().to_string_lossy().into_owned(), session_id: context.session_manager().session_id().into(), get_prompt_cache_safe_wait_seconds: None };
            call.signal.check()?;
            let id = call.id.to_owned();
            let signal = senpi_task::manager::AbortSignal::default();
            let abort = signal.clone();
            let update_abort = signal.clone();
            let update_error = Arc::new(std::sync::Mutex::new(None));
            let progress_error = update_error.clone();
            let on_update = call.on_update.map(|callback| Arc::new(move |partial: senpi_task::tools::task::execute_single::TaskToolPartialResult| {
                let result = serde_json::to_value(partial.details.details).map_err(ToolError::from).and_then(|mut details| {
                    let progress = serde_json::to_value(partial.details.progress)?;
                    if let (Some(details), Some(progress)) = (details.as_object_mut(), progress.as_object()) { details.extend(progress.clone()); }
                    callback(ToolResult { content: partial.content.into_iter().map(|content| ToolContent::text(content.text)).collect(), details: Some(details) })
                });
                if let Err(error) = result { *progress_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error); update_abort.abort(); }
            }) as Arc<senpi_task::tools::task::execute_single::TaskToolUpdate>);
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let worker = std::thread::spawn(move || {
                let execute = build_task_execute(TaskExecuteDeps { manager: &deps.manager, tool: &deps.spawn, policy: deps.policy.as_ref() }, ForegroundWaitOptions::default());
                let result = execute.execute(&id, &params, Some(&signal), on_update, &ctx);
                let _ = sender.send(result);
            });
            let result = crate::worker::settle(receiver, worker, &call.signal, || {
                abort.abort();
                Ok(())
            }, "task executor panicked").await?;
            if let Some(error) = update_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() { return Err(error); }
            let result = result.map_err(|error| ToolError::Message(error.to_string()))?;
            native_result(result)
        })
    }));
    task.label = "Task".into(); task.prompt_snippet = Some(TASK_PROMPT_SNIPPET.into()); task.prompt_guidelines = Some(TASK_PROMPT_GUIDELINES.iter().map(|line| (*line).into()).collect());
    task.prepare_arguments = Some(Arc::new(|raw| normalize_task_tool_arguments(&raw).map_err(|error| ToolError::Message(error.message))));
    register_synced_tool(api, task, sync.clone());
    let send_deps = deps.clone();
    let mut send = ToolDefinition::new("task_send", TASK_SEND_DESCRIPTION, task_send_params_schema(), Arc::new(move |call| {
        let deps = send_deps.clone();
        Box::pin(async move {
            let params: TaskSendInput = serde_json::from_value(call.params)?;
            let session = call.context.map(|context| context.session_manager().session_id());
            let routing = deps.team_routing.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            let result = run_task_send(deps.manager.as_ref(), &params, session, routing.as_ref()).map_err(|error| ToolError::Message(error.to_string()))?;
            native_result(result)
        })
    })); send.label = "Task Send".into(); register_synced_tool(api, send, sync.clone());
    let cancel_deps = deps.clone();
    let mut cancel = ToolDefinition::new("task_cancel", TASK_CANCEL_DESCRIPTION, task_cancel_params_schema(), Arc::new(move |call| {
        let deps = cancel_deps.clone();
        Box::pin(async move { let params: TaskCancelInput = serde_json::from_value(call.params)?; native_result(run_task_cancel(deps.manager.as_ref(), &params)) })
    })); cancel.label = "Task Cancel".into(); register_synced_tool(api, cancel, sync.clone());
    let mut output = ToolDefinition::new("task_output", TASK_OUTPUT_DESCRIPTION, task_output_params_schema(), Arc::new(move |call| {
        let deps = deps.clone();
        Box::pin(async move {
            let params: TaskOutputInput = serde_json::from_value(call.params)?;
            let session = call.context.map(|context| context.session_manager().session_id());
            let output_deps = TaskOutputDeps { manager: deps.manager.clone(), state_dir: deps.state_dir.clone(), transcript_reader: None, resolve_caller_session_id: None, now: None };
            native_result(run_task_output(&output_deps, &params, session)?)
        })
    })); output.label = "Task Output".into(); register_synced_tool(api, output, sync);
}
fn register_synced_tool(api: &mut ExtensionApi, mut tool: ToolDefinition, sync: Arc<dyn Fn() + Send + Sync>) {
        let execute = tool.execute.clone();
        tool.execute = Arc::new(move |call| {
            let future = execute(call);
            let sync = sync.clone();
            Box::pin(async move { let result = future.await; sync(); result })
        });
        api.register_tool(tool);
}

pub fn tool_text(text: String, details: Value) -> ToolResult { ToolResult { content: vec![ToolContent::text(text)], details: Some(details) } }

pub fn register_lead_team_tools(api: &mut ExtensionApi, service: Arc<dyn senpi_task::tools::team::types::TeamToolsService>) {
    let deps = senpi_task::tools::team::types::TeamToolDeps { service };
    for tool in senpi_task::tools::team::index::build_lead_team_tools(&deps) {
        let name = tool.name(); let label = tool.label(); let description = tool.description(); let parameters = tool.parameters().clone();
        let tool = Arc::new(MutexTeamTool(std::sync::Mutex::new(tool)));
        let mut definition = ToolDefinition::new(name, description, parameters, Arc::new(move |call| {
            let tool = tool.clone();
            Box::pin(async move {
                let result = tool.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).execute_json(call.id, &call.params).map_err(|error| ToolError::Message(error.to_string()))?;
                Ok(serde_json::from_value(result)?)
            })
        }));
        definition.label = label.into(); api.register_tool(definition);
    }
}
struct MutexTeamTool(std::sync::Mutex<senpi_task::tools::team::index::LeadTeamTool>);
