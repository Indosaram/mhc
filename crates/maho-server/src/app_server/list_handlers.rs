use super::{archive_state::ThreadArchiveState,registry::{JsonRpcError,MethodRegistration,MethodScope},registry_listing::{decode_cursor,encode_cursor},server_core::ServerCore,thread_registry::ThreadRegistry,wire_thread::build_disk_wire_thread};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

pub async fn register_list_handlers(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>,archive: Arc<ThreadArchiveState>,version: String) {
    for method in ["thread/list","thread/loaded/list"] {
        let threads = threads.clone();let archive = archive.clone();let version = version.clone();
        core.write().await.registry.register(method.into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
            let threads = threads.clone();let archive = archive.clone();let version = version.clone();
            Box::pin(async move {
                let params = &context.request["params"];
                let cursor = params["cursor"].as_str();
                let limit = params["limit"].as_f64().filter(|limit|limit.is_finite());
                if method == "thread/loaded/list" {
                    let ids = threads.list_loaded().await.into_iter().map(|thread|thread["id"].clone()).collect::<Vec<_>>();
                    let offset = decode_cursor(cursor);
                    let end = limit.map_or(ids.len(),|limit|{let end = (offset as f64 + limit).trunc();if end < 0.0 {(ids.len() as f64 + end).max(0.0) as usize} else {end as usize}}).min(ids.len());
                    let data = if end >= offset {ids.iter().skip(offset).take(end-offset).cloned().collect::<Vec<_>>()} else {Vec::new()};
                    let next = offset.saturating_add(data.len());
                    return Ok(json!({"data":data,"nextCursor":if next < ids.len() {Some(encode_cursor(next))} else {None}}));
                }
                let page = threads.list_threads(cursor,limit.unwrap_or(25.0).max(0.0) as usize).await;
                let archived = params["archived"] == true;
                let mut listed = page["threads"].as_array().cloned().unwrap_or_default();
                if archived {
                    for thread in archive.list_archived_threads().await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))? {
                        if let Some(index) = listed.iter().position(|listed|listed["id"] == thread["id"]) {listed[index] = thread;} else {listed.push(thread);}
                    }
                }
                let mut data = Vec::new();
                for thread in listed {if archive.is_archived(&thread).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))? == archived {data.push(build_disk_wire_thread(&thread,&version).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?);}}
                Ok(json!({"data":data,"nextCursor":page["nextCursor"],"backwardsCursor":null}))
            })
        })});
    }
}
