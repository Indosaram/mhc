use maho_ext_api::ToolRenderers;
use maho_ext_api::{ExtensionApi, ToolContent, ToolDefinition, ToolError, ToolResult};
use maho_tui::tui::Component;
use senpi_task::tools::{
    control::{
        renderers::{
            ToolRenderResultOptions as ControlRenderOptions, render_task_cancel_call,
            render_task_cancel_result, render_task_send_call, render_task_send_result,
        },
        tool_result::{AgentToolResult as ControlToolResult, ToolResultContent},
        types::{CancelResultDetails, SendResultDetails},
    },
    output::{
        renderers::{
            ToolRenderResultOptions as OutputRenderOptions, render_task_output_call,
            render_task_output_result,
        },
        types::TaskOutputDetails,
    },
    render::{
        Lines, LinesComponent, RendererTheme, ThemeColor, ToolContent as RenderContent,
        lines_component,
    },
    task::{
        call_renderer::{TaskCallArgs, render_task_call_lines},
        renderers::render_task_result_component,
        types::TaskToolDetails,
    },
};
use senpi_task::{
    agents::AgentDefinition,
    manager::TaskManager,
    tools::{
        control::{
            cancel::{
                TASK_CANCEL_DESCRIPTION, TaskCancelInput, run_task_cancel,
                task_cancel_params_schema,
            },
            send::{TASK_SEND_DESCRIPTION, run_task_send},
            send_schema::{TaskSendInput, task_send_params_schema},
            send_shutdown::TaskSendTeamRouting,
        },
        output::{
            output::{
                TASK_OUTPUT_DESCRIPTION, TaskOutputInput, run_task_output,
                task_output_params_schema,
            },
            types::TaskOutputDeps,
        },
        task::{
            argument_normalization::normalize_task_tool_arguments,
            description::{
                DescriptionInput, TASK_PROMPT_GUIDELINES, TASK_PROMPT_SNIPPET,
                build_task_tool_description,
            },
            execute::build_task_execute,
            execute_single::TaskExecuteDeps,
            execute_spec::TaskToolDeps,
            foreground_wait::{ForegroundWaitOptions, TaskToolContext},
            params::TASK_TOOL_PARAMS,
            spawn_policy::SpawnPolicyDeps,
            validation::{SpawnItemInput, SpawnParamsInput},
        },
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

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

fn native_result(result: impl Serialize) -> Result<ToolResult, ToolError> {
    Ok(serde_json::from_value(serde_json::to_value(result)?)?)
}

#[derive(Deserialize, Default)]
struct SpawnInput {
    prompt: Option<String>,
    category: Option<String>,
    subagent_type: Option<String>,
    model: Option<String>,
    task_summary: Option<String>,
    description: Option<String>,
    name: Option<String>,
    load_skills: Option<Vec<String>>,
    tools: Option<Vec<String>>,
    isolated: Option<bool>,
    apply: Option<bool>,
    merge: Option<senpi_task::state::IsolationMergeMode>,
    run_in_background: Option<bool>,
    tasks: Option<Vec<SpawnItem>>,
}
#[derive(Deserialize)]
struct SpawnItem {
    prompt: String,
    category: Option<String>,
    subagent_type: Option<String>,
    model: Option<String>,
    task_summary: Option<String>,
    description: Option<String>,
    name: Option<String>,
    load_skills: Option<Vec<String>>,
    tools: Option<Vec<String>>,
    isolated: Option<bool>,
    apply: Option<bool>,
    merge: Option<senpi_task::state::IsolationMergeMode>,
    run_in_background: Option<bool>,
}
impl From<SpawnInput> for SpawnParamsInput {
    fn from(input: SpawnInput) -> Self {
        Self {
            prompt: input.prompt,
            category: input.category,
            subagent_type: input.subagent_type,
            model: input.model,
            task_summary: input.task_summary,
            description: input.description,
            name: input.name,
            load_skills: input.load_skills,
            tools: input.tools,
            isolated: input.isolated,
            apply: input.apply,
            merge: input.merge,
            run_in_background: input.run_in_background,
            tasks: input.tasks.map(|items| {
                items
                    .into_iter()
                    .map(|item| SpawnItemInput {
                        prompt: item.prompt,
                        category: item.category,
                        subagent_type: item.subagent_type,
                        model: item.model,
                        task_summary: item.task_summary,
                        description: item.description,
                        name: item.name,
                        load_skills: item.load_skills,
                        tools: item.tools,
                        isolated: item.isolated,
                        apply: item.apply,
                        merge: item.merge,
                        run_in_background: item.run_in_background,
                    })
                    .collect()
            }),
        }
    }
}
pub fn register_task_tools(api: &mut ExtensionApi, deps: TaskToolsDeps) {
    register_task_tools_with_sync(api, deps, Arc::new(|| {}));
}
pub fn register_task_tools_with_sync(
    api: &mut ExtensionApi,
    deps: TaskToolsDeps,
    sync: Arc<dyn Fn() + Send + Sync>,
) {
    let deps = Arc::new(deps);
    let task_deps = deps.clone();
    let description = build_task_tool_description(&DescriptionInput {
        omo_config: &deps.omo_config,
        agents: &deps.agents,
    });
    let mut task = ToolDefinition::new(
        "task",
        &description,
        TASK_TOOL_PARAMS.clone(),
        Arc::new(move |call| {
            let deps = task_deps.clone();
            Box::pin(async move {
                let params =
                    SpawnParamsInput::from(serde_json::from_value::<SpawnInput>(call.params)?);
                let context = call
                    .context
                    .ok_or_else(|| ToolError::Message("task requires a session context".into()))?;
                let ctx = TaskToolContext {
                    model: context.model().map(|model| senpi_task::tools::task::foreground_wait::ParentModel { provider: model.provider.clone(), id: model.id.clone() }),
                    cwd: context.cwd().to_string_lossy().into_owned(),
                    session_id: context.session_manager().session_id().into(),
                    get_prompt_cache_safe_wait_seconds: {
                        let budget = context
                            .get_prompt_cache_safe_wait_seconds()
                            .filter(|budget| budget.is_finite())
                            .map(|budget| budget.trunc() as i64);
                        Some(Arc::new(move || budget))
                    },
                };
                let mut spawn = deps.spawn.clone();
                if let Some(start) = spawn.start.take() {
                    let capability = maho_ext_api::current_kernel_tools();
                    spawn.start = Some(Arc::new(move |spec, names| {
                        maho_ext_api::with_kernel_tools_sync(capability.clone(), || {
                            start(spec, names)
                        })
                    }));
                }
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
                    callback(ToolResult { content: partial.content.into_iter().map(|content| ToolContent::text(content.text)).collect(), details: Some(details), structured_content: None, is_error: None })
                });
                if let Err(error) = result { *progress_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error); update_abort.abort(); }
            }) as Arc<senpi_task::tools::task::execute_single::TaskToolUpdate>);
                let (sender, receiver) = tokio::sync::oneshot::channel();
                let worker = std::thread::spawn(move || {
                    let execute = build_task_execute(
                        TaskExecuteDeps {
                            manager: &deps.manager,
                            tool: &spawn,
                            policy: deps.policy.as_ref(),
                        },
                        ForegroundWaitOptions::default(),
                    );
                    let result = execute.execute(&id, &params, Some(&signal), on_update, &ctx);
                    let _ = sender.send(result);
                });
                let result = crate::worker::settle(
                    receiver,
                    worker,
                    &call.signal,
                    || {
                        abort.abort();
                        Ok(())
                    },
                    "task executor panicked",
                )
                .await?;
                if let Some(error) = update_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    return Err(error);
                }
                let result = result.map_err(|error| ToolError::Message(error.to_string()))?;
                native_result(result)
            })
        }),
    );
    task.label = "Task".into();
    task.prompt_snippet = Some(TASK_PROMPT_SNIPPET.into());
    task.prompt_guidelines = Some(
        TASK_PROMPT_GUIDELINES
            .iter()
            .map(|line| (*line).into())
            .collect(),
    );
    task.prepare_arguments = Some(Arc::new(|raw| {
        normalize_task_tool_arguments(&raw).map_err(|error| ToolError::Message(error.message))
    }));
    register_synced_tool(api, task, sync.clone(), task_tool_renderers());
    let send_deps = deps.clone();
    let mut send = ToolDefinition::new(
        "task_send",
        TASK_SEND_DESCRIPTION,
        task_send_params_schema(),
        Arc::new(move |call| {
            let deps = send_deps.clone();
            Box::pin(async move {
                let params: TaskSendInput = serde_json::from_value(call.params)?;
                let session = call
                    .context
                    .map(|context| context.session_manager().session_id());
                let routing = deps
                    .team_routing
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let result =
                    run_task_send(deps.manager.as_ref(), &params, session, routing.as_ref())
                        .map_err(|error| ToolError::Message(error.to_string()))?;
                native_result(result)
            })
        }),
    );
    send.label = "Task Send".into();
    register_synced_tool(api, send, sync.clone(), task_send_renderers());
    let cancel_deps = deps.clone();
    let mut cancel = ToolDefinition::new(
        "task_cancel",
        TASK_CANCEL_DESCRIPTION,
        task_cancel_params_schema(),
        Arc::new(move |call| {
            let deps = cancel_deps.clone();
            Box::pin(async move {
                let params: TaskCancelInput = serde_json::from_value(call.params)?;
                native_result(run_task_cancel(deps.manager.as_ref(), &params))
            })
        }),
    );
    cancel.label = "Task Cancel".into();
    register_synced_tool(api, cancel, sync.clone(), task_cancel_renderers());
    let mut output = ToolDefinition::new(
        "task_output",
        TASK_OUTPUT_DESCRIPTION,
        task_output_params_schema(),
        Arc::new(move |call| {
            let deps = deps.clone();
            Box::pin(async move {
                let params: TaskOutputInput = serde_json::from_value(call.params)?;
                let session = call
                    .context
                    .map(|context| context.session_manager().session_id());
                let output_deps = TaskOutputDeps {
                    manager: deps.manager.clone(),
                    state_dir: deps.state_dir.clone(),
                    transcript_reader: None,
                    resolve_caller_session_id: None,
                    now: None,
                };
                native_result(run_task_output(&output_deps, &params, session)?)
            })
        }),
    );
    output.label = "Task Output".into();
    register_synced_tool(api, output, sync, task_output_renderers());
}
fn register_synced_tool(
    api: &mut ExtensionApi,
    mut tool: ToolDefinition,
    sync: Arc<dyn Fn() + Send + Sync>,
    renderers: ToolRenderers<(), Value>,
) {
    let execute = tool.execute.clone();
    tool.execute = Arc::new(move |call| {
        let future = execute(call);
        let sync = sync.clone();
        Box::pin(async move {
            let result = future.await;
            sync();
            result
        })
    });
    if let Err(error) = api.register_tool_with_renderers(tool, renderers) {
        std::panic::panic_any(error);
    }
}

