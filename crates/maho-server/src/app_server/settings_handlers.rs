use super::{registry::{JsonRpcError,MethodRegistration,MethodScope},server_core::ServerCore,thread_registry::ThreadRegistry};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

pub async fn register_thread_settings(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>) {
    let weak = Arc::downgrade(core);
    core.write().await.registry.register("thread/settings/update".into(),MethodRegistration {requires_init:true,experimental:true,scope:MethodScope::Thread,handler:Arc::new(move |context| {
        let threads = threads.clone();let weak = weak.clone();
        Box::pin(async move {
            let params = context.request["params"].as_object().ok_or_else(||JsonRpcError::new(-32603,"Invalid params: threadId is required"))?;
            let id = params.get("threadId").and_then(serde_json::Value::as_str).filter(|id|!id.is_empty()).ok_or_else(||JsonRpcError::new(-32603,"Invalid params: threadId is required"))?;
            let mut unsupported = params.keys().filter(|key|!matches!(key.as_str(),"threadId"|"model"|"effort")).cloned().collect::<Vec<_>>();unsupported.sort();
            if !unsupported.is_empty() {return Err(JsonRpcError::new(-32600,format!("unsupported thread settings: {}",unsupported.join(", "))));}
            let entry = threads.resume_thread(id).await.map_err(|error|if error == format!("Thread not found: {id}") {JsonRpcError::new(-32600,format!("thread not found: {id}"))} else {JsonRpcError::new(-32603,error)})?;
            let entry = entry.lock().await;
            let session = &entry.session;
            let current = session.model();
            let model = match params.get("model").filter(|value|!value.is_null()) {
                None => None,
                Some(value) => {
                    let reference = value.as_str().filter(|value|!value.is_empty()).ok_or_else(||JsonRpcError::new(-32603,"Invalid params: model must be a non-empty string"))?;
                    let model = if let Some((provider,id)) = reference.split_once('/').filter(|(provider,_)|!provider.is_empty()) {session.model_registry().find(provider,id)} else {session.model_registry().get_all().into_iter().find(|model|model.id == reference)};
                    Some(model.ok_or_else(||JsonRpcError::new(-32600,format!("model not found: {reference}")))?)
                }
            };
            let effort = match params.get("effort").filter(|value|!value.is_null()) {
                None => None,
                Some(value) => {
                    let value = value.as_str().ok_or_else(||JsonRpcError::new(-32603,"Invalid params: effort must be a string"))?;
                    let levels = model.as_ref().map(maho_core::thinking_levels::get_supported_thinking_levels).unwrap_or_else(||session.get_available_thinking_levels());
                    Some(levels.into_iter().find(|level|serde_json::to_value(level).is_ok_and(|level|level == value)).ok_or_else(||JsonRpcError::new(-32603,format!("Invalid params: unsupported effort {value}")))?)
                }
            };
            let before_effort = session.thinking_level();
            if let Some(model) = model.filter(|model|model.provider != current.provider || model.id != current.id) {session.set_session_model(model).await.map_err(|error|JsonRpcError::new(-32603,error))?;}
            if let Some(effort) = effort.filter(|effort|*effort != session.thinking_level()) {session.set_session_thinking_level(effort);}
            let after = session.model();
            if after.provider != current.provider || after.id != current.id || before_effort != session.thinking_level() {
                let tier = session.service_tier().map(|tier|match tier {maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority"});
                let notification = json!({"method":"thread/settings/updated","params":{"threadId":id,"threadSettings":{"cwd":entry.cwd,"approvalPolicy":"never","approvalsReviewer":"user","sandboxPolicy":{"type":"dangerFullAccess"},"activePermissionProfile":null,"model":after.id,"modelProvider":after.provider,"serviceTier":tier,"effort":session.thinking_level(),"summary":null,"collaborationMode":{"mode":"default","settings":{"model":"unknown","reasoning_effort":"off","developer_instructions":null}},"personality":null}}});
                let recipients = entry.subscribers.iter().cloned().collect::<Vec<_>>();
                context.connection.defer_until_responded(move || {tokio::spawn(async move {
                    if let Some(core) = weak.upgrade() {let core = core.read().await;for recipient in recipients {if let Err(error) = core.send_notification_to_connection(&recipient,notification.clone(),chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server settings notification: {}",error.message);}}}
                });});
            }
            Ok(json!({}))
        })
    })});
}
