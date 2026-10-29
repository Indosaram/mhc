use std::{collections::BTreeSet, sync::{Arc,Mutex}};
use maho_ext_api::*;
use maho_omo_ultrawork::{SessionArming,shared_session_arming};
use crate::reminder::TODO_FANOUT_REMINDER;

pub struct TodoFanoutReminderComponent { pub arming:Arc<Mutex<SessionArming>> }
impl Default for TodoFanoutReminderComponent { fn default()->Self { Self{arming:shared_session_arming()} } }
impl Extension for TodoFanoutReminderComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let reminded=Arc::new(Mutex::new(BTreeSet::<Option<String>>::new()));
        for kind in [EventKind::SessionCompact,EventKind::SessionShutdown] {
            let reminded=Arc::clone(&reminded);
            api.on(kind,Arc::new(move |event,ctx| { let reminded=Arc::clone(&reminded); Box::pin(async move {
                if matches!(event,ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected{..})) { return Ok(EventResult::None); }
                let id=ctx.session_manager.session_id(); let id=(!id.is_empty()).then(||id.to_owned());
                reminded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);
                Ok(EventResult::None)
            }) }));
        }
        let arming=Arc::clone(&self.arming); let runtime=api.runtime.clone();
        api.on(EventKind::ToolResult,Arc::new(move |event,ctx| {
            let arming=Arc::clone(&arming); let reminded=Arc::clone(&reminded); let runtime=runtime.clone();
            Box::pin(async move {
                let ExtensionEvent::ToolResult(event)=event else { return Ok(EventResult::None); };
                if runtime.get_flag("omo-senpi-todo-fanout-reminder-disabled")==Some(FlagValue::Boolean(true)) || event.tool_name!="todo" || event.is_error || !matches!(event.input["op"].as_str(),Some("init"|"append")) { return Ok(EventResult::None); }
                let id=ctx.session_manager.session_id(); let id=(!id.is_empty()).then(||id.to_owned());
                if !arming.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_armed(id.as_deref()) { return Ok(EventResult::None); }
                if !reminded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id) { return Ok(EventResult::None); }
                let mut content=event.content.clone(); content.push(ToolContent::text(TODO_FANOUT_REMINDER));
                Ok(EventResult::ToolResult(ToolResultEventResult{content:Some(content),..Default::default()}))
            })
        }));
    }
}
