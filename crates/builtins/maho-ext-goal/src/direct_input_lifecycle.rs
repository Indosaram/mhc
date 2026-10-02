use std::collections::BTreeMap;
use crate::{errors::GoalError,types::{Goal,GoalStatus,GoalStoreRef,GoalUpdate,GoalUpdateSource}};
use maho_ext_api::{InputEvent,InputSource,InputDisposition};
#[derive(Default)]
pub struct GoalDirectInputLifecycle { candidates:BTreeMap<String,Option<String>>,suppressed_load_resume_armed:bool }
pub enum DirectInputGoalChange { Reactivated(Goal),Active { goal:Goal,resume_suppressed_load:bool } }
impl GoalDirectInputLifecycle {
    pub fn reset(&mut self) { self.candidates.clear(); self.suppressed_load_resume_armed=false; }
    pub fn arm_suppressed_load_resume(&mut self) { self.suppressed_load_resume_armed=true; }
    pub fn on_input(&mut self,event:&InputEvent,reference:&GoalStoreRef,hold:impl FnOnce(&str))->Result<(),GoalError> {
        if event.source==InputSource::Extension { return Ok(()); }
        hold(&event.input_id); self.candidates.insert(event.input_id.clone(),None);
        let goal=crate::store::read_goal(reference)?;
        self.candidates.insert(event.input_id.clone(),goal.map(|goal|goal.id));
        Ok(())
    }
    pub async fn on_disposition(&mut self,input_id:&str,disposition:InputDisposition,reference:&GoalStoreRef,now:u64,resolve:impl FnOnce(&str,bool))->Result<Option<DirectInputGoalChange>,GoalError> {
        let Some(candidate)=self.candidates.remove(input_id) else { return Ok(None); };
        let accepted=matches!(disposition,InputDisposition::Started|InputDisposition::Queued);
        resolve(input_id,accepted);
        if !accepted { return Ok(None); }
        let Some(candidate)=candidate else { return Ok(None); };
        let Some(current)=crate::store::read_goal(reference)? else { return Ok(None); };
        if current.id!=candidate { return Ok(None); }
        if current.status==GoalStatus::Blocked && crate::continuation_recovery::is_mechanical_continuation_block(current.blocked_reason.as_deref()) {
            crate::store::reset_continuation_streak(reference,true).await?;
            let reactivated=crate::store::update_goal(reference,&GoalUpdate { status:Some(GoalStatus::Active),..Default::default() },GoalUpdateSource::User,now).await?;
            return Ok(Some(DirectInputGoalChange::Reactivated(reactivated)));
        }
        if current.status!=GoalStatus::Active { return Ok(None); }
        let goal=crate::store::reset_continuation_streak(reference,true).await?.unwrap_or(current);
        let resume_suppressed_load=self.suppressed_load_resume_armed; self.suppressed_load_resume_armed=false;
        Ok(Some(DirectInputGoalChange::Active { goal,resume_suppressed_load }))
    }
}
#[cfg(test)] mod tests {
    use super::*;
    fn input()->InputEvent { InputEvent { input_id:"input".into(),text:"continue".into(),source:InputSource::Interactive,images:None,streaming_behavior:None } }
    #[tokio::test] async fn accepted_steering_reactivates_mechanical_block() {
        let temp=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:temp.path().into(),thread_id:"s".into() };
        crate::store::create_goal(&reference,"work",None,1).await.unwrap();
        crate::store::update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Blocked),reason:Some("continuation cap reached".into()),..Default::default() },GoalUpdateSource::Model,2).await.unwrap();
        let mut lifecycle=GoalDirectInputLifecycle::default(); lifecycle.on_input(&input(),&reference,|_|{}).unwrap();
        let change=lifecycle.on_disposition("input",InputDisposition::Queued,&reference,3,|_,accepted|assert!(accepted)).await.unwrap();
        assert!(matches!(change,Some(DirectInputGoalChange::Reactivated(goal)) if goal.status==GoalStatus::Active));
        assert_eq!(crate::store::read_goal(&reference).unwrap().unwrap().status,GoalStatus::Active);
    }
    #[tokio::test] async fn accepted_candidate_does_not_mutate_replacement_goal() {
        let temp=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:temp.path().into(),thread_id:"s".into() };
        let mut goal=crate::store::create_goal(&reference,"original",None,1).await.unwrap();
        let mut lifecycle=GoalDirectInputLifecycle::default(); lifecycle.on_input(&input(),&reference,|_|{}).unwrap();
        goal.id="replacement".into(); goal.consecutive_continuations=Some(5); crate::store::write_goal(&reference,Some(&goal)).await.unwrap();
        let change=lifecycle.on_disposition("input",InputDisposition::Started,&reference,2,|_,_|{}).await.unwrap();
        assert!(change.is_none()); assert_eq!(crate::store::read_goal(&reference).unwrap(),Some(goal));
    }
    #[tokio::test] async fn rejected_input_keeps_mechanical_block() {
        let temp=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:temp.path().into(),thread_id:"s".into() };
        crate::store::create_goal(&reference,"work",None,1).await.unwrap();
        let blocked=crate::store::update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Blocked),reason:Some("continuation cap reached".into()),..Default::default() },GoalUpdateSource::Model,2).await.unwrap();
        let mut lifecycle=GoalDirectInputLifecycle::default(); lifecycle.on_input(&input(),&reference,|_|{}).unwrap();
        lifecycle.on_disposition("input",InputDisposition::Rejected,&reference,3,|_,accepted|assert!(!accepted)).await.unwrap();
        assert_eq!(crate::store::read_goal(&reference).unwrap(),Some(blocked));
    }
    #[tokio::test] async fn suppressed_load_resume_waits_for_admission_and_fires_once() {
        let temp=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:temp.path().into(),thread_id:"s".into() };
        crate::store::create_goal(&reference,"work",None,1).await.unwrap();
        let mut lifecycle=GoalDirectInputLifecycle::default(); lifecycle.arm_suppressed_load_resume();
        lifecycle.on_input(&input(),&reference,|_|{}).unwrap();
        assert!(lifecycle.on_disposition("input",InputDisposition::Rejected,&reference,2,|_,accepted|assert!(!accepted)).await.unwrap().is_none());
        lifecycle.on_input(&input(),&reference,|_|{}).unwrap();
        assert!(matches!(lifecycle.on_disposition("input",InputDisposition::Started,&reference,3,|_,accepted|assert!(accepted)).await.unwrap(),Some(DirectInputGoalChange::Active { resume_suppressed_load:true,.. })));
        lifecycle.on_input(&input(),&reference,|_|{}).unwrap();
        assert!(matches!(lifecycle.on_disposition("input",InputDisposition::Queued,&reference,4,|_,_|{}).await.unwrap(),Some(DirectInputGoalChange::Active { resume_suppressed_load:false,.. })));
    }
    #[tokio::test] async fn extension_input_never_holds_or_consumes_suppressed_resume() {
        let temp=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:temp.path().into(),thread_id:"s".into() };
        crate::store::create_goal(&reference,"work",None,1).await.unwrap(); let mut lifecycle=GoalDirectInputLifecycle::default(); lifecycle.arm_suppressed_load_resume();
        let mut extension=input(); extension.source=InputSource::Extension;
        lifecycle.on_input(&extension,&reference,|_|panic!("extension input must not hold")).unwrap();
        assert!(lifecycle.on_disposition("input",InputDisposition::Started,&reference,2,|_,_|panic!("untracked input must not resolve")).await.unwrap().is_none());
        lifecycle.on_input(&input(),&reference,|_|{}).unwrap();
        assert!(matches!(lifecycle.on_disposition("input",InputDisposition::Started,&reference,3,|_,_|{}).await.unwrap(),Some(DirectInputGoalChange::Active { resume_suppressed_load:true,.. })));
    }
    #[tokio::test] async fn reset_retires_pending_input_and_suppressed_resume() {
        let temp=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:temp.path().into(),thread_id:"s".into() }; crate::store::create_goal(&reference,"work",None,1).await.unwrap();
        let mut lifecycle=GoalDirectInputLifecycle::default(); lifecycle.arm_suppressed_load_resume(); lifecycle.on_input(&input(),&reference,|_|{}).unwrap(); lifecycle.reset();
        assert!(lifecycle.on_disposition("input",InputDisposition::Started,&reference,2,|_,_|panic!("retired input must not resolve")).await.unwrap().is_none());
        lifecycle.on_input(&input(),&reference,|_|{}).unwrap();
        assert!(matches!(lifecycle.on_disposition("input",InputDisposition::Started,&reference,3,|_,_|{}).await.unwrap(),Some(DirectInputGoalChange::Active { resume_suppressed_load:false,.. })));
    }
}
