use std::sync::{Arc,Mutex,PoisonError};
use maho_ext_api::{ExtensionApi,EventKind,EventResult};
pub const TASK_USAGE_HINT_FLAG:&str="omo-task-usage-hint";
pub fn wire_task_usage_guidance(api:&mut ExtensionApi,enabled:Arc<dyn Fn()->bool+Send+Sync>) {
    let guard=Arc::new(Mutex::new(crate::usage_guidance::OncePerSessionGuard::default()));
    api.on(EventKind::BeforeAgentStart,Arc::new(move |_,context| {
        let first=enabled() && guard.lock().unwrap_or_else(PoisonError::into_inner).first_delivery(context.session_manager.session_id());
        Box::pin(async move { Ok(if first { EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult { message:Some(maho_ext_api::CustomMessage { custom_type:"senpi-task.usage".into(),content:vec![maho_ext_api::ToolContent::text(crate::usage_guidance::TASK_USAGE_GUIDANCE)],display:false,details:Some(serde_json::json!({})) }),system_prompt:None }) } else { EventResult::None }) })
    }));
}