/// senpi `Theme` seen through the `Pick<Theme, "fg" | "italic">` slice the senpi-task renderers use.
struct RenderTheme(maho_ext_api::Theme);
impl RendererTheme for RenderTheme {
    fn fg(&self, color: ThemeColor, text: &str) -> String {
        self.0.fg(color.as_str(), text)
    }
    fn italic(&self, text: &str) -> String {
        if text.is_empty() {
            String::new()
        } else {
            format!("\x1b[3m{text}\x1b[23m")
        }
    }
}

/// `linesComponent`/the renderer components: lines produced for the width the host asks for.
struct RenderedLines(Box<dyn Fn(usize) -> Vec<String>>);
impl Component for RenderedLines {
    fn render(&mut self, width: usize) -> Vec<String> {
        (self.0)(width)
    }
}
fn rendered(
    theme: &maho_ext_api::Theme,
    render: impl Fn(usize, &RenderTheme) -> Vec<String> + 'static,
) -> Box<dyn Component> {
    let theme = RenderTheme(theme.clone());
    Box::new(RenderedLines(Box::new(move |width| render(width, &theme))))
}
fn result_text(result: &maho_ext_api::AgentToolResult) -> Vec<RenderContent> {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            maho_ext_api::ContentBlock::Text(text) => Some(RenderContent::Text {
                text: text.text.clone(),
            }),
            _ => None,
        })
        .collect()
}
/// senpi falls back to the default result body when a custom renderer throws; details that do not
/// decode to the tool's typed details take that same fallback.
fn fallback_result(result: &maho_ext_api::AgentToolResult) -> Box<dyn Component> {
    let lines: Vec<String> = result_text(result)
        .into_iter()
        .flat_map(|RenderContent::Text { text }| {
            text.split('\n').map(str::to_owned).collect::<Vec<_>>()
        })
        .collect();
    Box::new(RenderedLines(Box::new(move |_| lines.clone())))
}
fn typed_details<T: serde::de::DeserializeOwned>(
    result: &maho_ext_api::AgentToolResult,
) -> Option<T> {
    serde_json::from_value(result.details.clone()).ok()
}
fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// `tools/task/tool.ts` `renderCall`/`renderResult`.
pub fn task_tool_renderers() -> ToolRenderers<(), Value> {
    ToolRenderers {
        render_call: Some(Arc::new(|args, theme, _| {
            let args = TaskCallArgs {
                prompt: string_arg(args, "prompt"),
                task_summary: string_arg(args, "task_summary"),
                category: string_arg(args, "category"),
                subagent_type: string_arg(args, "subagent_type"),
                run_in_background: args.get("run_in_background").and_then(Value::as_bool),
            };
            rendered(theme, move |width, theme| {
                render_task_call_lines(&args, theme, Some(width))
                    .into_iter()
                    .map(|line| theme.fg(ThemeColor::ToolTitle, &line))
                    .collect()
            })
        })),
        render_result: Some(Arc::new(|result, options, theme, _| {
            if options.is_partial {
                // The live status line is drawn by senpi's own tool-progress renderer from
                // details.progress; the partial content carries only the last-assistant row.
                let live = result_text(result)
                    .into_iter()
                    .map(|RenderContent::Text { text }| text)
                    .next()
                    .filter(|text| !text.is_empty());
                return rendered(theme, move |width, theme| {
                    let lines = live
                        .as_deref()
                        .map(|text| {
                            text.split('\n')
                                .map(|line| theme.fg(ThemeColor::Muted, line))
                                .collect()
                        })
                        .unwrap_or_default();
                    lines_component(Lines::Static(lines)).render(width)
                });
            }
            let Some(details) = typed_details::<TaskToolDetails>(result) else {
                return fallback_result(result);
            };
            rendered(theme, move |width, theme| {
                render_task_result_component(&details, theme).render(width)
            })
        })),
    }
}

