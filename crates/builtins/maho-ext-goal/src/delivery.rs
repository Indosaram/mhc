use std::sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}};
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionEvent,ExtensionFailure};
use crate::{types::{Goal,GoalStatus},continuation::*,runtime::GoalRuntime};

pub struct GoalDelivery {
    runtime:Arc<GoalRuntime>,reference:crate::accounting_hooks::GoalStoreReference,
    now:Arc<dyn Fn()->f64+Send+Sync>,api:Arc<ExtensionApi>,pending:AtomicBool,
    admission:tokio::sync::Mutex<()>,
    timer:tokio::sync::Mutex<crate::monitor_timer::GoalContinuationTimer>,
    wait:tokio::sync::Mutex<crate::wait_ticker::GoalWaitTicker>,
    context:Mutex<Option<ExtensionContext>>,subscriptions:Mutex<Vec<maho_ext_api::BusSubscription>>,
}
impl GoalDelivery {
    pub fn new(runtime:Arc<GoalRuntime>,reference:crate::accounting_hooks::GoalStoreReference,now:Arc<dyn Fn()->f64+Send+Sync>,api:&ExtensionApi)->Self {
        Self { runtime,reference,now:now.clone(),api:Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone())),pending:AtomicBool::new(false),admission:Default::default(),timer:Default::default(),wait:tokio::sync::Mutex::new(crate::wait_ticker::GoalWaitTicker::new(Arc::new(|ctx,status| { if ctx.has_ui { ctx.ui.set_status(crate::wait_ticker::GOAL_WAIT_STATUS_KEY,status); } Ok(()) }),now)),context:Mutex::new(None),subscriptions:Mutex::new(Vec::new()) }
    }
    pub fn queue_callback(self:&Arc<Self>)->crate::command_registration::QueueGoalContinuation {
        let owner=self.clone(); Arc::new(move |ctx,goal| { let owner=owner.clone(); Box::pin(async move { owner.queue(ctx,goal,GoalContinuationPath::SessionStart).await }) })
    }
    async fn queue(&self,ctx:&ExtensionContext,goal:&Goal,path:GoalContinuationPath)->Result<(),ExtensionFailure> {
        let _admission=self.admission.lock().await;
        let current=crate::store::read_goal(&(self.reference)(ctx)).map_err(failure)?;
        let Some(goal)=current.as_ref().filter(|current|current.id==goal.id&&current.status==GoalStatus::Active) else { return Ok(()); };
        let branch=ctx.session_manager.get_branch();
        let todos=crate::todo_gate::branch_todos(&branch);
        let open=todos.iter().filter(|todo|maho_ext_todotools::todo_format::is_incomplete_todo(todo)).count();
        let assistant=crate::lifecycle_helpers::last_assistant_from_entries(&branch);
        let signature=build_goal_continuation_signature(goal,open,todos.len(),&hash_assistant_text(&crate::lifecycle_helpers::last_assistant_text_from_entries(&branch)));
        let (hashes,streak,lengths)={ let monitor=self.runtime.monitor.lock().map_err(failure)?; (monitor.recent_normalized_output_hashes.clone(),monitor.toolless_continuation_streak,monitor.consecutive_length_recoveries.get(&goal.id).copied().unwrap_or(0)) };
        let input=GoalContinuationInput { goal:Some(goal),is_idle:ctx.is_idle(),has_pending_messages:ctx.has_pending_messages()?,path,last_stop_reason:assistant.as_ref().map(|message|message.stop_reason),last_turn_was_malformed_tool_use:assistant.as_ref().is_some_and(is_malformed_tool_use_turn),consecutive_continuations:goal.consecutive_continuations.unwrap_or(0),last_continuation_signature:goal.last_continuation_signature.as_deref(),current_signature:Some(&signature),consecutive_length_recoveries:lengths,recent_normalized_output_hashes:&hashes,toolless_continuation_streak:streak,continuation_pending:self.pending.load(Ordering::Acquire),last_turn_stuck_on_context_overflow:crate::lifecycle_helpers::is_last_turn_stuck_on_context_overflow(ctx,assistant.as_ref()) };
        let (recorded,verdict)=crate::lifecycle_helpers::admit_and_report_goal_continuation(&self.api,ctx,&(self.reference)(ctx),&input,((self.now)()/1000.0).floor() as u64).await?;
        if let (Some(recorded),Ok(verdict))=(recorded.as_ref(),crate::monitor_continuation_types::ContinuingGoalContinuationVerdict::try_from(verdict)) {
            self.pending.store(true,Ordering::Release);
            let content={ let mut monitor=self.runtime.monitor.lock().map_err(failure)?;
                if verdict.prompt==ContinuationPrompt::Minimal { *monitor.consecutive_length_recoveries.entry(goal.id.clone()).or_default()+=1; }
                monitor.build_continuation_content(&self.api,ctx,recorded,verdict)
            };
            if let Err(error)=crate::lifecycle_helpers::queue_hidden_goal_prompt(&self.api,content) { self.pending.store(false,Ordering::Release); return Err(error); }
        }
        self.runtime.sync_continuation_goal(ctx,recorded.as_ref()).await
    }
    async fn backstop(&self,ctx:&ExtensionContext,goal:&Goal,parked:Option<&crate::parked_wait::ParkedGoalWait>)->Result<(),ExtensionFailure> {
        let seconds=ctx.get_prompt_cache_goal_backstop_max_seconds()?;
        let minutes=ctx.get_ask_user_settings()?.timeout_minutes;
        let question=if minutes.is_finite()&&minutes>0.0 { minutes*60000.0 } else { 1800000.0 };
        let usage=crate::lifecycle_helpers::last_assistant_from_entries(&ctx.session_manager.get_branch()).map(|message|(message.usage.cache_read as f64,message.usage.cache_write as f64));
        let cache=crate::cache_warm::estimate_cache_warm_metrics(ctx.model.as_ref(),&std::env::vars().collect(),usage);
        let schedule=self.runtime.monitor.lock().map_err(failure)?.rearm_monitor_backstop(goal,parked,(self.now)(),seconds,question,cache);
        if let Some(schedule)=schedule { crate::monitor_continuation::publish_monitor_schedule(&self.api,&schedule,parked.is_none())?; }
        Ok(())
    }
    pub async fn reconcile(self:&Arc<Self>,ctx:&ExtensionContext)->Result<(),ExtensionFailure> {
        let weak=Arc::downgrade(self); let context=ctx.clone();
        self.timer.lock().await.sync_monitor(self.runtime.monitor.clone(),self.now.clone(),ctx.clone(),Arc::new(move |due| {
            let owner=weak.upgrade(); let ctx=context.clone();
            Box::pin(async move {
                if let Some(owner)=owner {
                    let sources=owner.runtime.monitor.lock().map_err(failure)?.wake_sources.clone();
                    crate::monitor_continuation::publish_monitor_resume(&owner.api,&due,&sources)?;
                    owner.queue(&ctx,&due.goal,due.path).await?;
                    owner.wait.lock().await.stop().await?;
                }
                Ok(())
            })
        })).await?;
        let (timer,counts)={ let monitor=self.runtime.monitor.lock().map_err(failure)?; (monitor.armed_timer,monitor.wake_sources.clone()) };
        let mut wait=self.wait.lock().await;
        if let Some(timer)=timer.filter(|_|ctx.has_ui) {
            wait.sync(ctx.clone(),crate::wait_progress::GoalWaitLabelInput { kind:timer.kind,remaining_ms:(timer.due_at_ms-(self.now)()).max(0.0),total_ms:timer.total_ms,channel_counts:counts }).await?;
        } else { wait.stop().await?; }
        Ok(())
    }
    fn subscribe(self:&Arc<Self>,ctx:&ExtensionContext) {
        *self.context.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(ctx.clone());
        let executor=tokio::runtime::Handle::current(); let mut subscriptions=self.subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner); subscriptions.clear();
        for event in ["terminal_monitor_state","wake_source_state","continuation_hold_state",crate::store_changed_event::GOAL_STORE_CHANGED_EVENT] {
            let owner=Arc::downgrade(self); let executor=executor.clone();
            subscriptions.push(self.api.events.on(event,Arc::new(move |data| {
                let Some(owner)=owner.upgrade() else { return; };
                let context=owner.context.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                let data=data.clone();
                executor.spawn(async move {
                    let Some(ctx)=context else { return; };
                    if owner.context.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_none_or(|active|active.session_manager.session_id()!=ctx.session_manager.session_id()) { return; }
                    let result=async {
                        if event==crate::store_changed_event::GOAL_STORE_CHANGED_EVENT&&crate::store_changed_event::is_goal_store_changed_event(&data) && let Some(goal)=owner.runtime.store_changed_with_context(&crate::store_changed_event::GoalStoreChangedEvent { thread_id:data["threadId"].as_str().unwrap_or("").into(),ctx:Some(ctx.clone()) }).await? { owner.queue(&ctx,&goal,GoalContinuationPath::SessionStart).await?; }
                        owner.reconcile(&ctx).await
                    }.await;
                    if let Err(error)=result && !crate::stale_context::is_stale_extension_context_error(&error) { ctx.ui.notify(&error.message,maho_ext_api::NotificationType::Error); }
                });
            })));
        }
    }
    pub async fn event(self:&Arc<Self>,event:&ExtensionEvent,ctx:&ExtensionContext,goal:Option<&Goal>)->Result<(),ExtensionFailure> {
        match event {
            ExtensionEvent::SessionStart(start)=>{
                self.pending.store(false,Ordering::Release); self.subscribe(ctx);
                if let Some(goal)=goal {
                    let reason=if start.reason==maho_ext_api::SessionReason::Resume { "resume" } else { "startup" };
                    if !self.runtime.maybe_prompt_resume_stopped_goal(ctx,reason,Some(goal),&self.queue_callback()).await? {
                        let active=self.runtime.monitor.lock().map_err(failure)?.has_active_wake_sources();
                        if start.reason==maho_ext_api::SessionReason::Reload&&active { let parked=crate::parked_wait::find_parked_goal_wait(&ctx.session_manager.get_branch(),&goal.id); self.backstop(ctx,goal,parked.as_ref()).await?; }
                        else if crate::index::count_trailing_goal_continuation_entries(&ctx.session_manager.get_branch())>=usize::try_from(GOAL_CONTINUATION_CAP).map_err(failure)? {
                            let count=crate::index::count_trailing_goal_continuation_entries(&ctx.session_manager.get_branch());
                            self.runtime.arm_suppressed_load_resume().await; ctx.ui.notify(&format!("Goal auto-continuation suppressed for this resumed session ({count} historical continuations). Send a message to resume."),maho_ext_api::NotificationType::Info);
                        } else { self.queue(ctx,goal,GoalContinuationPath::SessionStart).await?; }
                    }
                }
            },
            ExtensionEvent::AgentStart if self.pending.swap(false,Ordering::AcqRel) => { self.runtime.monitor.lock().map_err(failure)?.note_continuation_started(); },
            ExtensionEvent::AgentEnd { messages,aborted,abort_source,will_retry }=>{
                let ended=maho_core::agent_abort_provenance::AgentEndEvent { messages:messages.clone(),aborted:aborted.unwrap_or(false),abort_source:*abort_source,will_retry:will_retry.unwrap_or(false) };
                if crate::agent_end_continuation::goal_agent_end_route(goal,&ended)==crate::agent_end_continuation::GoalAgentEndRoute::AgentEnd&&should_queue_goal_continuation_after_agent_end(goal,ctx.has_pending_messages()?,messages)&&let Some(goal)=goal.filter(|goal|goal.status==GoalStatus::Active) {
                    let (sources,user)={ let monitor=self.runtime.monitor.lock().map_err(failure)?; (monitor.has_active_wake_sources(),monitor.ended_turn_was_user_initiated) };
                    let branch=ctx.session_manager.get_branch();
                    let todos=crate::todo_gate::branch_todos(&branch);
                    let open=todos.iter().filter(|todo|maho_ext_todotools::todo_format::is_incomplete_todo(todo)).count();
                    let signature=build_goal_continuation_signature(goal,open,todos.len(),&hash_assistant_text(&crate::lifecycle_helpers::last_assistant_text(messages)));
                    let reset=!user&&(continuation_turn_used_tools(messages)||goal.last_continuation_signature.as_ref().is_some_and(|previous|previous!=&signature));
                    let refreshed=if reset { crate::store::reset_continuation_streak(&(self.reference)(ctx),false).await.map_err(failure)? } else { None };
                    let goal=refreshed.as_ref().unwrap_or(goal);
                    if user { let mut monitor=self.runtime.monitor.lock().map_err(failure)?; monitor.ended_turn_was_user_initiated=false; monitor.arm_timer(crate::wait_progress::GoalWaitKind::UserGrace,10000.0,10000.0,false,(self.now)()); }
                    else if sources { self.backstop(ctx,goal,None).await?; }
                    else { self.queue(ctx,goal,GoalContinuationPath::Immediate).await?; }
                }
            },
            ExtensionEvent::AgentSettled=>{
                let recovery=self.runtime.monitor.lock().map_err(failure)?.take_settled_recovery();
                if let Some((path,recovery))=recovery { self.queue(ctx,&recovery.goal,path).await?; }
            },
            ExtensionEvent::InputDisposition { disposition: maho_ext_api::InputDisposition::Started|maho_ext_api::InputDisposition::Queued,.. }=>{ self.pending.store(false,Ordering::Release); },
            ExtensionEvent::SessionAbort=>{ self.pending.store(false,Ordering::Release); },
            ExtensionEvent::SessionShutdown(_)=>{
                self.pending.store(false,Ordering::Release); self.subscriptions.lock().map_err(failure)?.clear(); *self.context.lock().map_err(failure)?=None;
                self.timer.lock().await.cancel().await?; self.wait.lock().await.stop().await?; return Ok(());
            },_=>{},
        }
        self.reconcile(ctx).await
    }
}
fn failure(error:impl std::fmt::Display)->ExtensionFailure { ExtensionFailure::new(error.to_string()) }
#[cfg(test)] mod tests {
    use super::*;
    struct Capture(Mutex<Vec<maho_ext_api::CustomMessage>>);
    impl maho_ext_api::ExtensionActions for Capture {
        fn send_message(&self,message:maho_ext_api::CustomMessage,_:maho_ext_api::SendMessageOptions)->Result<(),ExtensionFailure> { self.0.lock().unwrap().push(message); Ok(()) }
        fn send_user_message(&self,_:maho_ext_api::UserMessageContent,_:maho_ext_api::SendUserMessageOptions)->Result<(),ExtensionFailure> { panic!("unexpected user message") }
        fn append_entry(&self,_:&str,_:Option<serde_json::Value>)->Result<(),ExtensionFailure> { Ok(()) }
        fn get_all_tools(&self)->Result<Vec<maho_ext_api::ToolInfo>,ExtensionFailure> { Ok(Vec::new()) }
    }
    #[tokio::test] async fn concurrent_admission_delivers_once_and_records_before_followup() {
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        let resolver:crate::accounting_hooks::GoalStoreReference=Arc::new(move |_|stored.clone());
        let runtime=Arc::new(GoalRuntime::new(resolver.clone(),Arc::new(||0.0)));
        let capture=Arc::new(Capture(Mutex::new(Vec::new()))); let transport=maho_ext_api::ExtensionRuntime::default(); transport.bind(capture.clone());
        let api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),transport);
        let owner=Arc::new(GoalDelivery::new(runtime.clone(),resolver,Arc::new(||0.0),&api));
        let mut ctx=crate::test_context::context(); let session=crate::test_context::bind_session(&mut ctx); ctx.has_ui=false;
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let (first,second)=tokio::join!(owner.queue(&ctx,&goal,GoalContinuationPath::SessionStart),owner.queue(&ctx,&goal,GoalContinuationPath::SessionStart)); first.unwrap(); second.unwrap();
        assert_eq!(capture.0.lock().unwrap().len(),1);
        assert_eq!(crate::store::read_goal(&reference).unwrap().unwrap().consecutive_continuations,Some(1));
        assert!(!capture.0.lock().unwrap()[0].display);
        runtime.event(&ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent { reason:maho_ext_api::SessionReason::Quit,target_session_file:None,signal:None }),&ctx).await.unwrap(); session.dispose().await;
    }
}
