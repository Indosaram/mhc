use std::sync::{Arc, Mutex};
use maho_omo_task::category_unavailable_warning::create_category_unavailable_warning_planner;
use senpi_task::manager::types::{ChildPlanner, PlanResolutionCode, PlanResolutionError};
use serde_json::{json, Value};
pub mod support;

#[tokio::test] async fn registered_engine_delivers_real_category_failure_without_starting_child() {
    use maho_ext_api::*;
    use maho_omo_task::{component::TaskComponent,engine::{compose_task_engine,ComposeTaskEngineDeps}};
    use senpi_task::{host::{HostError,SenpiModelRegistry},manager::types::{ManagedRunner,ManagedRunnerResult,ManagedStartSpec,ManagedRunners,ListScope}};
    struct EmptyRegistry;
    impl SenpiModelRegistry for EmptyRegistry {
        fn get_available(&self)->Result<Value,HostError> { Ok(json!([])) }
        fn find(&self,_:&str,_:&str)->Option<Value> { None }
    }
    struct NeverStarts;
    impl ManagedRunner for NeverStarts { fn start(&self,_:&ManagedStartSpec)->ManagedRunnerResult { panic!("unresolved category must never reach runner") } }
    #[derive(Default)] struct Actions(Mutex<Vec<(CustomMessage,SendMessageOptions)>>);
    impl ExtensionActions for Actions {
        fn send_message(&self,message:CustomMessage,options:SendMessageOptions)->Result<(),ExtensionFailure> { self.0.lock().expect("messages").push((message,options)); Ok(()) }
        fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> { panic!("unexpected user message") }
        fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure> { panic!("unexpected entry") }
        fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(vec![]) }
    }
    let root=tempfile::tempdir().expect("state"); let actions=Arc::new(Actions::default()); let runner=Arc::new(NeverStarts);
    let engine=compose_task_engine(ComposeTaskEngineDeps { cwd:root.path().into(),config:json!({}),runners:ManagedRunners { in_process:runner.clone(),process:runner },actions:actions.clone(),coordinator:None,resolve_registry:Arc::new(|| Some(Arc::new(EmptyRegistry))),host_transport:None });
    let mut api=support::api(); api.runtime.bind(actions.clone());
    let component=TaskComponent::register(&mut api,engine,Default::default(),senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps { state_dir:senpi_task::store::StateDirConfig { project_dir:root.path().into(),task_state_dir:None },team_bounds:senpi_task::team::runtime_config::TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 },load_runtime_state:None },false).expect("register").expect("enabled");
    assert!(api.registered.handlers.contains_key(&EventKind::ModelSelect),"assembled task component must capture model-select context and sync status");
    let mut context=support::context(); context.cwd=root.path().into();
    let model:Model=serde_json::from_value(json!({"id":"native","name":"native","api":"openai-completions","provider":"task44","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":128000,"maxTokens":4096})).expect("model");
    let mut selected=ExtensionEvent::ModelSelect(ModelSelectEvent { model,previous_model:None,source:ModelSelectSource::Set,system_prompt:String::new(),system_prompt_options:Default::default() });
    for handler in &api.registered.handlers[&EventKind::ModelSelect] { handler(&mut selected,&context).await.expect("model selection"); }
    { let runtime=component.engine.runtime.lock().expect("runtime"); assert_eq!(runtime.cwd(),root.path()); assert_eq!(runtime.session_id(),Some("session")); assert!(runtime.model_registry().is_some()); assert!(runtime.ui().is_some()); }
    let mut event=ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::New,initial_model_provenance:None,previous_session_file:None });
    for handler in &api.registered.handlers[&EventKind::SessionStart] { handler(&mut event,&context).await.expect("session start"); }
    let spec=senpi_task::manager::types::ManagerStartSpec { prompt:"work".into(),category:Some("quick".into()),parent_session_id:"session".into(),..Default::default() };
    let senpi_task::manager::types::StartResult::PlanUnresolved(original)=component.engine.manager.start(&spec) else { panic!("expected real planner failure") };
    assert_eq!(original.code,PlanResolutionCode::ModelUnavailable); assert_eq!(original.category.as_deref(),Some("quick")); assert!(!original.attempted_chain.as_ref().expect("chain").is_empty());
    let task=api.registered.tools.iter().find(|tool| tool.definition.name=="task").expect("task");
    for id in ["task44-category-failure-1","task44-category-failure-2"] {
        let result=(task.definition.execute)(ToolCall { id,params:json!({"prompt":"work","category":"quick","run_in_background":true}),signal:Default::default(),on_update:None,context:Some(&context) }).await.expect("typed failure");
        let details=result.details.expect("failure details");
        assert_eq!(details["status"],"plan_error"); assert_eq!(details["task_id"],""); assert_eq!(details["reason"],original.message);
    }
    assert!(component.engine.manager.list(&ListScope::All).is_empty());
    {
        let messages=actions.0.lock().expect("messages"); let warnings=messages.iter().filter(|(message,_)| message.custom_type=="senpi-task.category-unavailable").collect::<Vec<_>>(); assert_eq!(warnings.len(),1);
        let (message,options)=warnings[0]; assert!(message.display); assert!(!options.trigger_turn); let details=message.details.as_ref().expect("warning details"); assert_eq!(details["category"],"quick"); assert_eq!(details["reason"],"no_chain_rung_available"); assert!(!details["attempted_chain"].as_array().expect("chain").is_empty()); assert!(details["missing_providers"].as_array().expect("providers").iter().any(|provider| provider=="openai-codex"));
    }
    for name in ["omo.task.send","omo.task.cancel","omo.task.output"] { let response=(api.registered.rpc_handlers[name])(json!([])).await.expect("invalid request"); assert_eq!(response["kind"],"invalid_arguments"); }
    component.dispose(); drop(api); drop(component); root.close().expect("state cleanup");
}

