use crate::{index::{LoopCreateOk,LoopCreateOutcome,StartFixedRequest,StartDynamicRequest,StartBareRequest},scheduler::{LoopScheduler,CreateDynamicRequest,CreateFixedRequest,CreateResult},types::*};
fn outcome(scheduler:&LoopScheduler,created:CreateResult)->LoopCreateOutcome {
    match created {
        CreateResult::Cap { active_loop_ids }=>LoopCreateOutcome::Rejected { message:format!("At most 5 loops can be active at once. Active: {}. Stop one with `/loop stop <id>` or `/loop stop all` first.",active_loop_ids.join(", ")) },
        CreateResult::Created { loop_id,superseded_loop_id }=>{
            let expires_at=scheduler.state.entries.get(&loop_id).map(|entry|match entry { CronEntry::Fixed { fields,.. }|CronEntry::Dynamic { fields,.. }=>fields.expires_at });
            LoopCreateOutcome::Created(LoopCreateOk { loop_id,superseded_loop_id,rounding_notice:None,cron_expression:None,effective_cadence:None,expires_at })
        },
    }
}
pub fn create_fixed(scheduler:&mut LoopScheduler,request:StartFixedRequest,id:LoopId,now:f64)->LoopCreateOutcome {
    let normalized=crate::cron_planner::normalize_interval(&request.requested_interval);
    let cron=crate::cron_planner::describe_cron(normalized.effective.value,normalized.effective.unit);
    let notice=normalized.effective.rounding_notice.clone(); let human=normalized.effective.human.clone();
    let created=scheduler.create_fixed(CreateFixedRequest { base:CreateDynamicRequest { reentry_prompt:format!("/loop {}",request.original_args).trim_end().into(),original_args:request.original_args,payload:LoopPayload::Prompt { prompt:request.prompt } },requested_interval:request.requested_interval,effective_interval:normalized.effective,cron_expression:cron.clone(),interval_ms:normalized.interval_ms },id,now);
    let mut result=outcome(scheduler,created); if let LoopCreateOutcome::Created(created)=&mut result { created.rounding_notice=notice; created.cron_expression=Some(cron); created.effective_cadence=Some(human); } result
}
pub fn create_dynamic(scheduler:&mut LoopScheduler,request:StartDynamicRequest,id:LoopId,now:f64)->LoopCreateOutcome {
    let created=scheduler.create_dynamic(CreateDynamicRequest { reentry_prompt:format!("/loop {}",request.original_args).trim_end().into(),original_args:request.original_args,payload:LoopPayload::Prompt { prompt:request.prompt } },id,now); outcome(scheduler,created)
}
pub fn create_bare(scheduler:&mut LoopScheduler,request:StartBareRequest,id:LoopId,now:f64,file_present:bool)->LoopCreateOutcome {
    let sentinel=match (request.interval.is_some(),file_present) { (true,true)=>LoopSentinel::LoopFile,(false,true)=>LoopSentinel::LoopFileDynamic,(true,false)=>LoopSentinel::Autonomous,(false,false)=>LoopSentinel::AutonomousDynamic };
    let trimmed=request.original_args.trim(); let reentry_prompt=if trimmed.is_empty() { "/loop".into() } else { format!("/loop {trimmed}") };
    let base=CreateDynamicRequest { original_args:request.original_args,reentry_prompt,payload:LoopPayload::Sentinel { sentinel } };
    let created=if let Some(interval)=request.interval { let normalized=crate::cron_planner::normalize_interval(&interval); let cron=crate::cron_planner::describe_cron(normalized.effective.value,normalized.effective.unit); scheduler.create_fixed(CreateFixedRequest { base,requested_interval:interval,effective_interval:normalized.effective,cron_expression:cron,interval_ms:normalized.interval_ms },id,now) } else { scheduler.create_dynamic(base,id,now) };
    outcome(scheduler,created)
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn fixed_creation_exposes_normalized_cadence_and_arms_scheduler() {
        let mut scheduler=LoopScheduler::new("s",None,&Default::default());
        let LoopCreateOutcome::Created(created)=create_fixed(&mut scheduler,StartFixedRequest { original_args:"45s check".into(),prompt:"check".into(),requested_interval:RequestedInterval { value:45.0,unit:RequestedIntervalUnit::Seconds,raw:"45s".into() } },"a".into(),0.0) else { panic!("creation rejected") };
        assert_eq!(created.effective_cadence.as_deref(),Some("1 minute")); assert!(created.rounding_notice.is_some()); assert_eq!(scheduler.armed_timers["a"],60_000.0);
    }
    #[test] fn bare_dynamic_selects_file_sentinel_and_preserves_reentry() {
        let mut scheduler=LoopScheduler::new("s",None,&Default::default());
        assert!(matches!(create_bare(&mut scheduler,StartBareRequest { original_args:" ".into(),interval:None },"a".into(),0.0,true),LoopCreateOutcome::Created(_)));
        let CronEntry::Dynamic { fields,.. }=&scheduler.state.entries["a"] else { panic!("expected dynamic") };
        assert_eq!(fields.reentry_prompt,"/loop"); assert_eq!(fields.payload,LoopPayload::Sentinel { sentinel:LoopSentinel::LoopFileDynamic });
    }
    #[test] fn dynamic_creation_reports_superseded_identity() {
        let mut scheduler=LoopScheduler::new("s",None,&Default::default());
        create_dynamic(&mut scheduler,StartDynamicRequest { original_args:"first".into(),prompt:"first".into() },"a".into(),0.0);
        let LoopCreateOutcome::Created(created)=create_dynamic(&mut scheduler,StartDynamicRequest { original_args:"second".into(),prompt:"second".into() },"b".into(),1.0) else { panic!("creation rejected") };
        assert_eq!(created.superseded_loop_id.as_deref(),Some("a")); assert_eq!(scheduler.state.active_dynamic_id.as_deref(),Some("b"));
    }
}
