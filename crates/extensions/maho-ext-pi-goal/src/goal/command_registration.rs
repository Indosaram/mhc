use std::sync::Arc;
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionFailure,ExtensionFuture,NotificationType};
use super::{command::{parse_goal_command,ParsedGoalCommand},format::{format_goal_for_tool,goal_status_label},store::{clear_goal,create_goal,read_goal,update_goal},types::*,ui::update_goal_ui};

pub trait GoalCommandRegistrationDeps:Send+Sync {
    fn goal_store_ref(&self,ctx:&ExtensionContext)->GoalStoreRef;
    fn begin_agent_goal_accounting(&self,goal:&Goal);
    fn stop_agent_goal_accounting(&self,id:&str);
    fn clear_agent_goal_accounting(&self);
    fn account_current_agent_turn<'a>(&'a self,ctx:&'a ExtensionContext,mode:GoalAccountingMode)->ExtensionFuture<'a,Option<Goal>>;
    fn queue_goal_continuation(&self,ctx:&ExtensionContext,goal:&Goal)->Result<(),ExtensionFailure>;
}
pub fn register_goal_command(api:&mut ExtensionApi,deps:Arc<dyn GoalCommandRegistrationDeps>){
    api.register_command("goal",Some("Set, inspect, pause, resume, or clear the persistent goal".into()),None,Arc::new(move|args,ctx|{
        let deps=Arc::clone(&deps);
        Box::pin(async move {
            if let Err(error)=handle_goal_command(args,ctx,deps.as_ref()).await{ctx.ui.notify(&error.message,NotificationType::Error);}
            Ok(())
        })
    }));
}
async fn handle_goal_command(args:&str,ctx:&ExtensionContext,deps:&dyn GoalCommandRegistrationDeps)->Result<(),ExtensionFailure>{
    let reference=deps.goal_store_ref(ctx);
    match parse_goal_command(args){
        ParsedGoalCommand::Show=>{
            let goal=read_goal(&reference).map_err(ExtensionFailure::new)?;
            update_goal_ui(ctx,goal.as_ref());
            let text=match &goal{None=>"Usage: /goal <objective>\nNo goal is currently set.".into(),Some(goal)=>format_goal_for_tool(Some(goal)).map_err(ExtensionFailure::new)?};
            ctx.ui.notify(&text,if goal.is_some(){NotificationType::Info}else{NotificationType::Warning});
        }
        ParsedGoalCommand::SetObjective{objective}=>{
            let current=read_goal(&reference).map_err(ExtensionFailure::new)?;
            if current.is_some()&&ctx.has_ui{
                let choice=ctx.ui.select(&format!("Replace goal?\nNew objective: {objective}"),&["Replace current goal".into(),"Cancel".into()],Default::default()).await;
                if choice.as_deref()!=Some("Replace current goal"){return Ok(());}
            }
            if current.as_ref().is_some_and(|goal|goal.status==GoalStatus::Active){deps.account_current_agent_turn(ctx,GoalAccountingMode::Active).await?;}
            let goal=if current.is_none(){create_goal(&reference,&objective)}else{update_goal(&reference,&GoalUpdate{objective:Some(objective),..Default::default()},GoalUpdateSource::User)}.map_err(ExtensionFailure::new)?;
            if goal.status==GoalStatus::Active{deps.begin_agent_goal_accounting(&goal);}
            notify_goal(ctx,&goal)?;deps.queue_goal_continuation(ctx,&goal)?;
        }
        command @ (ParsedGoalCommand::Pause|ParsedGoalCommand::Resume)=>{
            let status=if matches!(command,ParsedGoalCommand::Pause){GoalStatus::Paused}else{GoalStatus::Active};
            if status==GoalStatus::Paused{deps.account_current_agent_turn(ctx,GoalAccountingMode::Active).await?;}
            let goal=update_goal(&reference,&GoalUpdate{status:Some(status),..Default::default()},GoalUpdateSource::User).map_err(ExtensionFailure::new)?;
            if goal.status==GoalStatus::Active{deps.begin_agent_goal_accounting(&goal);}else{deps.stop_agent_goal_accounting(&goal.id);}
            notify_goal(ctx,&goal)?;deps.queue_goal_continuation(ctx,&goal)?;
        }
        ParsedGoalCommand::Clear=>{
            deps.account_current_agent_turn(ctx,GoalAccountingMode::Active).await?;
            let cleared=clear_goal(&reference).map_err(ExtensionFailure::new)?;
            deps.clear_agent_goal_accounting();update_goal_ui(ctx,None);
            ctx.ui.notify(if cleared{"Goal cleared"}else{"No goal to clear\nThis thread does not currently have a goal."},if cleared{NotificationType::Info}else{NotificationType::Warning});
        }
    }
    Ok(())
}
fn notify_goal(ctx:&ExtensionContext,goal:&Goal)->Result<(),ExtensionFailure>{
    update_goal_ui(ctx,Some(goal));
    ctx.ui.notify(&format!("Goal {}\n{}",goal_status_label(goal.status),format_goal_for_tool(Some(goal)).map_err(ExtensionFailure::new)?),NotificationType::Info);
    Ok(())
}
