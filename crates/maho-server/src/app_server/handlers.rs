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
