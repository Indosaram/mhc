use std::sync::Arc;
use maho_ext_api::{Extension,ExtensionApi,EventKind,EventResult};
use crate::{accounting_hooks::GoalStoreReference,runtime::GoalRuntime};
pub struct GoalExtension { pub reference:GoalStoreReference,pub now:Arc<dyn Fn()->f64+Send+Sync> }
impl GoalExtension {
    pub fn new(reference:GoalStoreReference)->Self { Self { reference,now:Arc::new(||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0,|duration|duration.as_millis() as f64)) } }
}
impl Extension for GoalExtension {
    fn register(&self,api:&mut ExtensionApi) {
        self.register_with_runtime(api);
    }
}
impl GoalExtension {
    pub fn register_with_runtime(&self,api:&mut ExtensionApi)->Arc<GoalRuntime> {
        api.register_entry_renderer(crate::cache_warm::GOAL_CACHE_WARMUP_ENTRY_TYPE,crate::cache_warm_renderer::render_goal_cache_warmup_entry(),maho_ext_api::EntryRendererOptions { replaces:Some(crate::cache_warm_renderer::cache_warm_entry_replaces()) });
        let mut runtime=GoalRuntime::new(self.reference.clone(),self.now.clone()); runtime.events=api.events.clone();
        let runtime=Arc::new(runtime);
        for (name,label,description,schema) in [
            ("create_goal","Create Goal","Register a goal for work that outlives this turn: it waits on external state, or the user's requested outcome needs more than one verify-and-fix round before it is true. A single answer, lookup, or one-shot edit needs no goal.\nObjectives are limited to 4,000 characters. For longer instructions, put the full objective in a file and refer to that file.\nReplaces the current goal when it is complete and archives it; fails if an unfinished goal exists.",crate::tool_registration::create_goal_schema()),
            ("get_goal","Get Goal","Get the current goal for this thread, including status, token and elapsed-time usage.",crate::tool_registration::get_goal_schema()),
            ("update_goal","Update Goal","Set the current goal to complete or blocked. Completion requires no open todo tasks; blocking requires a reason.",crate::tool_registration::update_goal_schema()),
        ] {
            let mut definition=maho_ext_api::ToolDefinition::new(name,description,schema,Arc::new(|_|Box::pin(async { Err(maho_ext_api::ToolError::Message("Goal tool requires an extension context".into())) })));
            definition.label=label.into();
            let runtime=runtime.clone();
            let execute:maho_ext_api::ExtensionToolExecutor=Arc::new(move |_,params,_,_,context| {
                let runtime=runtime.clone();
                Box::pin(async move {
                    let result=match name {
                        "create_goal"=>runtime.create(context,params["objective"].as_str().ok_or_else(||maho_ext_api::ExtensionFailure::new("objective must be a string"))?).await,
                        "get_goal"=>runtime.get(context).await,
                        "update_goal"=>{
                            let entries=context.session_manager.get_branch().into_iter().map(|entry|{
                                let mut value=entry.data; value["type"]=entry.kind.into(); value
                            }).collect::<Vec<_>>();
                            let open=maho_ext_todotools::todo_storage::get_latest_todos_from_branch_entries(&entries).into_iter().filter(|task|matches!(task.status,maho_ext_todotools::todo_types::TodoStatus::Pending|maho_ext_todotools::todo_types::TodoStatus::InProgress)).map(|task|task.content).collect::<Vec<_>>();
                            runtime.update(context,&params,&open).await
                        },
                        _=>unreachable!(),
                    }.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    Ok(maho_ext_api::AgentToolResult {
                        content:result.content.into_iter().map(|content|match content { maho_ext_api::ToolContent::Text { text,.. }=>maho_ext_api::ContentBlock::text(text),maho_ext_api::ToolContent::Image { data,mime_type }=>maho_ext_api::ContentBlock::Image(maho_ai::types::ImageContent { data,mime_type }) }).collect(),
                        details:result.details.unwrap_or(serde_json::Value::Null),usage:None,added_tool_names:None,terminate:None,is_error:None,
                    })
                })
            });
            if let Err(error)=api.register_tool_with_extension_context(definition,execute) { std::panic::panic_any(error); }
        }
        for kind in [EventKind::SessionStart,EventKind::AgentStart,EventKind::MessageStart,EventKind::MessageEnd,EventKind::AgentEnd,EventKind::Input,EventKind::InputDisposition,EventKind::SessionAbort,EventKind::SessionShutdown] {
            let runtime=runtime.clone();
            let events=api.events.clone();
            api.on(kind,Arc::new(move |event,context| { let runtime=runtime.clone(); let events=events.clone(); Box::pin(async move { runtime.event(event,context).await?; if matches!(event,maho_ext_api::ExtensionEvent::SessionStart(_)) { runtime.start_channels(&events,context).await?; } Ok(EventResult::None) }) }));
        }
        runtime
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn native_update_reads_canonical_todos_and_rejects_false_completion() {
        use maho_ext_api::*;
        struct Branch(std::sync::Mutex<Vec<SessionEntry>>);
        impl ToolSessionManager for Branch { fn session_id(&self)->&str { "s" } fn session_file(&self)->Option<&std::path::Path> { None } }
        impl SessionManager for Branch {
            fn get_entries(&self)->Vec<SessionEntry> { self.get_branch() }
            fn get_branch(&self)->Vec<SessionEntry> { self.0.lock().unwrap().clone() }
            fn get_leaf_id(&self)->Option<String> { None }
            fn get_session_name(&self)->Option<String> { None }
        }
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let extension=GoalExtension { reference:Arc::new(move |_|stored.clone()),now:Arc::new(||0.0) };
        let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default()); extension.register(&mut api);
        let branch=Arc::new(Branch(std::sync::Mutex::new(vec![SessionEntry { id:"todo".into(),parent_id:None,timestamp:String::new(),kind:"custom".into(),data:serde_json::json!({"type":"custom","customType":maho_ext_todotools::todo_types::TODO_STATE_ENTRY_TYPE,"data":{"schema":"v2","phases":[{"name":"Work","tasks":[{"content":"unfinished","status":"pending"}]}]}}) }])));
        let mut context=crate::test_context::context(); context.session_manager=branch.clone();
        let create=api.runtime.extension_tool_executor("goal","create_goal").unwrap();
        let update=api.runtime.extension_tool_executor("goal","update_goal").unwrap();
        create("create",serde_json::json!({"objective":"work"}),None,None,&context).await.unwrap();
        assert!(update("complete",serde_json::json!({"status":"complete"}),None,None,&context).await.is_err());
        assert_eq!(crate::store::read_goal(&reference).unwrap().unwrap().status,crate::types::GoalStatus::Active);
        branch.0.lock().unwrap()[0].data["data"]["phases"][0]["tasks"][0]["status"]="completed".into();
        let result=update("complete",serde_json::json!({"status":"complete"}),None,None,&context).await.unwrap();
        assert_eq!(result.details["goal"]["status"],"complete");
        assert_eq!(crate::store::read_goal(&reference).unwrap().unwrap().status,crate::types::GoalStatus::Complete);
    }
    #[tokio::test] async fn factory_start_binds_channel_settings_and_shutdown_releases_subscription() {
        use maho_ext_api::*;
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let extension=GoalExtension { reference:Arc::new(move |_|stored.clone()),now:Arc::new(||0.0) };
        let events=EventBus::default(); let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),events.clone(),Default::default()); extension.register(&mut api);
        let mut context=crate::test_context::context(); let session=crate::test_context::bind_session(&mut context);
        let mut start=ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None });
        api.registered.handlers[&EventKind::SessionStart][0](&mut start,&context).await.unwrap();
        events.emit("wake_source_state",&serde_json::json!({"source":"task","activeCount":1}));
        let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None });
        api.registered.handlers[&EventKind::SessionShutdown][0](&mut shutdown,&context).await.unwrap();
        events.emit("wake_source_state",&serde_json::json!({"source":"task","activeCount":0})); session.dispose().await;
    }
    #[tokio::test] async fn factory_context_tools_create_and_read_the_same_store() {
        use maho_ext_api::*;
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let extension=GoalExtension { reference:Arc::new(move |_|stored.clone()),now:Arc::new(||0.0) };
        let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default()); extension.register(&mut api);
        let context=crate::test_context::context();
        let create=api.runtime.extension_tool_executor("goal","create_goal").unwrap();
        let get=api.runtime.extension_tool_executor("goal","get_goal").unwrap();
        assert!(get("get",serde_json::json!({}),None,None,&context).await.unwrap().details["goal"].is_null());
        let result=create("create",serde_json::json!({"objective":"work"}),None,None,&context).await.unwrap();
        assert_eq!(result.details["goal"]["objective"],"work");
        assert!(create("replace",serde_json::json!({"objective":"other"}),None,None,&context).await.is_err());
        let read=get("get",serde_json::json!({}),None,None,&context).await.unwrap();
        let persisted=crate::store::read_goal(&reference).unwrap().unwrap();
        assert_eq!(read.details,serde_json::to_value(crate::format::goal_tool_response(Some(&persisted))).unwrap());
        assert_eq!(read.details["goal"]["objective"],result.details["goal"]["objective"]);
        let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None });
        api.registered.handlers[&EventKind::SessionShutdown][0](&mut shutdown,&context).await.unwrap();
    }
    #[tokio::test] async fn registered_factory_hooks_share_accounting_and_persist_user_abort() {
        use maho_ext_api::*;
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let clock=Arc::new(std::sync::atomic::AtomicU64::new(0)); let reading=clock.clone();
        let extension=GoalExtension { reference:Arc::new(move |_|stored.clone()),now:Arc::new(move ||reading.load(std::sync::atomic::Ordering::SeqCst) as f64) };
        let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default()); extension.register(&mut api);
        let context=crate::test_context::context();
        api.registered.handlers[&EventKind::AgentStart][0](&mut ExtensionEvent::AgentStart,&context).await.unwrap();
        clock.store(1500,std::sync::atomic::Ordering::SeqCst);
        api.registered.handlers[&EventKind::SessionAbort][0](&mut ExtensionEvent::SessionAbort,&context).await.unwrap();
        let blocked=crate::store::read_goal(&reference).unwrap().unwrap(); assert_eq!(blocked.status,crate::types::GoalStatus::Blocked); assert_eq!(blocked.time_used_seconds,2.0);
        let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None });
        api.registered.handlers[&EventKind::SessionShutdown][0](&mut shutdown,&context).await.unwrap();
    }
}