/// `tools/control/send.ts` `createTaskSendTool` renderers.
pub fn task_send_renderers() -> ToolRenderers<(), Value> {
    ToolRenderers {
        render_call: Some(Arc::new(|args, theme, _| {
            match serde_json::from_value::<TaskSendInput>(args.clone()) {
                Ok(args) => rendered(theme, move |width, theme| {
                    render_task_send_call(&args, theme).render(width)
                }),
                Err(_) => Box::new(RenderedLines(Box::new(|_| vec!["task_send".into()]))),
            }
        })),
        render_result: Some(Arc::new(|result, options, theme, _| {
            let Some(details) = typed_details::<SendResultDetails>(result) else {
                return fallback_result(result);
            };
            let result = ControlToolResult {
                content: result_text(result)
                    .into_iter()
                    .map(|RenderContent::Text { text }| ToolResultContent::Text { text })
                    .collect(),
                details,
            };
            let options = ControlRenderOptions {
                expanded: options.expanded,
                is_partial: options.is_partial,
            };
            rendered(theme, move |width, theme| {
                render_task_send_result(&result, &options, theme).render(width)
            })
        })),
    }
}

/// `tools/control/cancel.ts` renderers.
pub fn task_cancel_renderers() -> ToolRenderers<(), Value> {
    ToolRenderers {
        render_call: Some(Arc::new(|args, theme, _| {
            let args = serde_json::from_value::<TaskCancelInput>(args.clone()).unwrap_or_default();
            rendered(theme, move |width, theme| {
                render_task_cancel_call(&args, theme).render(width)
            })
        })),
        render_result: Some(Arc::new(|result, options, theme, _| {
            let Some(details) = typed_details::<CancelResultDetails>(result) else {
                return fallback_result(result);
            };
            let result = ControlToolResult {
                content: result_text(result)
                    .into_iter()
                    .map(|RenderContent::Text { text }| ToolResultContent::Text { text })
                    .collect(),
                details,
            };
            let options = ControlRenderOptions {
                expanded: options.expanded,
                is_partial: options.is_partial,
            };
            rendered(theme, move |width, theme| {
                render_task_cancel_result(&result, &options, theme).render(width)
            })
        })),
    }
}

