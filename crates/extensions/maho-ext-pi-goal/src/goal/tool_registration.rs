use maho_ext_api::{ExtensionContext,ExtensionFailure,ExtensionFuture,ToolContent};
use super::{format::format_goal_tool_response,store::{create_goal,objective_full_text_file_name,read_goal,update_goal},types::*,ui::update_goal_ui,validation::{js_whitespace,objective_truncation_notice,validate_objective}};
use std::sync::Arc;
pub fn register_goal_tools(api:&mut maho_ext_api::ExtensionApi,deps:Arc<dyn GoalToolRegistrationDeps>)->Result<(),ExtensionFailure>{
    for (name,label,description,parameters) in [
        ("create_goal","Create Goal","Create a goal only when explicitly requested by the user or system/developer instructions; do not infer goals from ordinary tasks.\nObjectives are limited to 4,000 characters. For longer instructions, put the full objective in a file and refer to that file.\nReplaces the current goal when it is complete and archives it; fails if an unfinished goal exists.",serde_json::json!({"type":"object","required":["objective"],"properties":{"objective":{"type":"string","description":"Required. The concrete objective to start pursuing. Limit: 4,000 characters. For longer instructions, put the full objective in a file and refer to that file."}},"additionalProperties":false})),
        ("update_goal","Update Goal","Update the existing goal.\nSet status to `complete` only when the objective has actually been achieved and no required work remains. Do not mark a goal complete merely because you are stopping work.\nSet status to `blocked` only after the same blocking condition recurs for at least 3 consecutive goal turns. After resuming, begin a fresh blocked audit after resume. Never mark a goal blocked merely because the work is hard, slow, or uncertain.\nA non-empty reason is required when blocking; reason must not be provided when completing.\nYou cannot use this tool to pause or resume a goal; those status changes are controlled by the user or system.\nWhen marking the goal achieved with status `complete`, report the final elapsed time and token usage from the tool result to the user.",serde_json::json!({"type":"object","required":["status"],"properties":{"status":{"type":"string","enum":["complete","blocked"],"description":"Required. Set to complete when achieved or blocked with a non-empty reason."},"reason":{"type":"string","description":"Required and non-empty when status is blocked; rejected when status is complete."}},"additionalProperties":false})),
        ("get_goal","Get Goal","Get the current goal for this thread, including status, token and elapsed-time usage.",serde_json::json!({"type":"object","properties":{},"additionalProperties":false})),
    ]{
        let mut definition=maho_ext_api::ToolDefinition::new(name,description,parameters,Arc::new(|_|Box::pin(async{Err(maho_ext_api::ToolError::Message("goal tools require live extension context".into()))})));
        definition.label=label.into();
        let deps=Arc::clone(&deps);
        api.register_tool_with_extension_context(definition,Arc::new(move|_,params,_,_,ctx|{
            let deps=Arc::clone(&deps);
            Box::pin(async move{
                let result=match name{
                    "create_goal"=>execute_create_goal(params["objective"].as_str().ok_or_else(||ExtensionFailure::new("objective must be a string"))?,ctx,deps.as_ref()).await?,
                    "update_goal"=>{let status=match params["status"].as_str(){Some("complete")=>ModelSettableGoalStatus::Complete,Some("blocked")=>ModelSettableGoalStatus::Blocked,_=>return Err(ExtensionFailure::new("status must be complete or blocked"))};let reason=params.get("reason").map(|reason|reason.as_str().ok_or_else(||ExtensionFailure::new("reason must be a string"))).transpose()?;execute_update_goal(status,reason,ctx,deps.as_ref()).await?},
                    _=>execute_get_goal(ctx,deps.as_ref()).await?,
                };
                let mut output=maho_ext_api::AgentToolResult::text(result.content.iter().map(|content|match content{ToolContent::Text{text,..}=>text.as_str(),_=>""}).collect::<Vec<_>>().join("\n"));
                output.details=result.details.unwrap_or_else(||serde_json::json!({}));Ok(output)
            })
        }))?;
    }
    Ok(())
}
pub trait GoalToolRegistrationDeps:Send+Sync{
    fn goal_store_ref(&self,ctx:&ExtensionContext)->GoalStoreRef;
    fn begin_agent_goal_accounting(&self,goal:&Goal);
    fn mark_goal_blocked_this_turn(&self,goal:&Goal);
    fn mark_goal_completed_this_turn(&self,goal:&Goal);
    fn account_current_agent_turn<'a>(&'a self,ctx:&'a ExtensionContext,mode:GoalAccountingMode)->ExtensionFuture<'a,Option<Goal>>;
}
pub async fn execute_create_goal(objective:&str,ctx:&ExtensionContext,deps:&dyn GoalToolRegistrationDeps)->Result<maho_ext_api::ToolResult,ExtensionFailure>{
    let reference=deps.goal_store_ref(ctx);let current=read_goal(&reference).map_err(ExtensionFailure::new)?;
    if current.as_ref().is_some_and(|goal|goal.status!=GoalStatus::Complete){return Err(ExtensionFailure::new("cannot create a new goal because this thread already has an unfinished goal; use update_goal only when the existing goal is complete"));}
    let filename=objective_full_text_file_name(&reference);let validated=validate_objective(objective,&filename).map_err(ExtensionFailure::new)?;let goal=create_goal(&reference,objective).map_err(ExtensionFailure::new)?;
    deps.begin_agent_goal_accounting(&goal);update_goal_ui(ctx,Some(&goal));let notice=validated.truncated.then(||objective_truncation_notice(&filename));tool_text(Some(&goal),notice.as_deref())
}
pub async fn execute_update_goal(status:ModelSettableGoalStatus,reason:Option<&str>,ctx:&ExtensionContext,deps:&dyn GoalToolRegistrationDeps)->Result<maho_ext_api::ToolResult,ExtensionFailure>{
    let trimmed=reason.map(|reason|reason.trim_matches(js_whitespace));
    if status==ModelSettableGoalStatus::Blocked&&trimmed.is_none_or(str::is_empty){return Err(ExtensionFailure::new("reason is required when status is blocked"));}
    if status==ModelSettableGoalStatus::Complete&&reason.is_some(){return Err(ExtensionFailure::new("reason must not be provided when status is complete"));}
    deps.account_current_agent_turn(ctx,GoalAccountingMode::Active).await?;
    let goal=update_goal(&deps.goal_store_ref(ctx),&GoalUpdate{status:Some(match status{ModelSettableGoalStatus::Blocked=>GoalStatus::Blocked,ModelSettableGoalStatus::Complete=>GoalStatus::Complete}),reason:trimmed.map(str::to_owned),..Default::default()},GoalUpdateSource::Model).map_err(ExtensionFailure::new)?;
    if goal.status==GoalStatus::Blocked{deps.mark_goal_blocked_this_turn(&goal);}else{deps.mark_goal_completed_this_turn(&goal);}update_goal_ui(ctx,Some(&goal));tool_text(Some(&goal),None)
}
pub async fn execute_get_goal(ctx:&ExtensionContext,deps:&dyn GoalToolRegistrationDeps)->Result<maho_ext_api::ToolResult,ExtensionFailure>{let goal=deps.account_current_agent_turn(ctx,GoalAccountingMode::Active).await?;update_goal_ui(ctx,goal.as_ref());tool_text(goal.as_ref(),None)}
fn tool_text(goal:Option<&Goal>,notice:Option<&str>)->Result<maho_ext_api::ToolResult,ExtensionFailure>{Ok(maho_ext_api::ToolResult{content:vec![ToolContent::text(format_goal_tool_response(goal,notice).map_err(|error|ExtensionFailure::new(error.to_string()))?)],details:Some(serde_json::json!({}))})}
