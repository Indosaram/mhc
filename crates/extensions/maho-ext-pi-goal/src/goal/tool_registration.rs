use maho_ext_api::{ExtensionContext,ExtensionFailure,ExtensionFuture,ToolContent};
use super::{format::format_goal_tool_response,store::{create_goal,objective_full_text_file_name,read_goal,update_goal},types::*,ui::update_goal_ui,validation::{js_whitespace,objective_truncation_notice,validate_objective}};
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
