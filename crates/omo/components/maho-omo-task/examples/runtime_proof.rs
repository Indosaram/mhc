#[path = "../tests/support/mod.rs"]
mod support;
use std::{collections::BTreeMap, sync::{Arc, Mutex, Condvar, mpsc}, time::Duration};
use maho_ext_api::*;
use maho_omo_task::{component::TaskComponent, engine::{ComposeTaskEngineDeps, compose_task_engine}};
use senpi_task::{host::HostError, manager::{ManagedChildHandle, ManagedChildListener, Unsubscribe, types::{ManagedRunner, ManagedRunnerResult, ManagedStartSpec, ManagedRunners}}, runners::RunnerOutcome};
use serde_json::{Value, json};
struct Child { id: String, outcome: Mutex<Option<RunnerOutcome>>, ready: Condvar, listeners: Arc<Mutex<BTreeMap<u64, ManagedChildListener>>>, activity_on_subscribe: bool }
impl Child {
    fn finish(&self) { *self.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(RunnerOutcome::completed("native result")); self.ready.notify_all(); }
}
impl ManagedChildHandle for Child {
    fn task_id(&self)->&str { &self.id }
    fn session_id(&self)->Option<String> { Some("native-child".into()) }
    fn pid(&self)->Option<i64> { None }
    fn steer(&self,_:&str)->Result<(),HostError> { Ok(()) }
    fn follow_up(&self,_:&str)->Result<(),HostError> { Ok(()) }
    fn abort(&self)->Result<(),HostError> { self.finish(); Ok(()) }
    fn subscribe(&self, listener:ManagedChildListener)->Unsubscribe {
        let mut listeners = self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = listeners.keys().next_back().copied().unwrap_or(0) + 1;
        listeners.insert(id, listener);
        let activity = self.activity_on_subscribe.then(|| listeners.values().cloned().collect::<Vec<_>>());
        drop(listeners);
        if let Some(listeners) = activity {
            for listener in listeners { listener(&senpi_task::shared::ManagedChildEvent { event_type:"tool_execution_start".into(), tool_name:Some("read".into()), args:Some(json!({"path":"task44-activity"})), ..Default::default() }); }
        }
        let listeners = self.listeners.clone();
        Box::new(move || { listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id); })
    }
    fn wait_for_outcome(&self)->RunnerOutcome {
        let guard = self.outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut guard, result) = self.ready.wait_timeout_while(guard, Duration::from_secs(10), |outcome| outcome.is_none()).unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!result.timed_out(), "child outcome not signaled");
        guard.take().unwrap_or(RunnerOutcome::Cancelled)
    }
    fn last_assistant_text(&self)->Option<String> { None }
    fn dispose(&self)->Result<(),HostError> { self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear(); Ok(()) }
}
struct Runner(Arc<Mutex<Option<Arc<Child>>>>);
impl ManagedRunner for Runner {
    fn start(&self,spec:&ManagedStartSpec)->ManagedRunnerResult {
        let child = Arc::new(Child { id:spec.task_id.clone(), outcome:Mutex::new(None), ready:Condvar::new(), listeners:Arc::default(), activity_on_subscribe:false });
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(child.clone()); Ok(child)
    }
}
struct ImmediateRunner(Mutex<Option<mpsc::Receiver<()>>>);
impl ManagedRunner for ImmediateRunner {
    fn start(&self,spec:&ManagedStartSpec)->ManagedRunnerResult {
        if let Some(first_event) = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            first_event.recv_timeout(Duration::from_secs(10)).map_err(|error| senpi_task::manager::types::ManagedRunnerError::Other(error.to_string()))?;
        }
        Ok(Arc::new(Child { id:spec.task_id.clone(), outcome:Mutex::new(Some(RunnerOutcome::completed("dag native result"))), ready:Condvar::new(), listeners:Arc::default(), activity_on_subscribe:true }))
    }
}
struct Actions(mpsc::Sender<CustomMessage>);
#[derive(Default)]
struct SurfaceTimers { callbacks: Mutex<BTreeMap<u64, Box<dyn FnOnce() + Send>>>, kinds: Mutex<BTreeMap<u64, u64>>, next: Mutex<u64> }
impl maho_omo_task::status_ui::StatusUiTimers for SurfaceTimers {
    fn set(&self, callback: Box<dyn FnOnce() + Send>, delay_ms: u64) -> u64 {
        let mut next = self.next.lock().unwrap_or_else(std::sync::PoisonError::into_inner); *next += 1;
        self.callbacks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(*next, callback);
        self.kinds.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(*next, delay_ms); *next
    }
    fn clear(&self, handle: u64) {
        self.callbacks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&handle);
        self.kinds.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&handle);
    }
}
impl SurfaceTimers {
    fn count(&self) -> usize { self.callbacks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len() }
    // The armed timers' scheduled delays, so a count mismatch names which timer kind is live.
    fn kinds(&self) -> Vec<u64> { self.kinds.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values().copied().collect() }
    fn fire(&self) {
        let callbacks = std::mem::take(&mut *self.callbacks.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        self.kinds.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        for callback in callbacks.into_values() { callback(); }
    }
}
struct Cleanup(Arc<TaskComponent>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for entry in self.0.engine.manager.list(&senpi_task::manager::types::ListScope::All) {
            if !entry.record.status.is_terminal()
                && let Err(error) = self.0.engine.manager.cancel_task(&entry.record.task_id, Some("proof cleanup"), Default::default()) {
                eprintln!("proof child cleanup failed: {error}");
            }
        }
        self.0.dispose();
        for entry in self.0.engine.manager.list(&senpi_task::manager::types::ListScope::All) {
            self.0.engine.manager.forget(&entry.record.task_id);
        }
    }
}
impl ExtensionActions for Actions {
    fn send_message(&self,message:CustomMessage,_:SendMessageOptions)->Result<(),ExtensionFailure> { self.0.send(message).map_err(|error| ExtensionFailure::new(error.to_string())) }
    fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> { Err(ExtensionFailure::new("unexpected user message")) }
    fn append_entry(&self,_:&str,_:Option<Value>)->Result<(),ExtensionFailure> { Err(ExtensionFailure::new("unexpected entry")) }
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(vec![]) }
}
async fn dispatch(api:&ExtensionApi, kind:EventKind, event:&mut ExtensionEvent, context:&ExtensionContext)->Result<(),ExtensionFailure> {
    for handler in api.registered.handlers.get(&kind).into_iter().flatten() { handler(event,context).await?; } Ok(())
}
#[tokio::main(flavor="current_thread")]
pub async fn main()->Result<(),Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let path = root.path().to_path_buf();
    let (messages, delivered) = mpsc::channel();
    let actions = Arc::new(Actions(messages));
    let child_slot = Arc::new(Mutex::new(None));
    let runner = Arc::new(Runner(child_slot.clone()));
    for member in [true, false] {
        let engine = compose_task_engine(ComposeTaskEngineDeps { cwd:path.clone(), config:json!({}), runners:ManagedRunners { in_process:runner.clone(), process:runner.clone() }, actions:actions.clone(), coordinator:None, resolve_registry:Arc::new(|| None) });
        let mut gated = support::api();
        if !member { gated.set_flag("omo-task", FlagValue::Boolean(false)); }
        assert!(TaskComponent::register(&mut gated, engine, Default::default(), senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps { state_dir:senpi_task::store::StateDirConfig { project_dir:path.clone(), task_state_dir:None }, team_bounds:senpi_task::team::runtime_config::TeamTaskBounds { max_members:4, max_parallel_members:2, max_wall_clock_minutes:10 }, load_runtime_state:None }, member)?.is_none());
        assert!(gated.registered.tools.is_empty());
        assert!(gated.registered.handlers.is_empty());
    }
    println!("PASS member early return and disabled component register no task surfaces");
    let engine = compose_task_engine(ComposeTaskEngineDeps { cwd:path.clone(), config:json!({}), runners:ManagedRunners { in_process:runner.clone(), process:runner }, actions:actions.clone(), coordinator:None, resolve_registry:Arc::new(|| None) });
    let mut api = support::api();
    api.runtime.bind(actions);
    let channels = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sink = channels.clone();
    let subscription = api.events.on("wake_source_state", Arc::new(move |event| sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone())));
    let component = TaskComponent::register(&mut api, engine, Default::default(), senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps { state_dir:senpi_task::store::StateDirConfig { project_dir:path.clone(), task_state_dir:None }, team_bounds:senpi_task::team::runtime_config::TeamTaskBounds { max_members:4, max_parallel_members:2, max_wall_clock_minutes:10 }, load_runtime_state:None }, false)?.ok_or("component disabled")?;
    let cleanup = Cleanup(component.clone());
    let mut context = support::context(); context.cwd = path;
    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::New, initial_model_provenance:None, previous_session_file:None }), &context).await?;
    let task = api.registered.tools.iter().find(|tool| tool.definition.name == "task").ok_or("task missing")?;
    let cancelled = AbortSignal::default(); cancelled.abort();
    assert!((task.definition.execute)(ToolCall { id:"cancelled-launch", params:json!({"prompt":"never launch","subagent_type":"explore","model":"faux/native","run_in_background":true}), signal:cancelled, on_update:None, context:Some(&context) }).await.is_err());
    assert!(child_slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none());
    let result = (task.definition.execute)(ToolCall { id:"native-launch", params:json!({"prompt":"produce native result","subagent_type":"explore","model":"faux/native","run_in_background":true}), signal:Default::default(), on_update:None, context:Some(&context) }).await?;
    println!("REGISTERED TASK {}", serde_json::to_string(&result.details)?);
    assert!(channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|event| event["activeCount"] == 1));
    let child = child_slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().ok_or("child not launched")?;
    child.finish();
    let message = delivered.recv_timeout(Duration::from_secs(10))?;
    assert_eq!(message.custom_type, "senpi-task.completion");
    assert!(serde_json::to_string(&message.details)?.contains("native result"));
    let deadline = Duration::from_secs(10);
    component.engine.manager.wait_for(&child.id, None, Some(deadline))?;
    let output = (api.registered.rpc_handlers["omo.task.output"])(json!({"task_id":child.id})).await?;
    assert!(serde_json::to_string(&output)?.contains("native result"));
    dispatch(&api, EventKind::SessionBeforeSwitch, &mut ExtensionEvent::SessionBeforeSwitch { reason:SessionReason::Resume, target_session_file:None }, &context).await?;
    assert_eq!((api.registered.rpc_handlers["omo.task.output"])(json!({"task_id":child.id})).await?["kind"], "unavailable");
    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::Resume, initial_model_provenance:None, previous_session_file:None }), &context).await?;
    assert!(delivered.try_recv().is_err(), "terminal completion replayed twice");
    let (progress, started) = tokio::sync::oneshot::channel();
    let progress = Mutex::new(Some(progress));
    let mut invocation = (task.definition.execute)(ToolCall { id:"dropped-foreground", params:json!({"prompt":"drop foreground invocation","subagent_type":"explore","model":"faux/native","run_in_background":false}), signal:Default::default(), on_update:Some(Arc::new(move |_| {
        if let Some(progress) = progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() { let _ = progress.send(()); }
        Ok(())
    })), context:Some(&context) });
    tokio::select! {
        result = &mut invocation => panic!("foreground invocation settled before drop: {result:?}"),
        result = started => result?,
    }
    drop(invocation);
    let dropped = child_slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().ok_or("dropped child missing")?;
    component.engine.manager.cancel_task(&dropped.id, Some("registered lifecycle cleanup"), Default::default())?;
    component.engine.manager.forget(&dropped.id);
    println!("PASS dropped registered foreground invocation settles owned executor before lifecycle cleanup");
    let running = (task.definition.execute)(ToolCall { id:"shutdown-launch", params:json!({"prompt":"suspend without dropping notification obligation","subagent_type":"explore","model":"faux/native","run_in_background":true}), signal:Default::default(), on_update:None, context:Some(&context) }).await?;
    let running_id = running.details.as_ref().and_then(|details| details["task_id"].as_str()).ok_or("shutdown child missing")?;
    dispatch(&api, EventKind::SessionShutdown, &mut ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit, target_session_file:None, signal:None }), &context).await?;
    let suspended = component.engine.store.load(running_id)?.ok_or("suspended record missing")?;
    assert_eq!(suspended.residency_state, senpi_task::state::ResidencyState::PersistedOnly);
    assert!(suspended.notify_on_terminal);
    assert!(suspended.notification.notified_epoch < suspended.notification.run_epoch);
    assert!(component.engine.manager.get_resident_handle(running_id).is_none());
    println!("PASS registered shutdown joins active completion waiter and preserves persisted notification obligation");
    assert_eq!(channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last().ok_or("no clear")?["activeCount"], 0);
    component.engine.manager.forget(&child.id);
    drop(cleanup);
    assert!(child.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
    drop(subscription); drop(api); drop(component); drop(child);
    let (entered, first_event) = mpsc::channel();
    let runner = Arc::new(ImmediateRunner(Mutex::new(Some(first_event))));
    let dag_task_engine = compose_task_engine(ComposeTaskEngineDeps { cwd:root.path().into(), config:json!({"task":{"dag":{"subscriber_ring":1}}}), runners:ManagedRunners { in_process:runner.clone(), process:runner }, actions:Arc::new(Actions(mpsc::channel().0)), coordinator:None, resolve_registry:Arc::new(|| None) });
    let dag = Arc::new(maho_omo_task::dag_engine::TaskDagEngine::compose(&dag_task_engine, None)?);
    let started = dag.manager.start(senpi_task::dag::manager::DagStartParams {
        definition: senpi_task::dag::graph::DagDefinition { key:"configured-ring".into(), name:"configured ring".into(), nodes:vec![senpi_task::dag::graph::DagNodeInput {
            id:"one".into(), prompt:"one".into(), target:senpi_task::dag::types::DagNodeTarget::SubagentType { subagent_type:"explore".into(), model:Some("faux/native".into()) },
            label:None, depends_on:None, task_summary:None, description:None, load_skills:None,
        }] }, parent_session_id:"s".into(), root_session_id:"s".into(),
    })?;
    let configured = dag.scheduler(&dag_task_engine, &started.snapshot.run_id, "s")?;
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let overflow = Arc::new(Mutex::new(Vec::new()));
    let captured = overflow.clone();
    let entered = Mutex::new(Some(entered));
    let subscription = configured.subscribe(Arc::new(move |event| {
        if let Some(entered) = entered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            if let Err(error) = entered.send(()) { eprintln!("ring proof entry failed: {error}"); }
            if let Err(error) = released.lock().unwrap_or_else(std::sync::PoisonError::into_inner).recv_timeout(Duration::from_secs(10)) { panic!("ring proof release failed: {error}"); }
        }
        if matches!(event.payload, senpi_task::dag::types::DagRunEventPayload::StreamOverflow { .. }) { captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone()); }
    }));
    let running = configured.clone();
    let worker = std::thread::spawn(move || running.run());
    let completion = worker.join().map_err(|_| "configured scheduler panicked")??;
    release.send(())?;
    configured.when_idle();
    subscription();
    assert_eq!(completion.status, senpi_task::dag::types::DagRunStatus::Completed);
    {
        let overflow = overflow.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!overflow.is_empty(), "configured subscriber ring did not overflow during gated burst");
        let durable = dag.store.read_events(&started.snapshot.run_id, 0, &senpi_task::dag::store::DagEventReadOptions { limit:1000, ..Default::default() })?;
        assert!(overflow.iter().all(|event| durable.events.contains(event)));
    }
    drop(configured);
    println!("PASS configured assembled scheduler ring overflow persisted and subscriber drained");
    let mut api = support::api();
    let component = TaskComponent::register(&mut api, dag_task_engine, Default::default(), senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps { state_dir:senpi_task::store::StateDirConfig { project_dir:root.path().into(), task_state_dir:None }, team_bounds:senpi_task::team::runtime_config::TeamTaskBounds { max_members:4, max_parallel_members:2, max_wall_clock_minutes:10 }, load_runtime_state:None }, false)?.ok_or("dag component disabled")?;
    let cleanup = Cleanup(component.clone());
    let dag_ui = Arc::new(support::Ui::default());
    context.ui = dag_ui.clone();
    context.mode = ExtensionMode::Tui;
    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::New, initial_model_provenance:None, previous_session_file:None }), &context).await?;
    dag.register_queries(&mut api, &component)?;
    let status_timers = Arc::new(SurfaceTimers::default());
    dag.register_rpc_with_status_timers(&mut api, &component, status_timers.clone());
    let pending = dag.manager.start(senpi_task::dag::manager::DagStartParams {
        definition: senpi_task::dag::graph::DagDefinition { key:"lifecycle".into(), name:"lifecycle".into(), nodes:vec![senpi_task::dag::graph::DagNodeInput {
            id:"pending".into(), prompt:"pending".into(), target:senpi_task::dag::types::DagNodeTarget::SubagentType { subagent_type:"explore".into(), model:Some("faux/native".into()) },
            label:None, depends_on:None, task_summary:None, description:None, load_skills:None,
        }] }, parent_session_id:context.session_manager.session_id().into(), root_session_id:context.session_manager.session_id().into(),
    })?;
    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::Resume, initial_model_provenance:None, previous_session_file:None }), &context).await?;
    assert_eq!(status_timers.count(), 1, "live registered DAG requires one refresh timer (kinds={:?})", status_timers.kinds());
    dispatch(&api, EventKind::SessionBeforeSwitch, &mut ExtensionEvent::SessionBeforeSwitch { reason:SessionReason::Resume, target_session_file:None }, &context).await?;
    assert_eq!(status_timers.count(), 0, "before-switch must cancel registered status timers (kinds={:?})", status_timers.kinds());
    let paints = dag_ui.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len();
    status_timers.fire();
    assert_eq!(dag_ui.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), paints);
    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::Resume, initial_model_provenance:None, previous_session_file:None }), &context).await?;
    assert_eq!(status_timers.count(), 1, "rebind must restore exactly one refresh timer (kinds={:?})", status_timers.kinds());
    assert!(dag.manager.snapshot(&pending.snapshot.run_id, context.session_manager.session_id()).is_ok());
    let dag_channels = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_channels = dag_channels.clone();
    let wake_subscription = api.events.on("wake_source_state", Arc::new(move |event| {
        if event["source"] == "omo-dag" { captured_channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone()); }
    }));
    let (terminal, settled) = mpsc::channel();
    let (activity, observed_activity) = mpsc::channel();
    let ledger = Arc::new(Mutex::new(Vec::<u64>::new()));
    let entries = ledger.clone();
    let dag_subscription = api.events.on("senpi:extension-rpc-event", Arc::new(move |event| {
        if event["name"] == "omo.dag.activity" && let Err(error) = activity.send(event["data"].clone()) { eprintln!("DAG activity delivery failed: {error}"); }
        if event["name"] == "omo.dag.event" {
            if let Some(seq) = event["data"]["seq"].as_u64() { entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(seq); }
            if event["data"]["type"] == "dag.run.completed" && let Err(error) = terminal.send(event["data"].clone()) { eprintln!("DAG proof terminal delivery failed: {error}"); }
        }
    }));
    dag.register_tool(&mut api, component.clone());
    let tool = &api.registered.tools.iter().find(|tool| tool.definition.name == "dag").ok_or("dag missing")?.definition;
    let result = tokio::time::timeout(Duration::from_secs(10), (tool.execute)(ToolCall { id:"native-dag", params:json!({"action":"start","definition":{"key":"native-proof","name":"native","nodes":[{"id":"one","prompt":"one","subagent_type":"explore","model":"faux/native"},{"id":"two","prompt":"two","subagent_type":"explore","model":"faux/native","dependsOn":["one"]}]}}), signal:Default::default(), on_update:None, context:Some(&context) })).await??;
    let run = result.details.as_ref().and_then(|details| details["run_id"].as_str()).ok_or("dag run missing")?;
    let scheduler = dag.scheduler(&component.engine, run, "s")?;
    let record = scheduler.snapshot();
    let terminal = settled.recv_timeout(Duration::from_secs(10))?;
    let activity = observed_activity.recv_timeout(Duration::from_secs(10))?;
    assert_eq!(activity["runId"], run);
    assert_eq!(activity["schemaVersion"], 1);
    assert!(activity["currentTool"].as_str().is_some_and(|tool| tool.contains("task44-activity")));
    assert!(activity.get("seq").is_none(), "activity must stay off the durable sequence ledger");
    println!("PASS registered DAG child progress reaches unsequenced RPC activity");
    // register_tool awaits the scheduler.run() oneshot and joins its worker before
    // returning; run_waves commits RunCompleted/RunFailed before returning.
    // This snapshot is causally after terminal commit, not a timing assumption.
    drop(cleanup);
    dispatch(&api, EventKind::SessionShutdown, &mut ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit, target_session_file:None, signal:None }), &context).await?;
    assert_eq!(status_timers.count(), 0, "shutdown must cancel registered status timers");
    let paints = dag_ui.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len();
    status_timers.fire();
    assert_eq!(dag_ui.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), paints);
    println!("PASS registered DAG before-switch cancellation, rebind and shutdown timer cleanup");
    assert_eq!(terminal["runId"], run);
    let sequences = ledger.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    drop(sequences); drop(dag_subscription);
    assert_eq!(record.status, senpi_task::dag::types::DagRunStatus::Completed);
    assert_eq!(record.nodes.len(), 2);
    assert_eq!(record.waves.len(), 2);
    {
        let channels = dag_channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(channels.iter().any(|event| event["activeCount"].as_u64().is_some_and(|count| count > 0)), "registered DAG start did not publish a live wake channel");
        assert_eq!(channels.last().ok_or("no DAG shutdown clear")?["activeCount"], 0);
    }
    drop(wake_subscription);
    {
        let widgets = dag_ui.widgets.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(widgets.iter().any(|(content, placement)| *placement == WidgetPlacement::BelowEditor && matches!(content, Some(WidgetContent::Lines(rows)) if rows.iter().any(|row| row.contains("native")))), "registered DAG did not render below editor");
    }
    println!("PASS registered DAG live wake source and shutdown clear");
    component.dispose();
    for entry in component.engine.manager.list(&senpi_task::manager::types::ListScope::All) { component.engine.manager.forget(&entry.record.task_id); }
    drop(scheduler); drop(dag); drop(component); drop(api);
    println!("PASS registered DAG composition and two-wave scheduler execution with injected runner");
    root.close()?;
    println!("PASS registered composition/tool/session launch with injected runner, manager terminal notification, active channel, explicit clear");
    println!("cleanup: completion waiters joined, child forgotten, timers cancelled, event subscription dropped, temporary state removed");
    Ok(())
}
