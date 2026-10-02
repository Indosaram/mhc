use super::types::{Goal,GoalStatus,GoalUpdateSource};
pub fn transition_goal_status(current:&Goal,status:GoalStatus,source:GoalUpdateSource,reason:Option<&str>,updated_at:u64)->Result<Goal,String>{
    let allowed=current.status==status || match source{
        GoalUpdateSource::Model=>matches!((current.status,status),(GoalStatus::Active,GoalStatus::Blocked|GoalStatus::Complete)|(GoalStatus::Blocked,GoalStatus::Complete)),
        GoalUpdateSource::User=>matches!((current.status,status),(GoalStatus::Active,GoalStatus::Paused)|(GoalStatus::Paused|GoalStatus::Blocked,GoalStatus::Active)),
    };
    if !allowed{return Err(format!("illegal goal transition: {} -> {}",current.status.as_str(),status.as_str()));}
    if status==GoalStatus::Complete && reason.is_some(){return Err("reason must not be provided when status is complete".into());}
    let mut next=current.clone();next.status=status;next.updated_at=updated_at;
    if status==GoalStatus::Blocked{
        if current.status!=GoalStatus::Blocked{
            let reason=reason.map(str::trim).filter(|reason|!reason.is_empty()).ok_or_else(||"reason is required when status is blocked".to_owned())?;
            next.blocked_reason=Some(reason.into());next.blocked_at=Some(updated_at);
        }
    }else{next.blocked_reason=None;next.blocked_at=None;}
    if status==GoalStatus::Active && current.status!=GoalStatus::Active{next.last_started_at=Some(updated_at);}else if status!=GoalStatus::Active{next.last_started_at=None;}
    next.completed_at=if status==GoalStatus::Complete{Some(current.completed_at.unwrap_or(updated_at))}else{None};
    Ok(next)
}
