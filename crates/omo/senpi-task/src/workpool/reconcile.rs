use crate::state::TaskStatus;
use crate::store::TaskRecordStore;
use super::store::WorkpoolStore;
use super::types::{PoolId,WorkpoolError,WorkpoolItemStatus,WorkpoolStatus,WorkpoolWorkerStatus};
use super::worker_turn::{belongs_to_turn, WorkerTurn};
use super::yield_::apply_yield;
use super::yield_event::{read_yield_events,yield_digest};
pub fn reconcile_workpool(pools:&WorkpoolStore,pool_id:&PoolId)->Result<(),WorkpoolError>{
    let pool=pools.load(pool_id)?;
    if pool.hold.is_some()||pool.status==WorkpoolStatus::Cancelled{return Ok(())}
    pools.mutate_result(pool_id,|latest|{
        let mut next=latest.clone();
        for item in &mut next.items{if item.status==WorkpoolItemStatus::Assigned&&item.binding.is_none(){
            item.status=WorkpoolItemStatus::Error;
            item.error=Some(super::types::WorkpoolYieldError{code:"delivery_uncertain".into(),message:"Delivery is uncertain.".into()});
        }}
        Ok(next)
    })?;
    Ok(())
}

pub fn reconcile_workpool_with_tasks(pools:&WorkpoolStore,tasks:&TaskRecordStore,pool_id:&PoolId,observed:impl Fn(&str,u32)->bool)->Result<(),WorkpoolError>{
    let pool=pools.load(pool_id)?;
    if pool.hold.is_some()||pool.status==WorkpoolStatus::Cancelled{return Ok(())}
    let mut turns=std::collections::BTreeMap::new();
    for item in &pool.items { if let Some(binding)=&item.binding && binding.generation==pool.generation {
        let turn=(pool_id.clone(),binding.generation,binding.task_id.clone(),binding.run_epoch);
        turns.insert((binding.task_id.clone(),binding.run_epoch),turn);
    }}
    for ((task_id,epoch),turn) in turns {
        if observed(&task_id,epoch){continue}
        let task=tasks.load(&task_id).map_err(|error|WorkpoolError::new(super::types::WorkpoolErrorCode::StoreCorrupt,error.to_string()))?;
        if task.as_ref().is_some_and(|task| task.status == TaskStatus::Paused) { continue; }
        let queued=pool.items.iter().any(|item|belongs_to_turn(item,&turn)&&item.status==WorkpoolItemStatus::Assigned&&item.delivery.as_ref().is_some_and(|delivery|delivery.phase==super::types::WorkpoolDeliveryPhase::Queued));
        let recoverable=task.as_ref().is_some_and(|task|task.killed!=Some(true)&&
            ((task.status.is_terminal()&&task.notification.run_epoch==i64::from(epoch))||
                (task.status==TaskStatus::Pending&&task.started_at.is_none()&&epoch==0)));
        if queued&&recoverable {
            super::worker_turn::append_assigned_messages(pools,tasks,&turn)?;
        }else if queued {
            pools.mutate_result(pool_id,|latest|{
                if latest.generation!=turn.1{return Ok(latest.clone())}
                let mut next=latest.clone();
                for item in &mut next.items { if belongs_to_turn(item,&turn)&&item.status==WorkpoolItemStatus::Assigned {
                    *item=uncertain_item_with_context(item.clone(),latest,task.as_ref());
                }}
                Ok(next)
            })?;
        }
        reconcile_worker_turn(pools,tasks,&turn)?;
    }
    let latest=pools.load(pool_id)?;
    let mut terminal_workers=Vec::new();
    for worker in &latest.workers {
        if worker.status!=WorkpoolWorkerStatus::Busy{continue}
        if tasks.load(&worker.task_id).map_err(|error|WorkpoolError::new(super::types::WorkpoolErrorCode::StoreCorrupt,error.to_string()))?
            .is_some_and(|task|task.status.is_terminal()&&task.notification.run_epoch==i64::from(worker.run_epoch)){
            terminal_workers.push((worker.task_id.clone(),worker.run_epoch));
        }
    }
    pools.mutate_result(pool_id,|latest|{
        let mut next=latest.clone();
        for worker in &mut next.workers { if terminal_workers.contains(&(worker.task_id.clone(),worker.run_epoch))&&worker.status==WorkpoolWorkerStatus::Busy {
            worker.status=WorkpoolWorkerStatus::Idle;worker.completed_turns+=1;
            worker.idle_since=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        }}
        Ok(next)
    })?;
    Ok(())
}
pub fn reconcile_worker_turn(pools:&WorkpoolStore,tasks:&TaskRecordStore,turn:&WorkerTurn)->Result<(),WorkpoolError>{
    let events=read_yield_events(tasks,&turn.0,&turn.2,turn.3,turn.1).map_err(|error|WorkpoolError::new(super::types::WorkpoolErrorCode::StoreCorrupt,error.to_string()))?;
    let record=tasks.load(&turn.2).map_err(|error|WorkpoolError::new(super::types::WorkpoolErrorCode::StoreCorrupt,error.to_string()))?;
    if record.as_ref().is_some_and(|task| task.status == TaskStatus::Paused) { return Ok(()); }
    let terminal=record.as_ref().is_some_and(|task|task.notification.run_epoch==i64::from(turn.3)&&task.status.is_terminal());
    pools.mutate_result(&turn.0,|pool|{
        if pool.generation!=turn.1||pool.status==WorkpoolStatus::Cancelled{return Ok(pool.clone())}
        let mut next=pool.clone();
        for item in &mut next.items{
            if !belongs_to_turn(item,turn)||item.yield_sha256.is_some()||item.status==WorkpoolItemStatus::Cancelled{continue}
            if item.status!=WorkpoolItemStatus::Assigned&&item.error.as_ref().is_none_or(|error|error.code!="delivery_uncertain"){continue}
            let authoritative=events.iter().find(|event|event.item_id==item.item_id&&event.result.key()==item.key);
            if let Some(event)=authoritative.filter(|event|yield_digest(&event.result)==event.sha256){
                *item=apply_yield(item.clone(),&event.result);
            }else if item.status==WorkpoolItemStatus::Assigned&&item.delivery.as_ref().is_some_and(|delivery|delivery.phase==super::types::WorkpoolDeliveryPhase::Acknowledged)&&terminal{
                item.status=WorkpoolItemStatus::Error;
                item.error=Some(super::types::WorkpoolYieldError{code:"item_missing_yield".into(),message:"Worker completed without a keyed yield for this item.".into()});
            }else if item.status==WorkpoolItemStatus::Assigned&&item.delivery.as_ref().is_some_and(|delivery|delivery.phase!=super::types::WorkpoolDeliveryPhase::Queued){
                *item=uncertain_item_with_context(item.clone(),pool,record.as_ref());
            }
        }
        Ok(next)
    })?;
    Ok(())
}
pub fn uncertain_item(mut item:super::types::WorkpoolItem)->super::types::WorkpoolItem{
    item.status=WorkpoolItemStatus::Error;
    item.error=Some(super::types::WorkpoolYieldError{code:"delivery_uncertain".into(),message:"Delivery is uncertain. Submit a NEW explicit key.".into()});
    item
}

