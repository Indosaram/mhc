use std::sync::{Arc,Mutex};
use maho_ext_api::{ExtensionContext,ExtensionEvent,ExtensionFailure};
use crate::{accounting_hooks::GoalStoreReference,index::GoalTurnAccounting,types::*,monitor_continuation::MonitorAwareGoalContinuation,direct_input_lifecycle::{GoalDirectInputLifecycle,DirectInputGoalChange}};
struct State {
    context:Option<ExtensionContext>,accounting:GoalTurnAccounting,input:GoalDirectInputLifecycle,
    ticker:crate::elapsed_ticker::GoalElapsedTicker,
    subscriptions:Vec<maho_ext_api::BusSubscription>,
}
pub struct GoalRuntime {
    state:tokio::sync::Mutex<State>,pub monitor:Arc<Mutex<MonitorAwareGoalContinuation>>,
    reference:GoalStoreReference,now:Arc<dyn Fn()->f64+Send+Sync>,
}
impl GoalRuntime {
    pub fn new(reference:GoalStoreReference,now:Arc<dyn Fn()->f64+Send+Sync>)->Self {
        let render=Arc::new(|context:&ExtensionContext,goal:&Goal,elapsed:f64| { crate::ui::update_goal_ui(context,Some(goal),Some(elapsed)); Ok(()) });
        Self { state:tokio::sync::Mutex::new(State { context:None,accounting:Default::default(),input:Default::default(),ticker:crate::elapsed_ticker::GoalElapsedTicker::new(render,now.clone()),subscriptions:Vec::new() }),monitor:Arc::new(Mutex::new(Default::default())),reference,now }
    }
    pub async fn start_channels(&self,events:&maho_ext_api::EventBus,context:&ExtensionContext)->Result<(),ExtensionFailure> {
        let mut state=self.state.lock().await; state.subscriptions.clear();
        let minutes=context.get_ask_user_settings()?.timeout_minutes;
        let question_idle_ms=if minutes.is_finite()&&minutes>0.0 { minutes*60000.0 } else { 1800000.0 };
        let wake_monitor=self.monitor.clone(); let hold_monitor=self.monitor.clone(); let wake_now=self.now.clone(); let hold_now=self.now.clone();
        state.subscriptions=crate::channel_state_subscriptions::subscribe_goal_channel_state(events,Arc::new(move |source,count,event| {
            let deadlines=event.and_then(|data|data.get("items")).and_then(serde_json::Value::as_array).map_or_else(Vec::new,|items|items.iter().filter_map(|item|item.get("deadlineAtMs").and_then(serde_json::Value::as_f64)).collect::<Vec<_>>());
            wake_monitor.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_wake_source_count(source,count,&deadlines,wake_now(),question_idle_ms);
        }),Arc::new(move |source,active| {
            let mut monitor=hold_monitor.lock().unwrap_or_else(std::sync::PoisonError::into_inner); let id=format!("external:{source}");
            if active { monitor.hold_direct_input(&id,hold_now()); } else { monitor.resolve_direct_input(&id,false,hold_now()); }
        }));
        Ok(())
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
                state.accounting.clear(); state.input.reset(); state.subscriptions.clear();
                self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.dispose();
                crate::persistence::migrate_legacy_goal_file_default(&reference).map_err(failure)?;
                goal=crate::store::read_goal(&reference).map_err(failure)?;
                state.context=Some(context.clone());
                if let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Active) { state.accounting.begin(goal,now); }
            },
            ExtensionEvent::AgentStart=>state.accounting.agent_start(goal.as_ref(),now),
            ExtensionEvent::MessageStart { message:maho_agent::types::AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(message)) } if message.custom_type=="manual-continue"=>{
                if goal.as_ref().is_some_and(|goal|goal.status==GoalStatus::Blocked) {
                    goal=Some(crate::store::update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Active),..Default::default() },GoalUpdateSource::User,seconds).await.map_err(failure)?);
                    if let Some(goal)=goal.as_ref() { state.accounting.begin(goal,now); }
                }
            },
            ExtensionEvent::MessageEnd { message }=>{ state.accounting.usage.note_message_end(message); return Ok(goal); },
            ExtensionEvent::AgentEnd { messages,aborted,abort_source,will_retry }=>{
                goal=state.accounting.agent_end(&reference,messages,*aborted==Some(true)&&*abort_source==Some(maho_ext_api::AbortSource::User),now,seconds).await.map_err(failure)?;
                let ended=maho_core::agent_abort_provenance::AgentEndEvent { messages:messages.clone(),aborted:aborted.unwrap_or(false),abort_source:*abort_source,will_retry:will_retry.unwrap_or(false) };
                let route=crate::agent_end_continuation::goal_agent_end_route(goal.as_ref(),&ended);
                if route==crate::agent_end_continuation::GoalAgentEndRoute::AgentEnd {
                    let mut monitor=self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                    monitor.sync_goal(goal.as_ref());
                    monitor.reset_length_recovery_after_clean_stop(goal.as_ref(),messages);
                    if let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Active) {
                        let used_tools=crate::continuation::continuation_turn_used_tools(messages);
                        monitor.record_assistant_output(&crate::lifecycle_helpers::last_assistant_text(messages),used_tools);
                        monitor.record_toolless_continuation_turn(&goal.id,used_tools);
                    }
                }
                if route==crate::agent_end_continuation::GoalAgentEndRoute::ProviderFailure {
                    self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.after_provider_failure(context,goal.as_ref(),&ended);
                }
                if route==crate::agent_end_continuation::GoalAgentEndRoute::SystemAbort {
                    let active_sources=self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.has_active_wake_sources();
                    let (backstop_seconds,question_idle_ms)=if active_sources&&goal.as_ref().is_some_and(|goal|goal.status==GoalStatus::Active)&&!ended.will_retry {
                        let minutes=context.get_ask_user_settings()?.timeout_minutes;
                        (context.get_prompt_cache_goal_backstop_max_seconds()?,if minutes.is_finite()&&minutes>0.0 { minutes*60000.0 } else { 1800000.0 })
                    } else { (0.0,0.0) };
                    self.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.after_system_abort(context,goal.as_ref(),&ended,now,backstop_seconds,question_idle_ms);
                }
                if route==crate::agent_end_continuation::GoalAgentEndRoute::PolicyBlock {
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
                state.accounting.clear(); state.context=None; state.subscriptions.clear();
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
    pub async fn maybe_prompt_resume_stopped_goal(&self,context:&ExtensionContext,reason:&str,goal:Option<&Goal>,queue:&crate::command_registration::QueueGoalContinuation)->Result<bool,ExtensionFailure> {
        if !crate::lifecycle_helpers::is_resume_of_stopped_goal(context,reason,goal)? { return Ok(false); }
        let Some(goal)=goal else { return Ok(false); };
        if context.mode==maho_ext_api::ExtensionMode::Rpc {
            context.ui.notify(&format!("Goal remains {} after session resume. Resume it explicitly after the session finishes loading.",goal.status),maho_ext_api::NotificationType::Info); return Ok(true);
        }
        let choices=["Resume goal".into(),"Leave stopped".into()];
        if context.ui.select(&format!("Resume {} goal?\nGoal: {}",goal.status,goal.objective),&choices,Default::default()).await.as_deref()!=Some("Resume goal") { return Ok(true); }
        let resumed=crate::store::update_goal(&(self.reference)(context),&GoalUpdate { status:Some(GoalStatus::Active),..Default::default() },GoalUpdateSource::User,((self.now)()/1000.0).floor() as u64).await.map_err(failure)?;
        { let mut state=self.state.lock().await; state.accounting.begin(&resumed,(self.now)()); self.refresh(&mut state,context,Some(&resumed)).await?; }
        context.ui.notify(&format!("Goal {}\n{}",crate::format::goal_status_label(resumed.status),crate::format::format_goal_for_tool(Some(&resumed)).map_err(failure)?),maho_ext_api::NotificationType::Info);
        queue(context,&resumed).await?; Ok(true)
    }
}
fn failure(error:crate::errors::GoalError)->ExtensionFailure { ExtensionFailure::new(error.to_string()) }
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn owning_channels_replace_subscriptions_and_shutdown_unsubscribes() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||2000.0)); let mut context=crate::test_context::context(); let session=crate::test_context::bind_session(&mut context);
        let first=maho_ext_api::EventBus::default(); let second=maho_ext_api::EventBus::default();
        runtime.monitor.lock().unwrap().sync_goal(Some(&goal)); runtime.start_channels(&first,&context).await.unwrap();
        first.emit("wake_source_state",&serde_json::json!({"source":"ask-user","activeCount":1,"items":[{"deadlineAtMs":5000}]}));
        assert_eq!(runtime.monitor.lock().unwrap().ask_user_deadline_at_ms,Some(5000.0));
        runtime.monitor.lock().unwrap().arm_timer(crate::wait_progress::GoalWaitKind::Monitor,10000.0,10000.0,false,2000.0);
        first.emit("continuation_hold_state",&serde_json::json!({"source":"guard","active":true})); assert!(runtime.monitor.lock().unwrap().held_timer.is_some());
        first.emit("continuation_hold_state",&serde_json::json!({"source":"guard","active":false})); assert!(runtime.monitor.lock().unwrap().armed_timer.is_some());
        runtime.start_channels(&second,&context).await.unwrap(); first.emit("wake_source_state",&serde_json::json!({"source":"old","activeCount":1})); assert!(!runtime.monitor.lock().unwrap().wake_sources.contains_key("old"));
        second.emit("wake_source_state",&serde_json::json!({"source":"ask-user","activeCount":0})); assert!(runtime.monitor.lock().unwrap().armed_timer.unwrap().drain_fire);
        runtime.event(&ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent { reason:maho_ext_api::SessionReason::Quit,target_session_file:None,signal:None }),&context).await.unwrap();
        second.emit("wake_source_state",&serde_json::json!({"source":"task","activeCount":1})); assert!(runtime.monitor.lock().unwrap().wake_sources.is_empty()); session.dispose().await;
    }
    #[tokio::test] async fn owning_system_abort_stages_only_error_without_live_sources_or_retry() {
        for (stop,will_retry,staged) in [(maho_ai::types::StopReason::Error,false,true),(maho_ai::types::StopReason::Aborted,false,false),(maho_ai::types::StopReason::Error,true,false)] {
            let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
            let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
            let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||2000.0)); let context=crate::test_context::context();
            let mut assistant=maho_ai::providers::faux::faux_assistant_message("",Default::default()); assistant.stop_reason=stop;
            let message=maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(assistant)));
            runtime.event(&ExtensionEvent::AgentEnd { messages:vec![message],aborted:Some(true),abort_source:Some(maho_ext_api::AbortSource::System),will_retry:Some(will_retry) },&context).await.unwrap();
            let mut monitor=runtime.monitor.lock().unwrap(); let pending=monitor.take_settled_recovery(); assert_eq!(pending.is_some(),staged);
            if let Some((path,pending))=pending { assert_eq!(path,crate::continuation::GoalContinuationPath::SystemRecovery); assert_eq!(pending.goal.id,goal.id); }
            assert!(monitor.take_settled_recovery().is_none()); assert!(monitor.recent_normalized_output_hashes.is_empty());
        }
    }
    #[tokio::test] async fn owning_system_abort_uses_bound_backstop_settings_for_live_channels() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||2000.0)); let mut context=crate::test_context::context(); let session=crate::test_context::bind_session(&mut context);
        { let mut monitor=runtime.monitor.lock().unwrap(); monitor.sync_goal(Some(&goal)); monitor.set_wake_source_count("task",1.0,&[],2000.0,1800000.0); }
        runtime.event(&ExtensionEvent::AgentEnd { messages:vec![],aborted:Some(true),abort_source:Some(maho_ext_api::AbortSource::System),will_retry:Some(false) },&context).await.unwrap();
        { let monitor=runtime.monitor.lock().unwrap(); let timer=monitor.armed_timer.unwrap(); assert_eq!(timer.due_at_ms,2000.0+crate::cache_warm::resolve_goal_monitor_continuation_delay_ms(Some(context.get_prompt_cache_goal_backstop_max_seconds().unwrap()))); }
        runtime.event(&ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent { reason:maho_ext_api::SessionReason::Quit,target_session_file:None,signal:None }),&context).await.unwrap(); session.dispose().await;
    }
    #[tokio::test] async fn provider_failure_stages_runtime_recovery_only_without_core_retry() {
        for will_retry in [false,true] {
            let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
            let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
            let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||2000.0)); let context=crate::test_context::context();
            let mut assistant=maho_ai::providers::faux::faux_assistant_message("",Default::default());
            assistant.stop_reason=maho_ai::types::StopReason::Error; assistant.error_message=Some("provider transport unavailable".into());
            let message=maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(assistant)));
            runtime.event(&ExtensionEvent::AgentEnd { messages:vec![message],aborted:Some(false),abort_source:None,will_retry:Some(will_retry) },&context).await.unwrap();
            let mut monitor=runtime.monitor.lock().unwrap(); let pending=monitor.take_settled_recovery();
            assert_eq!(pending.is_some(),!will_retry);
            if let Some((path,pending))=pending { assert_eq!(path,crate::continuation::GoalContinuationPath::ProviderRecovery); assert_eq!(pending.goal.id,goal.id); }
            assert!(monitor.take_settled_recovery().is_none()); assert!(monitor.recent_normalized_output_hashes.is_empty()); assert_eq!(monitor.toolless_continuation_streak,u64::from(will_retry));
        }
    }
    #[tokio::test] async fn agent_end_clean_stop_clears_runtime_length_recovery() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||2000.0));
        let context=crate::test_context::context();
        { let mut monitor=runtime.monitor.lock().unwrap(); monitor.sync_goal(Some(&goal)); monitor.consecutive_length_recoveries.insert(goal.id.clone(),1); }
        let message=maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(maho_ai::providers::faux::faux_assistant_message("done",Default::default()))));
        runtime.event(&ExtensionEvent::AgentEnd { messages:vec![message],aborted:Some(false),abort_source:None,will_retry:Some(false) },&context).await.unwrap();
        let monitor=runtime.monitor.lock().unwrap();
        assert!(!monitor.consecutive_length_recoveries.contains_key(&goal.id));
        assert_eq!(monitor.recent_normalized_output_hashes,vec![crate::continuation::hash_assistant_text("done")]);
        assert_eq!(monitor.toolless_continuation_streak,1);
    }
    #[tokio::test] async fn registered_replacement_confirmation_cancels_without_accounting_or_delivery() {
        use maho_ext_api::*;
        for accepted in [false,true] {
            let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone(); let original=crate::store::create_goal(&reference,"Original",None,0).await.unwrap();
            let runtime=Arc::new(GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||2000.0))); let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default());
            let delivered=Arc::new(std::sync::atomic::AtomicBool::new(false)); let capture=delivered.clone(); runtime.register_command(&mut api,Arc::new(move |_,_| { capture.store(true,std::sync::atomic::Ordering::SeqCst); Box::pin(async { Ok(()) }) }));
            let ui=Arc::new(crate::test_context::Ui::default()); *ui.selection.lock().unwrap()=Some(if accepted { "Replace current goal" } else { "Cancel" }.into()); let mut context=crate::test_context::context(); context.ui=ui.clone();
            api.registered.commands[0].handler.as_ref()("Replacement",&context).await.unwrap(); assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst),accepted); assert_eq!(ui.choices.lock().unwrap()[0],["Replace current goal","Cancel"]);
            let current=crate::store::read_goal(&reference).unwrap().unwrap(); if accepted { assert_ne!(current.id,original.id); assert_eq!(current.objective,"Replacement"); } else { assert_eq!(current,original); }
            if let Some(worker)=runtime.state.lock().await.ticker.stop().unwrap() { match worker.await { Ok(result)=>result.unwrap(),Err(error)=>assert!(error.is_cancelled()) } }
        }
    }
    #[tokio::test] async fn stopped_resume_confirmation_preserves_rpc_and_declined_goals() {
        for (rpc,choice,resumed) in [(true,Some("Resume goal"),false),(false,None,false),(false,Some("Leave stopped"),false),(false,Some("Resume goal"),true)] {
            let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
            let original=crate::store::create_goal(&reference,"work",None,0).await.unwrap(); let paused=crate::store::update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Paused),..Default::default() },GoalUpdateSource::User,1).await.unwrap();
            let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||2000.0)); let ui=Arc::new(crate::test_context::Ui::default()); *ui.selection.lock().unwrap()=choice.map(str::to_owned); let mut context=crate::test_context::context(); context.ui=ui.clone(); if rpc { context.mode=maho_ext_api::ExtensionMode::Rpc; }
            let session=crate::test_context::bind_session(&mut context);
            let delivered=Arc::new(std::sync::atomic::AtomicBool::new(false)); let capture=delivered.clone(); let queue:crate::command_registration::QueueGoalContinuation=Arc::new(move |_,goal| { assert_eq!(goal.status,GoalStatus::Active); capture.store(true,std::sync::atomic::Ordering::SeqCst); Box::pin(async { Ok(()) }) });
            assert!(runtime.maybe_prompt_resume_stopped_goal(&context,"resume",Some(&paused),&queue).await.unwrap()); assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst),resumed);
            let current=crate::store::read_goal(&reference).unwrap().unwrap(); assert_eq!(current.id,original.id); assert_eq!(current.status,if resumed { GoalStatus::Active } else { GoalStatus::Paused });
            assert_eq!(ui.choices.lock().unwrap().len(),usize::from(!rpc)); if !rpc { assert_eq!(ui.choices.lock().unwrap()[0],["Resume goal","Leave stopped"]); }
            if let Some(worker)=runtime.state.lock().await.ticker.stop().unwrap() { match worker.await { Ok(result)=>result.unwrap(),Err(error)=>assert!(error.is_cancelled()) } }
            session.dispose().await;
        }
    }
    fn assistant_usage(input:u64,output:u64)->maho_agent::types::AgentMessage {
        serde_json::from_value(serde_json::json!({"role":"assistant","content":[],"api":"faux","provider":"faux","model":"faux","usage":{"input":input,"output":output,"cacheRead":0,"cacheWrite":0,"totalTokens":input+output,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0})).unwrap()
    }
    #[tokio::test] async fn manual_continue_resumes_only_blocked_goals_and_preserves_identity() {
        for status in [GoalStatus::Blocked,GoalStatus::Paused,GoalStatus::Complete] {
            let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
            let initial=crate::store::create_goal(&reference,"work",None,0).await.unwrap(); crate::store::update_goal(&reference,&GoalUpdate { status:Some(status),reason:(status==GoalStatus::Blocked).then(||"user interrupted the turn".into()),..Default::default() },if status==GoalStatus::Paused { GoalUpdateSource::User } else { GoalUpdateSource::Model },1).await.unwrap();
            let runtime=GoalRuntime::new(Arc::new(move |_|stored.clone()),Arc::new(||2000.0)); let context=crate::test_context::context();
            let message=maho_agent::types::AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(maho_agent::harness::messages::CustomMessage { role:"custom".into(),custom_type:"manual-continue".into(),content:maho_agent::harness::messages::CustomMessageContent::Text("continue".into()),display:false,details:None,timestamp:0 }));
            let goal=runtime.event(&ExtensionEvent::MessageStart { message },&context).await.unwrap().unwrap(); assert_eq!(goal.id,initial.id); assert_eq!(goal.status,if status==GoalStatus::Blocked { GoalStatus::Active } else { status });
        }
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
