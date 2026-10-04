mod support;
use std::sync::{Arc, Mutex};
use maho_ext_api::*;
use maho_omo_task::{component::TaskComponent, engine::{compose_task_engine, ComposeTaskEngineDeps}};
use senpi_task::{manager::types::{ManagedRunner, ManagedRunnerResult, ManagedStartSpec, ManagedRunners}, store::StateDirConfig, team::{liveness_ownership::TeamMemberOwnershipDeps, runtime_config::TeamTaskBounds}};

struct NoLaunch;
#[tokio::test]
async fn registered_send_observes_late_team_routing_and_propagates_resolver_failure() {
    let root = tempfile::tempdir().expect("root");
    let runner = Arc::new(NoLaunch);
    let engine = compose_task_engine(ComposeTaskEngineDeps {
        cwd: root.path().into(), config: serde_json::json!({}),
        runners: ManagedRunners { in_process: runner.clone(), process: runner },
        actions: Arc::new(Actions::default()), coordinator: None, resolve_registry: Arc::new(|| None),
    });
    let mut api = support::api();
    let ownership = TeamMemberOwnershipDeps {
        state_dir: StateDirConfig { project_dir: root.path().into(), task_state_dir: None },
        team_bounds: TeamTaskBounds { max_members: 4, max_parallel_members: 2, max_wall_clock_minutes: 10 }, load_runtime_state: None,
    };
    let component = TaskComponent::register_with_status_timers(&mut api, engine, Default::default(), TeamMemberOwnershipDeps {
        state_dir: ownership.state_dir.clone(), team_bounds: ownership.team_bounds, load_runtime_state: None,
    }, false, Arc::new(Timers::default())).expect("register").expect("component");
    let execute = api.registered.tools.iter().find(|tool| tool.definition.name == "task_send").expect("send").definition.execute.clone();
    let service = Arc::new(maho_omo_task::team_service::create_team_service(maho_omo_task::team_service::TeamServiceDeps {
        manager: component.engine.manager.clone(), member_manager: Arc::new(Members), destruction: Arc::new(Members),
        session_id: Arc::new(|| Some("session".into())), state_dir: ownership.state_dir, bounds: ownership.team_bounds,
        omo_config: serde_json::json!({}), agent_names: Default::default(), member_extension: Default::default(),
        append_task_event: None, now: None, new_message_id: None,
    }).expect("service"));
    let params = serde_json::json!({"to":"beta","message":{"type":"shutdown_request"}});
    let context = support::context();
    let before = execute(ToolCall { id: "before", params: params.clone(), signal: Default::default(), on_update: None, context: Some(&context) }).await.expect("unwired result");
    assert_eq!(before.details.expect("details")["kind"], "invalid_arguments");
    component.set_team_routing(Some(senpi_task::tools::control::send_shutdown::TaskSendTeamRouting {
        service, from: senpi_task::team::normalize::TEAM_LEAD_SENTINEL.into(), team_run_id: None,
        resolve_default_team_run_id: Some(Arc::new(|| senpi_task::tools::control::send_shutdown::DefaultTeamRunIdResolution::Failed { reason: "fixture storage failure".into() })),
    }));
    for params in [params, serde_json::json!({"to":"beta","message":"work"})] {
        let result = execute(ToolCall { id: "after", params, signal: Default::default(), on_update: None, context: Some(&context) }).await;
        assert!(matches!(result, Err(maho_ext_api::ToolError::Message(reason)) if reason == "fixture storage failure"));
    }
    assert_eq!(api.registered.tools.iter().filter(|tool| tool.definition.name == "task_send").count(), 1);
    component.dispose(); drop(execute); drop(api); drop(component); root.close().expect("cleanup");
}
struct Members;
impl senpi_task::team::runtime_types::TeamMemberReadPort for Members { fn get(&self,_:&str)->Option<senpi_task::team::runtime_types::TeamMemberTaskRecord> { None } }
impl senpi_task::team::runtime_types::TeamMemberCancelPort for Members { fn cancel_task(&self,_:&str,_:Option<&str>)->senpi_task::team::runtime_types::TeamCancelOutcome { panic!("unexpected cancellation") } }
impl senpi_task::team::runtime_types::TeamRuntimeManagerPort for Members {
    fn start(&self,_:&senpi_task::team::runtime_types::TeamMemberStartSpec)->Result<senpi_task::team::runtime_types::TeamStartResult,String> { panic!("unexpected member launch") }
    fn get_resident_handle(&self,_:&str)->Option<senpi_task::team::member_projection::ResidentSessionRef> { None }
}
impl senpi_task::team::runtime_types::TeamMemberDestructionPort for Members { fn destroy_resident_task(&self,_:&str,_:senpi_task::lifecycle::DestroyCause)->Result<(),String> { panic!("unexpected destruction") } }
#[derive(Default)]
struct Timers(Mutex<std::collections::BTreeMap<u64,Box<dyn FnOnce()+Send>>>);
impl maho_omo_task::status_ui::StatusUiTimers for Timers {
    fn set(&self,callback:Box<dyn FnOnce()+Send>,_:u64)->u64 { let mut timers=self.0.lock().expect("timers"); let id=timers.keys().next_back().copied().unwrap_or(0)+1; timers.insert(id,callback); id }
    fn clear(&self,id:u64) { self.0.lock().expect("timers").remove(&id); }
}
struct Sink;
impl senpi_task::team::messaging::lead_poller_types::LeadInjectionSink for Sink { fn enqueue(&self,_:senpi_task::team::messaging::lead_poller_types::LeadInjection) { panic!("unexpected lead injection") } }
impl ManagedRunner for NoLaunch {
    fn start(&self, _: &ManagedStartSpec) -> ManagedRunnerResult { panic!("recovery cannot launch a child") }
}
#[derive(Default)]
struct Actions(Mutex<Vec<CustomMessage>>);
struct PersistedSession;
impl ToolSessionManager for PersistedSession {
    fn session_id(&self)->&str { "session" }
    fn session_file(&self)->Option<&std::path::Path> { Some(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/liveness-session.jsonl"))) }
}
impl SessionManager for PersistedSession {
    fn get_entries(&self)->Vec<SessionEntry> { vec![] }
    fn get_branch(&self)->Vec<SessionEntry> { vec![] }
    fn get_leaf_id(&self)->Option<String> { None }
    fn get_session_name(&self)->Option<String> { None }
}
impl ExtensionActions for Actions {
    fn send_message(&self, message: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> { self.0.lock().expect("messages").push(message); Ok(()) }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { panic!("unexpected user message") }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> { Ok(()) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(vec![]) }
}

#[tokio::test]
async fn registered_start_redelivers_expired_completion_before_ttl_cleanup() {
    recovery(false).await;
}
#[tokio::test]
async fn registered_owned_member_recovery_acknowledges_actual_session_marker() {
    recovery(true).await;
}
async fn recovery(member:bool) {
    let root = tempfile::tempdir().expect("root");
    let actions = Arc::new(Actions::default());
    let runner = Arc::new(NoLaunch);
    let engine = compose_task_engine(ComposeTaskEngineDeps {
        cwd: root.path().into(), config: serde_json::json!({"task":{"ttl_ms":1}}),
        runners: ManagedRunners { in_process: runner.clone(), process: runner }, actions: actions.clone(),
        coordinator: None, resolve_registry: Arc::new(|| None),
    });
    let mut record = senpi_task::state::create_task_record(senpi_task::state::TaskRecordInput {
        parent_session_id: "session".into(), root_session_id: "session".into(), ..Default::default()
    }, Some(1)).expect("record");
    record.status = senpi_task::state::TaskStatus::Completed;
    record.notify_on_terminal = true;
    record.notification.run_epoch = 1;
    record.notification.notified_epoch = 0;
    record.created_at = "1970-01-01T00:00:00.001Z".into();
    record.updated_at = record.created_at.clone();
    // `create_task_record(Some(1))` takes its id from the process-wide monotonic floor, so the
    // created id is not stable across a test binary. Pin the expired completion to an id that can
    // never collide with the member task the fixture pins
    // (tests/fixtures/liveness-session.jsonl -> team-member-liveness:st_00000001:0).
    record.task_id = "st_00000000".into();
    engine.store.save(&record).expect("save expired completion");
    let mut member_record=record.clone();
    if member {
        member_record.task_id="st_00000001".into();
        member_record.name=Some("team:12345678-1234-4234-8234-123456789012:beta".into());
        member_record.status=senpi_task::state::TaskStatus::Error;
        member_record.notify_on_terminal=false;
        member_record.notification.run_epoch=0;
        member_record.notification.notified_epoch=0;
        member_record.updated_at="9999-01-01T00:00:00.000Z".into();
        engine.store.save(&member_record).expect("save member");
    }
    let mut api = support::api();
    let wake = Arc::new(Mutex::new(Vec::new())); let emitted = wake.clone();
    let wake_subscription = api.events.on("wake_source_state", Arc::new(move |event| {
        if event["source"] == "senpi-task" { emitted.lock().expect("wake").push(event.clone()); }
    }));
    let state_dir=StateDirConfig { project_dir:root.path().into(),task_state_dir:None };
    let team_base=senpi_task::team::storage::team_storage_base_dir(&state_dir);
    let team_config=senpi_task::team::runtime_config::to_team_core_config(&TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 },&team_base.to_string_lossy()).expect("config");
    let spec=senpi_task::team::normalize::normalize_senpi_team_spec(&serde_json::json!({"members":[{"name":"beta","kind":"category","category":"quick","prompt":"work"}]}),"squad",None).expect("spec");
    let owned_runtime=team_core::team_state_store::create_runtime_state(&spec,Some("session"),team_core::types::SpecSource::Project,&team_config).expect("runtime");
    if member { member_record.name=Some(format!("team:{}:beta",owned_runtime.team_run_id)); engine.store.save(&member_record).expect("save actual owned member"); }
    let ownership=TeamMemberOwnershipDeps {
        state_dir: StateDirConfig { project_dir: root.path().into(), task_state_dir: None },
        team_bounds: TeamTaskBounds { max_members: 4, max_parallel_members: 2, max_wall_clock_minutes: 10 }, load_runtime_state: None,
    };
    let timers=Arc::new(Timers::default());
    let component = TaskComponent::register_with_status_timers(&mut api, engine, Default::default(), TeamMemberOwnershipDeps { state_dir:ownership.state_dir.clone(),team_bounds:ownership.team_bounds,load_runtime_state:None }, false,timers.clone()).expect("register").expect("component");
    let service=Arc::new(maho_omo_task::team_service::create_team_service(maho_omo_task::team_service::TeamServiceDeps {
        manager:component.engine.manager.clone(),member_manager:Arc::new(Members),destruction:Arc::new(Members),session_id:Arc::new(|| Some("session".into())),
        state_dir:ownership.state_dir.clone(),bounds:ownership.team_bounds,omo_config:serde_json::json!({}),agent_names:Default::default(),member_extension:Default::default(),append_task_event:None,now:None,new_message_id:None,
    }).expect("team service"));
    let seen=Arc::new(Mutex::new(Vec::new())); let observed=seen.clone(); let delivered=actions.clone();
    let config=senpi_task::team::runtime_config::to_team_core_config(&ownership.team_bounds,root.path().to_str().expect("path")).expect("team config"); let runtime=root.path().to_path_buf();
    let pollers=maho_omo_task::lead_poller_lifecycle::create_lead_poller_lifecycle(maho_omo_task::lead_poller_lifecycle::LeadPollerLifecycleDeps {
        list_teams:Arc::new(move || { observed.lock().expect("seen").push(delivered.0.lock().expect("messages").iter().filter(|message| message.custom_type=="senpi-task.completion").count()); Ok(vec![]) }),
        session_id:Arc::new(|| Some("session".into())),session_file:Arc::new(|| None),parent_state:Arc::new(|| senpi_task::completion::ParentState::Idle),config,
        runtime_dir:Arc::new(move |id| runtime.join(id)),delivery_journal:None,append_event:Arc::new(|_,_| {}),sink:Arc::new(Sink),factory:None,timers:timers.clone(),on_error:Arc::new(|error| panic!("{error}")),
    });
    let liveness_delivery=actions.clone();
    let liveness=maho_omo_task::member_liveness::create_team_member_liveness_notifier(maho_omo_task::member_liveness::bind_liveness_store_markers(maho_omo_task::member_liveness::TeamMemberLivenessDeps {
        deliver:Arc::new(move |_,message,_| { liveness_delivery.0.lock().expect("messages").push(message); Ok(()) }),was_delivered:Arc::new(|_| false),mark_delivered:Arc::new(|_| panic!("binding replaces marker")),on_error:Arc::new(|error| panic!("{error}")),timers:timers.clone(),max_delivery_retries:None,max_persistence_retries:None,
    },component.engine.store.clone()));
    let handlers=api.registered.handlers[&EventKind::SessionStart].len();
    component.register_team_runtime(&mut api,service,pollers,liveness,ownership);
    assert_eq!(api.registered.handlers[&EventKind::SessionStart].len(),handlers,"team recovery belongs to the original lifecycle chain");
    let mut context = support::context(); context.cwd = root.path().into();
    if member { context.session_manager=Arc::new(PersistedSession); }
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Resume, initial_model_provenance: None, previous_session_file: None });
    for handler in &api.registered.handlers[&EventKind::SessionStart] {
        tokio::time::timeout(std::time::Duration::from_secs(5), handler(&mut event, &context))
            .await.expect("registered start deadline").expect("registered start");
    }
    assert_eq!(wake.lock().expect("wake").as_slice(), &[serde_json::json!({"source":"senpi-task","activeCount":0,"channels":[]})], "registered startup awaits its owned resumption snapshot");
    let messages = actions.0.lock().expect("messages");
    assert_eq!(messages.iter().filter(|message| message.custom_type == "senpi-task.completion").count(), 1);
    assert!(component.engine.store.load(&record.task_id).expect("load").is_none(), "expired record is cleaned only after redelivery");
    assert_eq!(*seen.lock().expect("seen"),[1],"registered lead poll occurs after completion redelivery");
    if member { assert_eq!(messages.iter().filter(|message| message.custom_type=="senpi-task.team-member-liveness").count(),1); }
    drop(messages);
    if member {
        let mut end=ExtensionEvent::AgentEnd { messages:vec![],aborted:None,will_retry:None,abort_source:None };
        for handler in &api.registered.handlers[&EventKind::AgentEnd] {
            tokio::time::timeout(std::time::Duration::from_secs(5), handler(&mut end,&context))
                .await.expect("registered agent end deadline").expect("registered agent end");
        }
        assert_eq!(component.engine.store.load(&member_record.task_id).expect("member").expect("persisted member").notification.liveness_notified_epoch,Some(0));
    }
    component.dispose();
    assert_eq!(wake.lock().expect("wake").last(), Some(&serde_json::json!({"source":"senpi-task","activeCount":0,"channels":[]})), "component disposal joins final clear emission");
    assert!(wake.lock().expect("wake").len() >= 2, "disposal must publish a separate clear after startup");
    drop(wake_subscription); drop(api); drop(component); root.close().expect("cleanup");
    assert!(timers.0.lock().expect("timers").is_empty(),"component disposal releases lead and status timers");
}