pub fn uncertain_item_with_context(mut item:super::types::WorkpoolItem,pool:&super::types::WorkpoolRecord,task:Option<&crate::state::TaskRecord>)->super::types::WorkpoolItem{
    let message=item.binding.as_ref().and_then(|binding|{
        if binding.run_epoch==0 {task.and_then(|task|task.spawn_spec.as_ref()).and_then(crate::state::TaskSpawnSpec::as_v1).map(|spec|spec.prompt.clone())}else{None}
    }).unwrap_or_else(||serde_json::json!({"pool_id":pool.pool_id,"generation":item.binding.as_ref().map_or(pool.generation,|binding|binding.generation),"items":[{"item_id":item.item_id,"key":item.key,"input":item.input}]}).to_string());
    use sha2::Digest;
    let digest=sha2::Sha256::digest(message.as_bytes()).iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    let phase=item.delivery.as_ref().map_or(super::types::WorkpoolDeliveryPhase::Dispatching,|delivery|delivery.phase);
    let message_sha256=item.delivery.as_ref().and_then(|delivery|delivery.message_sha256.clone()).unwrap_or(digest);
    item.status=WorkpoolItemStatus::Error;
    item.error=Some(super::types::WorkpoolYieldError{code:"delivery_uncertain".into(),message:"Delivery is uncertain. Inspect worker output and submit a NEW explicit key; this key is never retried automatically.".into()});
    item.delivery=Some(super::types::WorkpoolDelivery{phase,entry_ids:item.delivery.as_ref().and_then(|d|d.entry_ids.clone()),message_sha256:Some(message_sha256)});
    item
}
#[cfg(test)]mod tests{use super::*;use crate::workpool::types::{WorkpoolAgent,WorkpoolItem,WorkpoolMode,WorkpoolRecord,WorkpoolSpec};#[test]fn uncertain_item_changes_status_and_records_delivery_identity(){let item=WorkpoolItem{key:"k".into(),input:serde_json::Value::Null,item_id:"i".into(),status:WorkpoolItemStatus::Assigned,binding:None,delivery:None,data:None,error:None,yield_sha256:None};let pool=WorkpoolRecord { hold: None, version:1,pool_id:"wp_00000000000000000000000000000000".into(),name:"p".into(),parent_session_id:"parent".into(),root_session_id:"root".into(),generation:1,revision:0,mode:WorkpoolMode::Fresh,agent:WorkpoolAgent::Category{prompt:"p".into(),model:None,category:"quick".into()},worker_spec:WorkpoolSpec{start:Default::default(),plan:Default::default()},status:WorkpoolStatus::Open,kernel_tool_names:None,items:vec![],workers:vec![],aggregate:None};let result=uncertain_item_with_context(item,&pool,None);assert_eq!(result.status,WorkpoolItemStatus::Error);assert_eq!(result.error.expect("uncertain error").code,"delivery_uncertain");assert_eq!(result.delivery.expect("delivery receipt").message_sha256.unwrap().len(),64);}}
