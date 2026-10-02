use super::{archive_state::ThreadArchiveState,metadata_state::{MetadataStateError,ThreadMetadataState},registry::{JsonRpcError,MethodRegistration,MethodScope},server_core::ServerCore,thread_registry::ThreadRegistry,turn_log::TurnLog,wire_thread::{build_disk_wire_thread,build_wire_thread}};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{Mutex,RwLock};

pub async fn register_metadata_handlers(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>,log: Arc<Mutex<TurnLog>>,archive: Arc<ThreadArchiveState>,version: String) {
    let state = Arc::new(ThreadMetadataState::default());
    core.write().await.registry.register("thread/metadata/update".into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
        let threads = threads.clone();let log = log.clone();let archive = archive.clone();let state = state.clone();let version = version.clone();
        Box::pin(async move {
            let params = &context.request["params"];
            let id = params["threadId"].as_str().filter(|id|!id.is_empty()).ok_or_else(||JsonRpcError::new(-32603,"Invalid params: threadId is required"))?;
            let archived = archive.list_archived_threads().await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?.into_iter().find(|thread|thread["id"] == id);
            let wire = if let Some(mut archived) = archived {
                let path = archived["sessionPath"].as_str().ok_or_else(||JsonRpcError::new(-32603,format!("Thread {id} has no session path for metadata state")))?;
                state.update_git_info(id,std::path::Path::new(path),&params["gitInfo"]).await.map_err(metadata_error)?;
                archived["status"] = json!({"type":"notLoaded"});
                build_disk_wire_thread(&archived,&version).await.map_err(metadata_error)?
            } else {
                let entry = threads.resume_thread(id).await.map_err(|error|if error == format!("Thread not found: {id}") {JsonRpcError::new(-32600,format!("thread not found: {id}"))} else {JsonRpcError::new(-32603,error)})?;
                let entry = entry.lock().await;
                let path = entry.session.session_file().ok_or_else(||JsonRpcError::new(-32603,format!("Thread {id} has no session path for metadata state")))?;
                state.update_git_info(id,std::path::Path::new(&path),&params["gitInfo"]).await.map_err(metadata_error)?;
                let mut log = log.lock().await;
                build_wire_thread(&entry,&mut log,false,&version).await.map_err(metadata_error)?
            };
            Ok(json!({"thread":wire}))
        })
    })});
}
fn metadata_error(error: MetadataStateError) -> JsonRpcError {
    JsonRpcError::new(if matches!(error,MetadataStateError::Update(_)) {-32600} else {-32603},error.to_string())
}
