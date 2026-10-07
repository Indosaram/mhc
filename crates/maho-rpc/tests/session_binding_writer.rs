#[tokio::test]
async fn binding_commits_attribution_and_routes_records_through_stdio_actor(){
    use tokio::io::AsyncReadExt;
    use maho_rpc::{session_binding::BindingRecords,session_attribution::SessionActivityRegistry,session_event_fanout::SessionEventFanout,session_event_writer::SessionWriterActor};
    let activity=SessionActivityRegistry::default();let mut binding=BindingRecords::new("s".into(),activity.clone());
    let mut fanout=SessionEventFanout::default();let(output,mut reader)=tokio::io::duplex(1);let writer=SessionWriterActor::new(output);
    binding.deliver_records("s","{\"type\":\"tool_execution_start\",\"toolCallId\":\"call\",\"toolName\":\"bash\"}\n",&mut fanout,&writer).unwrap();
    assert_eq!(activity.since(activity.mark()).unwrap().tool.as_deref(),Some("bash"));
    let expected=maho_rpc::jsonl::serialize_json_line(&serde_json::json!({"type":"tool_execution_start","toolCallId":"call","toolName":"bash","sessionId":"s"})).unwrap();
    let read=async{let mut bytes=vec![0;expected.len()];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(bytes,expected.as_bytes());};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(writer.flush(),read)}).await.unwrap();result.unwrap();
    binding.dispose();assert!(activity.since(activity.mark()).is_none());
}

#[test]
fn typed_session_events_preserve_optional_fields_and_flattened_budget(){
    use maho_rpc::session_binding::session_event_record;
    use maho_ext_api::{AgentSessionEvent as E,CompactionReason,CompactionRejectionCause,ToolHookLifecycleEvent,ToolHookName,ToolHookPhase,ToolHookStatus};
    use serde_json::json;
    let event=E::CompactionEnd{reason:CompactionReason::Manual,result:None,aborted:false,will_retry:false,request_id:Some("request".into()),accepted:Some(false),rejection_cause:Some(CompactionRejectionCause::ExternalOwner),error_message:None};
    assert_eq!(session_event_record(&event).unwrap(),json!({"type":"compaction_end","reason":"manual","aborted":false,"willRetry":false,"requestId":"request","accepted":false,"rejectionCause":"external-owner"}));
    assert_eq!(session_event_record(&E::ServiceTierChanged{tier:None,fast_mode:false}).unwrap(),json!({"type":"service_tier_changed","fastMode":false}));
    let event=E::ToolHookStatus(ToolHookLifecycleEvent{hook_run_id:"run".into(),hook_name:ToolHookName::PreToolUse,tool_name:"bash".into(),tool_call_id:"call".into(),extension_path:"native.rs".into(),status_message:"done".into(),started_at:1,phase:ToolHookPhase::End{completed_at:2,status:ToolHookStatus::Completed,error_message:None}});
    assert_eq!(session_event_record(&event).unwrap(),json!({"type":"tool_hook_status","hookRunId":"run","hookName":"PreToolUse","toolName":"bash","toolCallId":"call","extensionPath":"native.rs","statusMessage":"done","startedAt":1,"phase":"end","completedAt":2,"status":"completed"}));
    assert_eq!(session_event_record(&E::SettingsSourceSelected{selection:json!({"scope":"project"})}).unwrap(),json!({"type":"settings_source_selected","scope":"project"}));
}

#[tokio::test]
async fn rpc_attach_installs_its_ui_without_repeating_session_start() {
    use maho_core::{
        agent_session::ExtensionBindings,
        model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
        sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session},
        session_manager::SessionManager,
        settings_manager::{InMemorySettingsStorage, SettingsManager},
    };
    use maho_ext_api::*;
    use std::sync::{Arc, Mutex};

    struct NoopUi;
    impl ExtensionUi for NoopUi {
        fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
        fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn notify(&self, _: &str, _: NotificationType) {}
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<ComponentFactory>) {}
        fn set_footer(&self, _: Option<ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String { String::new() }
        fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err(ExtensionFailure::new("no custom UI in this fixture")) }) }
        fn theme(&self) -> Theme { Theme::default() }
    }

    #[derive(Default)]
    struct Recorder {
        starts: Arc<Mutex<Vec<(ExtensionMode, bool)>>>,
        shutdowns: Arc<Mutex<Vec<(ExtensionMode, bool)>>>,
    }
    impl Extension for Recorder {
        fn register(&self, api: &mut ExtensionApi) {
            let starts = self.starts.clone();
            api.on(EventKind::SessionStart, Arc::new(move |_, context| {
                starts.lock().expect("startup dispatches").push((context.mode, context.has_ui));
                Box::pin(async { Ok(EventResult::None) })
            }));
            let shutdowns = self.shutdowns.clone();
            api.on(EventKind::SessionShutdown, Arc::new(move |_, context| {
                shutdowns.lock().expect("shutdown dispatches").push((context.mode, context.has_ui));
                context.ui.notify("attach-ui", NotificationType::Info);
                Box::pin(async { Ok(EventResult::None) })
            }));
        }
    }

    let temp = tempfile::tempdir().expect("isolated session");
    let cwd = temp.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).expect("the faux provider registers faux-1");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")), auth_path: Some(temp.path().join("auth.json")),
        providers: Some(vec![provider.provider]), ..Default::default()
    });
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(cwd.clone()), model_runtime: Some(runtime), model: Some(model),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)),
        no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default()
    }).await.expect("the session is created").session;

    let recorder = Recorder::default();
    let starts = recorder.starts.clone();
    let shutdowns = recorder.shutdowns.clone();
    session.set_extension_runner(maho_ext_host::ExtensionRunner::from_static(vec![Box::new(recorder)], session.extension_context(Arc::new(NoopUi)))).await;
    session.bind_extensions(ExtensionBindings { mode: Some(ExtensionMode::Rpc), ..Default::default() }).await;
    assert_eq!(*starts.lock().expect("startup dispatches"), vec![(ExtensionMode::Rpc, false)]);
    assert!(session.has_started_extension_lifecycle());

    let records: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = { let records = records.clone(); Arc::new(move |value: serde_json::Value| records.lock().expect("handler records").push(value)) };
    let handler = maho_rpc::connection_handler::RpcConnectionHandler::create(session.clone(), sink, Vec::new()).await;
    assert_eq!(starts.lock().expect("startup dispatches").len(), 1);

    session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
    assert_eq!(*shutdowns.lock().expect("shutdown dispatches"), vec![(ExtensionMode::Rpc, true)]);
    assert!(records.lock().expect("handler records").iter().any(|record|
        record["type"] == "extension_ui_request" && record["method"] == "notify" && record["message"] == "attach-ui"
    ));
    handler.dispose().await;
}
