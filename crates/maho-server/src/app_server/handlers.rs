use super::{archive_state::ThreadArchiveState,handler_params::required_string,registry::{JsonRpcError,MethodRegistration,MethodScope},server_core::ServerCore,thread_registry::ThreadRegistry,wire_thread::build_disk_wire_thread};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

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