/// `tools/output/output.ts` renderers.
pub fn task_output_renderers() -> ToolRenderers<(), Value> {
    ToolRenderers {
        render_call: Some(Arc::new(|args, theme, _| {
            let args = serde_json::from_value::<TaskOutputInput>(args.clone()).unwrap_or_default();
            rendered(theme, move |width, theme| {
                render_task_output_call(&args, theme).render(width)
            })
        })),
        render_result: Some(Arc::new(|result, options, theme, _| {
            let Some(details) = typed_details::<TaskOutputDetails>(result) else {
                return fallback_result(result);
            };
            let result = ControlToolResult {
                content: result_text(result)
                    .into_iter()
                    .map(|RenderContent::Text { text }| ToolResultContent::Text { text })
                    .collect(),
                details,
            };
            let options = OutputRenderOptions {
                expanded: options.expanded,
                is_partial: options.is_partial,
            };
            rendered(theme, move |width, theme| {
                render_task_output_result(&result, &options, theme).render(width)
            })
        })),
    }
}

pub fn tool_text(text: String, details: Value) -> ToolResult {
    ToolResult {
        content: vec![ToolContent::text(text)],
        details: Some(details),
        structured_content: None,
        is_error: None,
    }
}

pub fn register_lead_team_tools(
    api: &mut ExtensionApi,
    service: Arc<dyn senpi_task::tools::team::types::TeamToolsService>,
) {
    let deps = senpi_task::tools::team::types::TeamToolDeps { service };
    for tool in senpi_task::tools::team::index::build_lead_team_tools(&deps) {
        let name = tool.name();
        let label = tool.label();
        let description = tool.description();
        let parameters = tool.parameters().clone();
        let tool = Arc::new(MutexTeamTool(std::sync::Mutex::new(tool)));
        let mut definition = ToolDefinition::new(
            name,
            description,
            parameters,
            Arc::new(move |call| {
                let tool = tool.clone();
                Box::pin(async move {
                    let result = tool
                        .0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .execute_json(call.id, &call.params)
                        .map_err(|error| ToolError::Message(error.to_string()))?;
                    Ok(serde_json::from_value(result)?)
                })
            }),
        );
        definition.label = label.into();
        api.register_tool(definition);
    }
}
struct MutexTeamTool(std::sync::Mutex<senpi_task::tools::team::index::LeadTeamTool>);
