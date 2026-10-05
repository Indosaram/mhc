use crate::{continuation::GOAL_CONTINUATION_CAP,types::{Goal,GoalStatus}};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ReloadReengagementOutcome { SkippedInactive,BackstopRearmed,SuppressedFlood,ContinuationQueued }
pub async fn reengage_goal_after_reload<E>(
    goal:&Goal,
    has_active_wake_sources:impl FnOnce()->bool,
    rearm_backstop:impl FnOnce(&Goal)->Result<(),E>,
    count_trailing_continuations:impl FnOnce()->u64,
    notify_suppressed:impl FnOnce(u64)->Result<(),E>,
    queue_continuation:impl AsyncFnOnce()->Result<(),E>,
)->Result<ReloadReengagementOutcome,E> {
    if goal.status!=GoalStatus::Active { return Ok(ReloadReengagementOutcome::SkippedInactive); }
    if has_active_wake_sources() { rearm_backstop(goal)?; return Ok(ReloadReengagementOutcome::BackstopRearmed); }
    let count=count_trailing_continuations();
    if count>=GOAL_CONTINUATION_CAP { notify_suppressed(count)?; return Ok(ReloadReengagementOutcome::SuppressedFlood); }
    queue_continuation().await?;
    Ok(ReloadReengagementOutcome::ContinuationQueued)
}
#[cfg(test)] mod tests {
    use super::*;
    fn goal(status:GoalStatus)->Goal { serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":status,"tokensUsed":0,"timeUsedSeconds":0,"createdAt":0,"updatedAt":0})).unwrap() }
    #[tokio::test] async fn inactive_goal_never_reads_channels_or_queues() {
        let result=reengage_goal_after_reload::<()>(&goal(GoalStatus::Blocked),||panic!("inactive channel read"),|_|panic!("inactive rearm"),||panic!("inactive history read"),|_|panic!("inactive notice"),async ||panic!("inactive queue")).await.unwrap();
        assert_eq!(result,ReloadReengagementOutcome::SkippedInactive);
    }
    #[tokio::test] async fn live_channels_rearm_before_history_flood_check() {
        let mut rearmed=false;
        let result=reengage_goal_after_reload::<()>(&goal(GoalStatus::Active),||true,|_|{ rearmed=true; Ok(()) },||panic!("live channel history read"),|_|panic!("live channel notice"),async ||panic!("live channel queue")).await.unwrap();
        assert_eq!((result,rearmed),(ReloadReengagementOutcome::BackstopRearmed,true));
    }
    #[tokio::test] async fn flooded_history_does_not_queue() {
        let mut notified=None;
        let result=reengage_goal_after_reload::<()>(&goal(GoalStatus::Active),||false,|_|panic!("no channels"),||8,|count|{ notified=Some(count); Ok(()) },async ||panic!("flooded queue")).await.unwrap();
        assert_eq!((result,notified),(ReloadReengagementOutcome::SuppressedFlood,Some(8)));
    }
    #[tokio::test] async fn active_goal_without_channels_queues() {
        let mut queued=false;
        let result=reengage_goal_after_reload::<()>(&goal(GoalStatus::Active),||false,|_|panic!("no channels"),||7,|_|panic!("not flooded"),async ||{ queued=true; Ok(()) }).await.unwrap();
        assert_eq!((result,queued),(ReloadReengagementOutcome::ContinuationQueued,true));
    }
}
