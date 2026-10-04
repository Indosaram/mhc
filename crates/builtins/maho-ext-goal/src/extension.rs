use std::sync::Arc;
use maho_ext_api::{Extension,ExtensionApi,EventKind,EventResult};
use crate::{accounting_hooks::GoalStoreReference,runtime::GoalRuntime};
pub struct GoalExtension { pub reference:GoalStoreReference,pub now:Arc<dyn Fn()->f64+Send+Sync> }
impl Default for GoalExtension {
    fn default()->Self { Self::new(Arc::new(crate::store_ref::context_goal_store_ref)) }
}
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
        let runtime=Arc::new(GoalRuntime::new(self.reference.clone(),self.now.clone()));
        let continuation=Arc::new(crate::delivery::GoalDelivery::new(runtime.clone(),self.reference.clone(),self.now.clone(),api));
        let queue=continuation.queue_callback();
        runtime.register_command(api,queue);
        crate::cache_warm_renderer::register_cache_warm_renderer(api);
        for (name,label,description,schema) in [
            ("create_goal","Create Goal","Register a goal for work that outlives this turn: it waits on external state, or the user's requested outcome needs more than one verify-and-fix round before it is true. A single answer, lookup, or one-shot edit needs no goal.\nObjectives are limited to 4,000 characters. For longer instructions, put the full objective in a file and refer to that file.\nReplaces the current goal when it is complete and archives it; fails if an unfinished goal exists.",crate::tool_registration::create_goal_schema()),
            ("get_goal","Get Goal","Get the current goal for this thread, including status, token and elapsed-time usage.",crate::tool_registration::get_goal_schema()),
            ("update_goal","Update Goal","Update the existing goal. Set complete only when the objective has actually been achieved and no required work remains. A non-empty reason is required when blocking; reason must not be provided when completing.",crate::tool_registration::update_goal_schema()),
        ] {
            let mut definition=maho_ext_api::ToolDefinition::new(name,description,schema,Arc::new(|_|Box::pin(async { Err(maho_ext_api::ToolError::Message("Goal tool requires an extension context".into())) })));
            definition.label=label.into();
            let runtime=runtime.clone();
            let delivery=continuation.clone();
            let execute:maho_ext_api::ExtensionToolExecutor=Arc::new(move |_,params,_,_,context| {
                let runtime=runtime.clone();
                let continuation=delivery.clone();
                Box::pin(async move {
                    let result=match name {
                        "create_goal"=>runtime.create(context,params["objective"].as_str().ok_or_else(||maho_ext_api::ExtensionFailure::new("objective must be a string"))?).await,
                        "get_goal"=>runtime.get(context).await,
                        "update_goal"=>runtime.update(context,&params,&crate::todo_gate::open_todo_task_contents(&context.session_manager.get_branch())).await,
                        _=>unreachable!(),
                    }.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    continuation.reconcile(context).await?;
                    Ok(maho_ext_api::AgentToolResult {
                        content:result.content.into_iter().map(|content|match content { maho_ext_api::ToolContent::Text { text,.. }=>maho_ext_api::ContentBlock::text(text),maho_ext_api::ToolContent::Image { data,mime_type }=>maho_ext_api::ContentBlock::Image(maho_ai::types::ImageContent { data,mime_type }) }).collect(),
                        details:result.details.unwrap_or(serde_json::Value::Null),usage:None,added_tool_names:None,terminate:None,is_error:None,
                    })
                })
            });
            if let Err(error)=api.register_tool_with_extension_context(definition,execute) { std::panic::panic_any(error); }
            api.registered.tool_renderers.insert(name.into(),Arc::new(crate::renderers::native_goal_tool_renderers(name)));
        }
        for kind in [EventKind::SessionStart,EventKind::AgentStart,EventKind::MessageStart,EventKind::MessageEnd,EventKind::AgentEnd,EventKind::AgentSettled,EventKind::Input,EventKind::InputDisposition,EventKind::SessionAbort,EventKind::SessionShutdown] {
            let runtime=runtime.clone();
            let events=api.events.clone();
            let continuation=continuation.clone();
            api.on(kind,Arc::new(move |event,context| { let runtime=runtime.clone(); let events=events.clone(); let continuation=continuation.clone(); Box::pin(async move { let goal=runtime.event(event,context).await?; if matches!(event,maho_ext_api::ExtensionEvent::SessionStart(_)) { runtime.start_channels(&events,context).await?; } continuation.event(event,context,goal.as_ref()).await?; Ok(EventResult::None) }) }));
        }
        let stale=Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reset=stale.clone();
        api.on(EventKind::TurnStart,Arc::new(move |_,_| { reset.store(false,std::sync::atomic::Ordering::Release); Box::pin(async { Ok(EventResult::None) }) }));
        let reference=self.reference.clone();
        api.on(EventKind::ToolResult,Arc::new(move |event,ctx| {
            let result=match event {
                maho_ext_api::ExtensionEvent::ToolResult(event) if event.tool_name=="todo"&&!event.is_error&&!stale.load(std::sync::atomic::Ordering::Acquire)&&crate::todo_gate::todo_result_adds_open_tasks(event.details.as_ref())=>{
                    match crate::store::read_goal(&reference(ctx)) {
                        Ok(goal)=>crate::todo_gate::stale_goal_todo_reminder(goal.as_ref()).map(|reminder| { stale.store(true,std::sync::atomic::Ordering::Release); let mut content=event.content.clone(); content.push(maho_ext_api::ToolContent::text(reminder)); EventResult::ToolResult(maho_ext_api::ToolResultEventResult { content:Some(content),..Default::default() }) }).ok_or(()),
                        Err(error)=>return Box::pin(async move { Err(maho_ext_api::ExtensionFailure::new(error.to_string())) }),
                    }
                },_=>Err(()),
            };
            Box::pin(async move { Ok(result.unwrap_or(EventResult::None)) })
        }));
        runtime
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn factory_installs_all_tools_command_renderers_and_settlement_hook() {
        let mut api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default());
        GoalExtension::default().register(&mut api);
        for name in ["create_goal","update_goal","get_goal"] {
            assert!(api.registered.tools.iter().any(|tool|tool.definition.name==name));
            assert!(api.runtime.extension_tool_executor("goal",name).is_some());
            assert!(api.registered.tool_renderers.contains_key(name));
        }
        assert!(api.registered.commands.iter().any(|command|command.name=="goal"));
        assert!(api.registered.handlers.contains_key(&EventKind::AgentSettled));
        assert!(api.registered.entry_renderers.contains_key(crate::cache_warm::GOAL_CACHE_WARMUP_ENTRY_TYPE));
    }
    #[tokio::test] async fn registered_update_completes_and_block_requires_reason() {
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let extension=GoalExtension { reference:Arc::new(move |_|stored.clone()),now:Arc::new(||0.0) };
        let mut api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default()); extension.register(&mut api);
        crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let context=crate::test_context::context();
        let update=api.runtime.extension_tool_executor("goal","update_goal").unwrap();
        assert!(update("bad",serde_json::json!({"status":"blocked"}),None,None,&context).await.is_err());
        let blocked=update("block",serde_json::json!({"status":"blocked","reason":"waiting"}),None,None,&context).await.unwrap();
        assert_eq!(blocked.details["goal"]["status"],"blocked");
        let complete=update("complete",serde_json::json!({"status":"complete"}),None,None,&context).await.unwrap();
        assert_eq!(complete.details["goal"]["status"],"complete");
        api.registered.handlers[&EventKind::SessionShutdown][0](&mut maho_ext_api::ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent { reason:maho_ext_api::SessionReason::Quit,target_session_file:None,signal:None }),&context).await.unwrap();
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