#[test] fn native_warning_delivery_notifies_when_captured_and_never_triggers_turn() {
    use maho_ext_api::*;
    #[derive(Default)] struct Actions(Mutex<Vec<(CustomMessage,SendMessageOptions)>>);
    impl ExtensionActions for Actions {
        fn send_message(&self,message:CustomMessage,options:SendMessageOptions)->Result<(),ExtensionFailure> { self.0.lock().expect("messages").push((message,options)); Ok(()) }
        fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> { panic!("not a user message") }
        fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure> { panic!("not an entry") }
        fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(vec![]) }
    }
    let actions=Actions::default(); let ui=support::Ui::default(); let details=json!({"category":"quick","reason":"no_chain_rung_available"});
    for ui in [None,Some(&ui as &dyn ExtensionUi)] { maho_omo_task::category_unavailable_warning::deliver_category_warning(&actions,ui,"unavailable",details.clone()).expect("delivery"); }
    assert_eq!(ui.notifications.lock().expect("notifications").len(),1); let messages=actions.0.lock().expect("messages"); assert_eq!(messages.len(),2);
    for (message,options) in messages.iter() { assert_eq!(message.custom_type,"senpi-task.category-unavailable"); assert!(message.display); assert_eq!(message.details.as_ref(),Some(&details)); assert!(!options.trigger_turn); assert_eq!(options.deliver_as,None); }
}

fn run(config: Value, settings: Value, dead_chain: bool, repetitions: usize) -> usize {
    let planner: ChildPlanner = Arc::new(move |_| {
        let mut error = PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, "unavailable");
        error.category = Some("quick".into());
        if dead_chain { error.attempted_chain = Some(vec![]); }
        error.missing_providers = Some(vec!["faux".into()]);
        Err(Box::new(error))
    });
    let messages = Arc::new(Mutex::new(Vec::new()));
    let captured = messages.clone();
    let planner = create_category_unavailable_warning_planner(planner, config, settings,
        Arc::new(|| Some("session".into())), Arc::new(move |_, details| {
            captured.lock().expect("valid test state").push(details);
        }));
    for _ in 0..repetitions {
        assert!(planner(&Default::default()).is_err());
    }
    let result = messages.lock().expect("valid test state");
    for message in result.iter() {
        assert_eq!(message["category"], "quick");
        assert_eq!(message["reason"], "no_chain_rung_available");
        assert_eq!(message["missing_providers"], json!(["faux"]));
    }
    result.len()
}

#[test] fn dead_chain_warns_once_per_session_category() {
    assert_eq!(run(json!({}), json!({}), true, 2), 1);
}
#[test] fn plain_model_miss_never_warns() {
    assert_eq!(run(json!({}), json!({}), false, 1), 0);
}
#[test] fn global_warning_suppression() {
    assert_eq!(run(json!({}), json!({"warnings":{"unavailable_categories":false}}), true, 1), 0);
}
#[test] fn category_opt_in_overrides_global_suppression() {
    assert_eq!(run(json!({"categories":{"quick":{"warn_unavailable":true}}}),
        json!({"warnings":{"unavailable_categories":false}}), true, 1), 1);
}
#[test] fn category_opt_out_overrides_default() {
    assert_eq!(run(json!({"categories":{"quick":{"warn_unavailable":false}}}), json!({}), true, 1), 0);
}
#[test] fn user_model_never_warns() {
    assert_eq!(run(json!({"categories":{"quick":{"model":"faux/custom"}}}), json!({}), true, 1), 0);
}
#[test] fn session_switch_allows_one_new_warning_without_mutating_error() {
    let session=Arc::new(Mutex::new(None)); let current=session.clone(); let calls=Arc::new(Mutex::new(Vec::new())); let sink=calls.clone();
    let mut error=PlanResolutionError::new(PlanResolutionCode::ModelUnavailable,"unavailable"); error.category=Some("quick".into()); error.attempted_chain=Some(vec![]); error.available_categories=Some(vec!["writing".into()]); let expected=error.clone();
    let planner=create_category_unavailable_warning_planner(Arc::new(move |_| Err(Box::new(error.clone()))),json!({}),json!({}),Arc::new(move || current.lock().expect("session").clone()),Arc::new(move |_,details| sink.lock().expect("calls").push(details)));
    assert!(calls.lock().expect("calls").is_empty());
    for _ in 0..2 { assert_eq!(*planner(&Default::default()).expect_err("dead chain"),expected); }
    *session.lock().expect("session")=Some("next".into()); assert_eq!(*planner(&Default::default()).expect_err("dead chain"),expected);
    let calls=calls.lock().expect("calls"); assert_eq!(calls.len(),2); assert_eq!(calls[0]["missing_providers"],json!([])); assert_eq!(calls[0]["available_categories"],json!(["writing"]));
}
