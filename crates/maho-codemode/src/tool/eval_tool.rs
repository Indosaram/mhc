use std::sync::Arc;
use maho_tools::definition::{ToolContent,ToolDefinition,ToolError,ToolExecutionMode,ToolResult};
use maho_ext_api::{AgentToolResult,ContentBlock};
use super::{eval_tool_options::{CreateEvalToolOptions,EvalCellInvocation},eval_request::{parse_eval_request,normalize_eval_summary},types::{EvalToolRequest,EvalDeadlineSeconds,create_eval_input_schema},run_eval_cell::run_eval_cell,detached_eval_result::{create_detached_control_result,create_eval_list_result},detached_cell_manager::EvalDetachedCellManager};

fn tool_result(result:AgentToolResult)->ToolResult {
    ToolResult {content:result.content.into_iter().filter_map(|content|match content {
        ContentBlock::Text(text)=>Some(ToolContent::text(text.text)),
        ContentBlock::Image(image)=>Some(ToolContent::Image {data:image.data,mime_type:image.mime_type}),
        _=>None,
    }).collect(),details:Some(result.details)}
}

pub fn create_eval_tool(options:Arc<CreateEvalToolOptions>) -> Result<ToolDefinition,String> {
    let settings=&options.settings;
    let deadlines=EvalDeadlineSeconds {run_budget_seconds:settings.run_budget_seconds,detach_after_seconds:settings.cell_timeout_seconds.min(settings.foreground_window_seconds),foreground_window_seconds:settings.foreground_window_seconds,hard_limit_seconds:settings.hard_limit_seconds};
    let parameters=create_eval_input_schema(&settings.languages,&deadlines).map_err(str::to_owned)?;
    let prompt=crate::prompt::eval_prompt::build_eval_prompt(&settings.languages,&options.prompt).map_err(|error|error.to_string())?;
    let mut definition=ToolDefinition::new("eval",&prompt.description,parameters,Arc::new(move |call| {
        let options=options.clone();
        Box::pin(async move {
            call.signal.check()?;
            let request=parse_eval_request(&call.params).map_err(|error|ToolError::Message(error.to_string()))?;
            let result=match request {
                EvalToolRequest::List=>{
                    let (live,recent)=options.cell_manager.lock().expect("cell manager lock").list();
                    Ok(create_eval_list_result(&live,&recent))
                }
                EvalToolRequest::Peek {cell_id}=>options.cell_manager.lock().expect("cell manager lock").peek(&cell_id).map(|snapshot|create_detached_control_result(&snapshot)),
                EvalToolRequest::Stop {cell_id}=>EvalDetachedCellManager::stop(&options.cell_manager,&cell_id,"Stopped detached eval cell").await.map(|snapshot|create_detached_control_result(&snapshot)),
                EvalToolRequest::Run(input)=>{
                    let enabled=match input.language {super::types::EvalLanguage::Js=>options.settings.languages.js,super::types::EvalLanguage::Py=>options.settings.languages.py,super::types::EvalLanguage::Rb=>options.settings.languages.rb,super::types::EvalLanguage::Jl=>options.settings.languages.jl};
                    if !enabled {return Err(ToolError::Message(format!("Unsupported eval language {:?}",input.language)));}
                    let kernel_manager=options.kernel_manager.clone();
                    let tracker=kernel_manager.execution_tracker();
                    if let Some(tracker)=tracker {tracker.assert_eval_execution_allowed().map_err(|error|ToolError::Message(error.to_string()))?;}
                    let controller=maho_ai::utils::abort::AbortController::new();
                    let signal=controller.signal();
                    let update=call.on_update.map(|update|Arc::new(move |result|{let _=update(tool_result(result));}) as super::eval_tool_options::CellUpdateCallback);
                    let context=call.context.map(|context|super::eval_tool_options::EvalInvocationContext {model:context.model().cloned(),cwd:context.cwd().into(),thinking_level:context.thinking_level(),goal_store_file:context.goal_store_file().map(std::path::PathBuf::from)});
                    let invocation=EvalCellInvocation {steering_signal:call.context.and_then(|context|context.get_steering_signal()),cell_id:call.id.into(),input,signal,on_update:update,mode:options.mode.clone(),model:call.context.and_then(|context|context.model()).cloned(),context};
                    let lifecycle_controller=controller.clone();
                    let execution=async move {
                        let operation=run_eval_cell(options,invocation);
                        tokio::pin!(operation);
                        tokio::select! {
                            result=&mut operation=>result,
                            ()=call.signal.cancelled()=>{controller.abort(None);operation.await}
                        }
                    };
                    if let Some(tracker)=tracker {
                        tracker.track_eval_execution(execution,lifecycle_controller).await.map_err(|error|ToolError::Message(error.to_string()))?
                    } else {execution.await}
                }
            }.map_err(ToolError::Message)?;
            Ok(tool_result(result))
        })
    }));
    definition.label="Eval".into();
    definition.prompt_snippet=Some(prompt.prompt_snippet);
    definition.prompt_guidelines=Some(prompt.prompt_guidelines);
    definition.execution_mode=Some(ToolExecutionMode::Sequential);
    definition.prepare_arguments=Some(Arc::new(|mut args| {
        if let Some(record)=args.as_object_mut() && !matches!(record.get("action").and_then(serde_json::Value::as_str),Some("peek"|"stop"|"list")) {
            let summary=record.get("summary").and_then(normalize_eval_summary);
            if let Some(summary)=summary {record.insert("summary".into(),summary.into());} else {record.remove("summary");}
        }
        Ok(args)
    }));
    Ok(definition)
}
