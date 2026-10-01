use std::sync::Arc;
use crate::{types::*,command::{ParsedGoalCommand,parse_goal_command},format::{format_goal_for_tool,goal_status_label}};
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionFailure,ExtensionFuture,NotificationType};
pub type AccountGoal=Arc<dyn for<'a> Fn(&'a ExtensionContext,GoalAccountingMode)->ExtensionFuture<'a,Option<Goal>>+Send+Sync>;
pub type QueueGoalContinuation=Arc<dyn Fn(&ExtensionContext,&Goal)->Result<(),ExtensionFailure>+Send+Sync>;
pub type RefreshGoalUi=Arc<dyn Fn(&ExtensionContext,Option<&Goal>)+Send+Sync>;
pub struct GoalCommandRegistrationDeps {
    pub goal_store_ref:Arc<dyn Fn(&ExtensionContext)->GoalStoreRef+Send+Sync>,
    pub now:Arc<dyn Fn()->u64+Send+Sync>,
    pub account_current_agent_turn:AccountGoal,
    pub begin_agent_goal_accounting:Arc<dyn Fn(&Goal)+Send+Sync>,
    pub stop_agent_goal_accounting:Arc<dyn Fn(&str)+Send+Sync>,
    pub clear_agent_goal_accounting:Arc<dyn Fn()+Send+Sync>,
    pub queue_goal_continuation:QueueGoalContinuation,
    pub refresh_goal_ui:RefreshGoalUi,
}
pub fn register_goal_command(api:&mut ExtensionApi,deps:Arc<GoalCommandRegistrationDeps>) {
    api.register_command("goal",Some("Set, inspect, pause, resume, or clear the persistent goal".into()),None,Arc::new(move |args,ctx| {
        let deps=Arc::clone(&deps);
        Box::pin(async move {
            if let Err(error)=run_goal_command(args,ctx,&deps).await { ctx.ui.notify(&error.message,NotificationType::Error); }
            Ok(())
        })
    }));
}
async fn run_goal_command(args:&str,ctx:&ExtensionContext,deps:&GoalCommandRegistrationDeps)->Result<(),ExtensionFailure> {
    let reference=(deps.goal_store_ref)(ctx);
    match parse_goal_command(args) {
        ParsedGoalCommand::Show=>{
            let goal=crate::store::read_goal(&reference).map_err(failure)?;
            (deps.refresh_goal_ui)(ctx,goal.as_ref());
            let text=if goal.is_none() { "Usage: /goal <objective>\nNo goal is currently set.".into() } else { format_goal_for_tool(goal.as_ref()).map_err(failure)? };
            ctx.ui.notify(&text,if goal.is_some() { NotificationType::Info } else { NotificationType::Warning });
        },
        ParsedGoalCommand::Clear=>{
            (deps.account_current_agent_turn)(ctx,GoalAccountingMode::Active).await?;
            let cleared=crate::store::clear_goal(&reference).await.map_err(failure)?;
            (deps.clear_agent_goal_accounting)(); (deps.refresh_goal_ui)(ctx,None);
            ctx.ui.notify(if cleared { "Goal cleared" } else { "No goal to clear\nThis thread does not currently have a goal." },if cleared { NotificationType::Info } else { NotificationType::Warning });
        },
        ParsedGoalCommand::SetStatus(status)=>{
            if status==GoalStatus::Paused { (deps.account_current_agent_turn)(ctx,GoalAccountingMode::Active).await?; }
            let goal=crate::store::update_goal(&reference,&GoalUpdate { status:Some(status),..Default::default() },GoalUpdateSource::User,(deps.now)()).await.map_err(failure)?;
            if goal.status==GoalStatus::Active { (deps.begin_agent_goal_accounting)(&goal); } else { (deps.stop_agent_goal_accounting)(&goal.id); }
            show_and_queue(ctx,&goal,deps)?;
        },
        ParsedGoalCommand::SetObjective(objective)=>{
            let current=crate::store::read_goal(&reference).map_err(failure)?;
            if current.is_some() && ctx.has_ui {
                let choices=["Replace current goal".into(),"Cancel".into()];
                let title=format!("Replace goal?\nNew objective: {objective}");
                if ctx.ui.select(&title,&choices,Default::default()).await.as_deref()!=Some("Replace current goal") { return Ok(()); }
            }
            if current.as_ref().is_some_and(|goal|goal.status==GoalStatus::Active) { (deps.account_current_agent_turn)(ctx,GoalAccountingMode::Active).await?; }
            let goal=if current.is_none() { crate::store::create_goal(&reference,&objective,None,(deps.now)()).await } else { crate::store::update_goal(&reference,&GoalUpdate { objective:Some(objective),..Default::default() },GoalUpdateSource::User,(deps.now)()).await }.map_err(failure)?;
            if goal.status==GoalStatus::Active { (deps.begin_agent_goal_accounting)(&goal); }
            show_and_queue(ctx,&goal,deps)?;
        },
    }
    Ok(())
}
fn failure(error:crate::errors::GoalError)->ExtensionFailure { ExtensionFailure::new(error.to_string()) }
fn show_and_queue(ctx:&ExtensionContext,goal:&Goal,deps:&GoalCommandRegistrationDeps)->Result<(),ExtensionFailure> {
    (deps.refresh_goal_ui)(ctx,Some(goal));
    ctx.ui.notify(&format!("Goal {}\n{}",goal_status_label(goal.status),format_goal_for_tool(Some(goal)).map_err(failure)?),NotificationType::Info);
    (deps.queue_goal_continuation)(ctx,goal)
}
