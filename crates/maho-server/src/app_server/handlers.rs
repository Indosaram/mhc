use super::{archive_state::ThreadArchiveState,handler_params::required_string,registry::{JsonRpcError,MethodRegistration,MethodScope},server_core::ServerCore,thread_registry::ThreadRegistry,wire_thread::build_disk_wire_thread};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ThreadLifecycleController {
    timers: std::sync::Mutex<std::collections::BTreeMap<String,tokio::task::JoinHandle<()>>>,
    core: std::sync::Weak<RwLock<ServerCore>>,
    threads: Arc<ThreadRegistry>,
    idle_unload: std::time::Duration,
}
impl ThreadLifecycleController {
    pub fn new(core:std::sync::Weak<RwLock<ServerCore>>,threads:Arc<ThreadRegistry>,idle_unload:std::time::Duration)->Arc<Self> {
        Arc::new(Self {timers:Default::default(),core,threads,idle_unload})
    }
    pub fn clear_idle_timer(&self,id:&str) {
        if let Some(timer)=self.timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id) {timer.abort();}
    }
    pub async fn schedule_idle_unload_for_thread(self:&Arc<Self>,id:&str) {
        let Ok(entry)=self.threads.get_loaded_thread(id).await else {return;};
        let entry=entry.lock().await;
        self.clear_idle_timer(id);
        if !entry.subscribers.is_empty()||entry.active_turn.is_some() {return;}
        let deadline=tokio::time::Instant::now()+self.idle_unload;
        let controller=Arc::downgrade(self);let id=id.to_owned();
        let mut timers=self.timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let task_id=id.clone();
        let task=tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let Some(controller)=controller.upgrade() else {return;};
            controller.timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&task_id);
            if !controller.threads.unload_if_idle(&task_id).await {return;}
            if let Some(core)=controller.core.upgrade() {
                let core=core.read().await;
                for notification in [json!({"method":"thread/closed","params":{"threadId":task_id}}),json!({"method":"thread/status/changed","params":{"threadId":task_id,"status":{"type":"notLoaded"}}})] {
                    if let Err(error)=core.broadcast_notification(notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server idle notification: {}",error.message);}
                }
            }
        });
        timers.insert(id,task);
    }
    pub fn dispose(&self) {
        for timer in std::mem::take(&mut *self.timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner)).into_values() {timer.abort();}
    }
}
impl Drop for ThreadLifecycleController {
    fn drop(&mut self) {self.dispose();}
}

pub async fn register_storage_lifecycle_handlers(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>,archive: Arc<ThreadArchiveState>,version: String) {
    let weak = Arc::downgrade(core);
    for method in ["thread/archive","thread/unarchive","thread/delete"] {
        let threads = threads.clone();let archive = archive.clone();let version = version.clone();let weak = weak.clone();
        core.write().await.registry.register(method.into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
            let threads = threads.clone();let archive = archive.clone();let version = version.clone();let weak = weak.clone();
            Box::pin(async move {
                let id = required_string(&context.request["params"]["threadId"],"threadId")?.to_owned();
                let error = |error: super::archive_state::ArchiveStateError|JsonRpcError::new(-32603,error.to_string());
                let response = match method {
                    "thread/archive"=>{
                        let entry = threads.resume_thread(&id).await.map_err(|error|JsonRpcError::new(-32603,error))?;
                        let mut wire = entry.lock().await.wire();wire["status"] = json!({"type":"notLoaded"});
                        archive.mark_archived(&wire,&chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true)).await.map_err(error)?;
                        threads.unload_thread(&id).await;
                        if let Some(core) = weak.upgrade() {core.read().await.broadcast_notification(json!({"method":"thread/status/changed","params":{"threadId":id,"status":{"type":"notLoaded"}}}),chrono::Utc::now().timestamp_millis() as u64).await?;}
                        json!({})
                    },
                    "thread/delete"=>{
                        archive.clear_archived(&id).await.map_err(error)?;
                        threads.delete_thread(&id).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
                        if let Some(core) = weak.upgrade() {core.read().await.broadcast_notification(json!({"method":"thread/status/changed","params":{"threadId":id,"status":{"type":"notLoaded"}}}),chrono::Utc::now().timestamp_millis() as u64).await?;}
                        json!({})
                    },
                    _=>{
                        let mut wire = archive.unarchive(&id,chrono::Utc::now().timestamp_millis()).await.map_err(error)?.ok_or_else(||JsonRpcError::new(-32600,format!("thread not found: {id}")))?;
                        wire["status"] = json!({"type":"notLoaded"});
                        json!({"thread":build_disk_wire_thread(&wire,&version).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?})
                    },
                };
                let notification = match method {"thread/archive"=>"thread/archived","thread/delete"=>"thread/deleted",_=>"thread/unarchived"};
                context.connection.defer_until_responded(move || {tokio::spawn(async move {if let Some(core) = weak.upgrade() && let Err(error) = core.read().await.broadcast_notification(json!({"method":notification,"params":{"threadId":id}}),chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server lifecycle notification: {}",error.message);}});});
                Ok(response)
            })
        })});
    }
}

pub async fn register_compaction_handler(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>,log: Arc<tokio::sync::Mutex<super::turn_log::TurnLog>>) {
    let weak = Arc::downgrade(core);
    core.write().await.registry.register("thread/compact/start".into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
        let threads = threads.clone();let log = log.clone();let weak = weak.clone();
        Box::pin(async move {
            let id = required_string(&context.request["params"]["threadId"],"threadId")?.to_owned();
            let entry = threads.get_loaded_thread(&id).await.map_err(|_|JsonRpcError::new(-32600,format!("thread not found: {id}")))?;
            context.connection.defer_until_responded(move || {tokio::spawn(async move {
                use super::turn_log::{RecordTurnOptions,CompleteTurnOptions,CompleteTurnStatus};
                let Some(core) = weak.upgrade() else {return;};
                let turn_id = uuid::Uuid::new_v4().to_string();
                let item = json!({"type":"contextCompaction","id":uuid::Uuid::new_v4().to_string()});
                let started = chrono::Utc::now();
                log.lock().await.record_turn(&id,RecordTurnOptions {turn_id:turn_id.clone(),started_at:started.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),status:None,completed_at:None,error:None});
                super::turns::emit(&core,&entry,json!({"method":"item/started","params":{"threadId":id,"turnId":turn_id,"item":item,"startedAtMs":started.timestamp_millis()}})).await;
                let session = entry.lock().await.session.clone();
                let result = session.compact(None).await;
                let completed = chrono::Utc::now();
                let successful = result.is_ok();
                let mut log = log.lock().await;
                if successful && let Some(item) = item.as_object() && let Err(error) = log.append_item(&id,&turn_id,item.clone()) {eprintln!("app-server compaction log: {error}");}
                if let Err(error) = log.complete_turn(&id,&turn_id,CompleteTurnOptions {status:if successful {CompleteTurnStatus::Completed} else {CompleteTurnStatus::Failed},completed_at:completed.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),error:result.err()}) {eprintln!("app-server compaction log: {error}");}
                drop(log);
                if successful {super::turns::emit(&core,&entry,json!({"method":"item/completed","params":{"threadId":id,"turnId":turn_id,"item":item,"completedAtMs":completed.timestamp_millis()}})).await;}
            });});
            Ok(json!({}))
        })
    })});
}
