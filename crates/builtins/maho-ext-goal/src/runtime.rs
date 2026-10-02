use std::sync::{Arc,Mutex};
use maho_ext_api::{ExtensionContext,ExtensionEvent,ExtensionFailure};
use crate::{accounting_hooks::GoalStoreReference,index::GoalTurnAccounting,types::*,monitor_continuation::MonitorAwareGoalContinuation,direct_input_lifecycle::{GoalDirectInputLifecycle,DirectInputGoalChange}};
struct State {
    context:Option<ExtensionContext>,accounting:GoalTurnAccounting,input:GoalDirectInputLifecycle,
    ticker:crate::elapsed_ticker::GoalElapsedTicker,
}
pub struct GoalRuntime {
    state:tokio::sync::Mutex<State>,pub monitor:Arc<Mutex<MonitorAwareGoalContinuation>>,
    reference:GoalStoreReference,now:Arc<dyn Fn()->f64+Send+Sync>,
}
impl GoalRuntime {
    pub fn new(reference:GoalStoreReference,now:Arc<dyn Fn()->f64+Send+Sync>)->Self {
        let render=Arc::new(|context:&ExtensionContext,goal:&Goal,elapsed:f64| { crate::ui::update_goal_ui(context,Some(goal),Some(elapsed)); Ok(()) });
        Self { state:tokio::sync::Mutex::new(State { context:None,accounting:Default::default(),input:Default::default(),ticker:crate::elapsed_ticker::GoalElapsedTicker::new(render,now.clone()) }),monitor:Arc::new(Mutex::new(Default::default())),reference,now }
    }
    async fn refresh(&self,state:&mut State,context:&ExtensionContext,goal:Option<&Goal>)->Result<(),ExtensionFailure> {
        self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.sync_goal(goal);
        state.accounting.refresh_ui(&mut state.ticker,context,goal).await
    }
    pub async fn create(&self,context:&ExtensionContext,objective:&str)->Result<maho_ext_api::ToolResult,maho_ext_api::ToolError> {
        let mut state=self.state.lock().await; let now=(self.now)();
        let (goal,result)=crate::tool_registration::execute_create_goal(&(self.reference)(context),objective,(now/1000.0).floor() as u64).await?;
        state.accounting.begin(&goal,now); self.monitor.lock().map_err(|error|maho_ext_api::ToolError::Message(error.to_string()))?.note_continuation_started();
        self.refresh(&mut state,context,Some(&goal)).await.map_err(|error|maho_ext_api::ToolError::Message(error.message))?; Ok(result)
    }
    pub async fn update(&self,context:&ExtensionContext,params:&serde_json::Value,open_tasks:&[String])->Result<maho_ext_api::ToolResult,maho_ext_api::ToolError> {
        let mut state=self.state.lock().await;
        let (goal,result)=crate::tool_registration::execute_update_goal(&(self.reference)(context),&mut state.accounting,params,open_tasks,(self.now)()).await?;
        self.refresh(&mut state,context,Some(&goal)).await.map_err(|error|maho_ext_api::ToolError::Message(error.message))?; Ok(result)
    }
    pub async fn get(&self,context:&ExtensionContext)->Result<maho_ext_api::ToolResult,maho_ext_api::ToolError> {
        let mut state=self.state.lock().await;
        let (goal,result)=crate::tool_registration::execute_get_goal(&(self.reference)(context),&mut state.accounting,(self.now)()).await?;
        self.refresh(&mut state,context,goal.as_ref()).await.map_err(|error|maho_ext_api::ToolError::Message(error.message))?; Ok(result)
    }
    pub fn register_command(self:&Arc<Self>,api:&mut maho_ext_api::ExtensionApi,queue:crate::command_registration::QueueGoalContinuation) {
        let account=self.clone(); let begin=self.clone(); let stop=self.clone(); let clear=self.clone(); let refresh=self.clone(); let now=self.now.clone();
        crate::command_registration::register_goal_command(api,Arc::new(crate::command_registration::GoalCommandRegistrationDeps {
            goal_store_ref:self.reference.clone(),now:Arc::new(move ||(now()/1000.0).floor() as u64),
            account_current_agent_turn:Arc::new(move |context,mode| { let runtime=account.clone(); Box::pin(async move { let mut state=runtime.state.lock().await; let now=(runtime.now)(); state.accounting.account(&(runtime.reference)(context),mode,None,now,(now/1000.0).floor() as u64).await.map_err(failure) }) }),
            begin_agent_goal_accounting:Arc::new(move |goal| { let runtime=begin.clone(); Box::pin(async move { runtime.state.lock().await.accounting.begin(goal,(runtime.now)()); Ok(()) }) }),
            stop_agent_goal_accounting:Arc::new(move |id| { let runtime=stop.clone(); Box::pin(async move { runtime.state.lock().await.accounting.stop(id); Ok(()) }) }),
            clear_agent_goal_accounting:Arc::new(move || { let runtime=clear.clone(); Box::pin(async move { runtime.state.lock().await.accounting.clear(); Ok(()) }) }),
            refresh_goal_ui:Arc::new(move |context,goal| { let runtime=refresh.clone(); Box::pin(async move { let mut state=runtime.state.lock().await; runtime.refresh(&mut state,context,goal).await }) }),
            queue_goal_continuation:queue,
        }));
    }
    pub async fn event(&self,event:&ExtensionEvent,context:&ExtensionContext)->Result<Option<Goal>,ExtensionFailure> {
        let mut state=self.state.lock().await; let reference=(self.reference)(context); let now=(self.now)(); let seconds=(now/1000.0).floor() as u64;
        let mut goal=if matches!(event,ExtensionEvent::SessionStart(_)) { None } else { crate::store::read_goal(&reference).map_err(failure)? };
        match event {
            ExtensionEvent::SessionStart(_)=>{
                state.accounting.clear(); state.input.reset();
                self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.dispose();
                crate::persistence::migrate_legacy_goal_file(&reference,&context.agent_dir).map_err(failure)?;
                goal=crate::store::read_goal(&reference).map_err(failure)?;
                state.context=Some(context.clone());
                if let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Active) { state.accounting.begin(goal,now); }
            },
            ExtensionEvent::AgentStart=>state.accounting.agent_start(goal.as_ref(),now),
            ExtensionEvent::MessageEnd { message }=>{ state.accounting.usage.note_message_end(message); return Ok(goal); },
            ExtensionEvent::AgentEnd { messages,aborted,abort_source,will_retry }=>{
                goal=state.accounting.agent_end(&reference,messages,*aborted==Some(true)&&*abort_source==Some(maho_ext_api::AbortSource::User),now,seconds).await.map_err(failure)?;
                let ended=maho_core::agent_abort_provenance::AgentEndEvent { messages:messages.clone(),aborted:aborted.unwrap_or(false),abort_source:*abort_source,will_retry:will_retry.unwrap_or(false) };
                if crate::agent_end_continuation::goal_agent_end_route(goal.as_ref(),&ended)==crate::agent_end_continuation::GoalAgentEndRoute::PolicyBlock {
                    goal=Some(crate::store::update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Blocked),reason:Some("provider policy rejection ended the turn".into()),..Default::default() },GoalUpdateSource::Model,seconds).await.map_err(failure)?);
                    state.accounting.clear();
                }
            },
            ExtensionEvent::Input(input)=>{
                let mut monitor=self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                state.input.on_input(input,&reference,|id|monitor.hold_direct_input(id,now)).map_err(failure)?;
                return Ok(goal);
            },
            ExtensionEvent::InputDisposition { input_id,disposition }=>{
                let monitor=self.monitor.clone();
                if let Some(change)=state.input.on_disposition(input_id,*disposition,&reference,seconds,move |id,accepted|monitor.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resolve_direct_input(id,accepted,now)).await.map_err(failure)? {
                    let current=match change { DirectInputGoalChange::Reactivated(goal)|DirectInputGoalChange::Active { goal,.. }=>goal };
                    state.accounting.begin(&current,now); goal=Some(current);
                }
            },
            ExtensionEvent::SessionAbort=>{
                if goal.as_ref().is_some_and(|goal|goal.status==GoalStatus::Active) {
                    state.accounting.account(&reference,GoalAccountingMode::Active,None,now,seconds).await.map_err(failure)?;
                    goal=Some(crate::store::update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Blocked),reason:Some("user interrupted the turn".into()),..Default::default() },GoalUpdateSource::Model,seconds).await.map_err(failure)?); state.accounting.clear();
                }
            },
            ExtensionEvent::SessionShutdown(_)=>{
                if state.accounting.window.is_some() { goal=state.accounting.account(&reference,GoalAccountingMode::Active,None,now,seconds).await.map_err(failure)?; }
                state.accounting.clear(); state.context=None;
                self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.dispose();
                if let Some(worker)=state.ticker.stop()? { match worker.await { Ok(result)=>result?,Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(ExtensionFailure::new(error.to_string())) } }
                return Ok(goal);
            },_=>return Ok(goal),
        }
        self.refresh(&mut state,context,goal.as_ref()).await?; Ok(goal)
    }
    pub async fn store_changed(&self,thread_id:&str)->Result<Option<Goal>,ExtensionFailure> {
        let mut state=self.state.lock().await;
        let Some(context)=state.context.clone().filter(|context|context.session_manager.session_id()==thread_id) else { return Ok(None); };
        if !context.is_idle()||context.has_pending_messages()? { return Ok(None); }
        let goal=crate::store::read_goal(&(self.reference)(&context)).map_err(failure)?;
        if !goal.as_ref().is_some_and(|goal|goal.status==GoalStatus::Active) { return Ok(None); }
        if let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Active) { state.accounting.begin(goal,(self.now)()); }
        self.refresh(&mut state,&context,goal.as_ref()).await?; Ok(goal)
    }
}
fn failure(error:crate::errors::GoalError)->ExtensionFailure { ExtensionFailure::new(error.to_string()) }
#[cfg(test)] mod tests {
    use super::*;
    fn assistant_usage(input:u64,output:u64)->maho_agent::types::AgentMessage {
        serde_json::from_value(serde_json::json!({"role":"assistant","content":[],"api":"faux","provider":"faux","model":"faux","usage":{"input":input,"output":output,"cacheRead":0,"cacheWrite":0,"totalTokens":input+output,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0})).unwrap()
    }
    #[tokio::test] async fn owning_agent_end_accounts_then_blocks_terminal_policy_rejection() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||0.0)); let context=crate::test_context::context(); runtime.create(&context,"work").await.unwrap(); runtime.event(&ExtensionEvent::AgentStart,&context).await.unwrap();
        let mut message=assistant_usage(10,5); if let maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(assistant))=&mut message { assistant.api="openai-codex-responses".into(); assistant.stop_reason=maho_ai::types::StopReason::Error; assistant.error_message=Some("Codex error: This request was blocked by our safety systems. Reason: rejected".into()); }
        let goal=runtime.event(&ExtensionEvent::AgentEnd { messages:vec![message],aborted:Some(false),abort_source:None,will_retry:Some(false) },&context).await.unwrap().unwrap();
        assert_eq!(goal.status,GoalStatus::Blocked); assert_eq!(goal.tokens_used,15); assert_eq!(goal.blocked_reason.as_deref(),Some("provider policy rejection ended the turn")); assert_eq!(crate::store::read_goal(&reference).unwrap(),Some(goal)); assert!(!runtime.state.lock().await.ticker.running());
    }
    #[tokio::test] async fn store_change_never_refreshes_a_busy_context() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||0.0));
        let mut context=crate::test_context::context(); context.is_idle_fn=Arc::new(||false);
        runtime.event(&ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent { reason:maho_ext_api::SessionReason::Startup,initial_model_provenance:None,previous_session_file:None }),&context).await.unwrap();
        assert!(runtime.store_changed("s").await.unwrap().is_none());
    }
    #[tokio::test] async fn upstream_streamed_usage_checkpoints_get_complete_and_blocked_tools() {
        for status in ["complete","blocked"] {
            let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
            let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||0.0)); let context=crate::test_context::context();
            runtime.create(&context,"work").await.unwrap(); runtime.event(&ExtensionEvent::AgentStart,&context).await.unwrap();
            let first=assistant_usage(100,50); runtime.event(&ExtensionEvent::MessageEnd { message:first.clone() },&context).await.unwrap();
            assert_eq!(runtime.get(&context).await.unwrap().details.unwrap()["goal"]["tokensUsed"],150.0);
            let params=if status=="blocked" { serde_json::json!({"status":status,"reason":"waiting"}) } else { serde_json::json!({"status":status}) };
            assert_eq!(runtime.update(&context,&params,&[]).await.unwrap().details.unwrap()["goal"]["tokensUsed"],150.0);
            let second=assistant_usage(10,5); runtime.event(&ExtensionEvent::MessageEnd { message:second.clone() },&context).await.unwrap();
            runtime.event(&ExtensionEvent::AgentEnd { messages:vec![first,second],aborted:Some(false),abort_source:None,will_retry:Some(false) },&context).await.unwrap();
            assert_eq!(crate::store::read_goal(&reference).unwrap().unwrap().tokens_used,165);
            assert!(!runtime.state.lock().await.ticker.running());
        }
    }
    #[tokio::test] async fn registered_command_uses_owned_accounting_and_refresh_before_queue() {
        use maho_ext_api::*;
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let runtime=Arc::new(GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||0.0)));
        let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default());
        let observed=Arc::new(Mutex::new(Vec::new())); let captured=observed.clone(); let stored=reference.clone();
        runtime.register_command(&mut api,Arc::new(move |_,goal| { let stored=stored.clone(); let captured=captured.clone(); Box::pin(async move { assert_eq!(crate::store::read_goal(&stored).unwrap().unwrap().status,goal.status); captured.lock().unwrap().push(goal.status); Ok(()) }) }));
        let handler=api.registered.commands[0].handler.clone(); let context=crate::test_context::context();
        handler("work",&context).await.unwrap(); assert!(runtime.state.lock().await.ticker.running());
        handler("pause",&context).await.unwrap(); assert!(!runtime.state.lock().await.ticker.running());
        handler("resume",&context).await.unwrap(); assert!(runtime.state.lock().await.ticker.running());
        handler("clear",&context).await.unwrap(); assert!(!runtime.state.lock().await.ticker.running()); assert!(crate::store::read_goal(&reference).unwrap().is_none());
        assert_eq!(*observed.lock().unwrap(),vec![GoalStatus::Active,GoalStatus::Paused,GoalStatus::Active]);
    }
    #[tokio::test] async fn command_completion_waits_for_continuation_delivery() {
        use maho_ext_api::*;
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let runtime=Arc::new(GoalRuntime::new(Arc::new(move |_|reference.clone()),Arc::new(||0.0)));
        let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default());
        let (entered_tx,entered_rx)=tokio::sync::oneshot::channel(); let entered=Arc::new(Mutex::new(Some(entered_tx)));
        let (release_tx,release_rx)=tokio::sync::oneshot::channel(); let release=Arc::new(Mutex::new(Some(release_rx)));
        runtime.register_command(&mut api,Arc::new(move |_,_| { let entered=entered.lock().unwrap().take().unwrap(); let release=release.lock().unwrap().take().unwrap(); Box::pin(async move { entered.send(()).unwrap(); release.await.unwrap(); Ok(()) }) }));
        let handler=api.registered.commands[0].handler.clone(); let context=crate::test_context::context();
        let command=tokio::spawn(async move { handler("work",&context).await });
        tokio::time::timeout(std::time::Duration::from_secs(2),entered_rx).await.unwrap().unwrap();
        assert!(!command.is_finished()); release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2),command).await.unwrap().unwrap().unwrap();
    }
    #[tokio::test] async fn owned_tools_share_accounting_and_completion_retires_footer() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let clock=Arc::new(std::sync::atomic::AtomicU64::new(0)); let reading=clock.clone();
        let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(move ||reading.load(std::sync::atomic::Ordering::SeqCst) as f64)); let context=crate::test_context::context();
        runtime.event(&ExtensionEvent::AgentStart,&context).await.unwrap();
        runtime.create(&context,"work").await.unwrap(); assert!(runtime.state.lock().await.ticker.running());
        clock.store(1500,std::sync::atomic::Ordering::SeqCst);
        let result=runtime.get(&context).await.unwrap(); assert_eq!(result.details.unwrap()["goal"]["timeUsedSeconds"],2.0);
        assert!(runtime.update(&context,&serde_json::json!({"status":"complete"}),&["open".into()]).await.is_err());
        runtime.update(&context,&serde_json::json!({"status":"complete"}),&[]).await.unwrap(); assert!(!runtime.state.lock().await.ticker.running());
        clock.store(2500,std::sync::atomic::Ordering::SeqCst);
        runtime.event(&ExtensionEvent::AgentEnd { messages:Vec::new(),aborted:Some(false),abort_source:None,will_retry:Some(false) },&context).await.unwrap();
        let goal=crate::store::read_goal(&reference).unwrap().unwrap(); assert_eq!(goal.status,GoalStatus::Complete); assert_eq!(goal.time_used_seconds,3.0);
    }
    #[tokio::test] async fn owned_runtime_accounts_aborts_and_cleans_up_session_state() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let clock=Arc::new(std::sync::atomic::AtomicU64::new(0)); let reading=clock.clone();
        let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(move ||reading.load(std::sync::atomic::Ordering::SeqCst) as f64));
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap(); let context=crate::test_context::context();
        runtime.event(&ExtensionEvent::AgentStart,&context).await.unwrap(); assert!(runtime.state.lock().await.ticker.running());
        clock.store(1500,std::sync::atomic::Ordering::SeqCst);
        let blocked=runtime.event(&ExtensionEvent::SessionAbort,&context).await.unwrap().unwrap(); assert_eq!(blocked.status,GoalStatus::Blocked); assert_eq!(blocked.time_used_seconds,2.0);
        assert!(!runtime.state.lock().await.ticker.running()); assert_eq!(blocked.id,goal.id);
        runtime.event(&ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent { reason:maho_ext_api::SessionReason::Quit,target_session_file:None,signal:None }),&context).await.unwrap();
        assert!(runtime.store_changed("s").await.unwrap().is_none());
    }
}
