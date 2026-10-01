use std::sync::{Arc,Mutex};
use maho_ext_api::{EventKind,EventResult,Extension,ExtensionApi,ExtensionEvent,ExtensionFailure};
use serde_json::Value;
use crate::{service::{ToolSearchRuntime,ToolSearchService},tool::create_tool_search_tool};

pub struct ToolSearchExtension { pub service:Arc<Mutex<ToolSearchService>> }
impl ToolSearchExtension {
    pub fn new(runtime:Arc<dyn ToolSearchRuntime>)->Self { Self { service:Arc::new(Mutex::new(ToolSearchService::new(runtime))) } }
}
impl Extension for ToolSearchExtension {
    fn register(&self,api:&mut ExtensionApi) {
        api.register_tool(create_tool_search_tool(Arc::clone(&self.service)));
        for kind in [EventKind::SessionStart,EventKind::Context] {
            let service=Arc::clone(&self.service);
            api.on(kind,Arc::new(move |event,ctx| {
                let service=Arc::clone(&service);
                Box::pin(async move {
                    let mut service=service.lock().map_err(|_|ExtensionFailure::new("ToolSearchService lock poisoned"))?;
                    match event {
                        ExtensionEvent::SessionStart(_)=> {
                            service.begin_session();
                            let messages:Vec<Value>=ctx.session_manager.get_entries().into_iter().map(|e|e.data).collect();
                            service.maybe_rehydrate_from_history(&messages);
                        },
                        ExtensionEvent::Context { messages }=> {
                            let messages=messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>,_>>().map_err(|e|ExtensionFailure::new(e.to_string()))?;
                            service.maybe_rehydrate_from_history(&messages);
                        },
                        _=>{},
                    }
                    Ok(EventResult::None)
                })
            }));
        }
    }
}
