//! Compaction policy primitives ported from senpi's builtin compaction extension.
pub mod policy;
pub mod speculation_lead;
pub mod idle;
pub mod idle_retry;
pub mod summarization_retry;
pub mod task_intent;
pub mod token_budget_reminder;
pub mod tool_truncation;
pub mod state;
pub mod circuit_breaker;
pub mod per_turn_cap;
pub mod degradation_monitor;
pub mod repair_tool_pairs;
pub mod summarization_turn_order;
pub mod overflow_retry;
pub mod tool_admission;
pub mod context_reduction;
pub mod lane_policy;
pub mod r#yield;
pub mod openai_remote_schema;
pub mod fallback_failed_turn_normalization;
pub mod orchestration;
pub mod retained_message_safety;
pub mod emergency_prune;
pub mod model_usability_budget;
pub mod switch_admission;
pub mod resume_admission;
pub mod resume_slice;
pub mod restoration_tracker;
pub mod checkpoint_state;
pub mod model_selection;
pub mod extension_wiring;
pub mod openai_remote_model;
pub mod openai_remote_convert;
pub mod transient_failure;
pub mod openai_remote_timeout;
pub mod log;
pub mod prompts;
pub mod openai_remote_responses_v2;
pub mod speculative_summary;
pub mod speculative_job;
pub mod speculative;
pub mod todo_bridge;
pub mod openai_remote_dependencies;
pub mod context_pipeline;
pub mod context_reduction_frontier;
pub mod deterministic_fallback;
pub mod openai_remote;

#[derive(Default)]
pub struct CompactionExtension;

impl maho_ext_api::Extension for CompactionExtension {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let state = std::sync::Arc::new(std::sync::Mutex::new(state::create_initial_state()));
        let restoration = std::sync::Arc::new(std::sync::Mutex::new(restoration_tracker::RestorationTrackerState::default()));
        let pending = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, (serde_json::Value, serde_json::Value))>::new()));
        let live_api = std::sync::Arc::new(maho_ext_api::ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone()));
        let warm = std::sync::Arc::new(std::sync::Mutex::new(None::<speculative_job::LiveSpeculativeJob>));
        let generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let idle_since_end = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reminder = std::sync::Arc::new(std::sync::Mutex::new(token_budget_reminder::create_initial_reminder_state()));
        let degradation = std::sync::Arc::new(tokio::sync::Mutex::new(degradation_monitor::create_degradation_monitor_state()));
        let logger = std::sync::Arc::new(std::sync::Mutex::new(None::<log::CompactionLogger>));
        let lane = std::sync::Arc::new(std::sync::Mutex::new(lane_policy::CompactionLanePolicy::default()));
        {
            let degradation=std::sync::Arc::clone(&degradation);
            let warm=std::sync::Arc::clone(&warm);
            let generation=std::sync::Arc::clone(&generation);
            let live_api=std::sync::Arc::clone(&live_api);
            let lane=std::sync::Arc::clone(&lane);
            let compaction_state=std::sync::Arc::clone(&state);
            api.on(maho_ext_api::EventKind::MessageEnd,std::sync::Arc::new(move |event,context| {
                let compaction_state=std::sync::Arc::clone(&compaction_state);
                let degradation=std::sync::Arc::clone(&degradation);let warm=std::sync::Arc::clone(&warm);let generation=std::sync::Arc::clone(&generation);let live_api=std::sync::Arc::clone(&live_api);let lane=std::sync::Arc::clone(&lane);
                Box::pin(async move {
                    let maho_ext_api::ExtensionEvent::MessageEnd {message}=event else {return Ok(maho_ext_api::EventResult::None);};
                    if message.as_assistant().is_some_and(|assistant|assistant.stop_reason==maho_ai::types::StopReason::Aborted) {
                        generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                        let retired = { warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() };
                        if let Some(job) = retired { job.cancel().await; }
                    }
                    if lane_policy::live_disables_senpi_compaction(&lane,context) {return Ok(maho_ext_api::EventResult::None);}
                    let content:Vec<_>;
                    let raw;
                    let trigger={
                        let mut degradation=degradation.lock().await;
                        // begin_recovery is a no-op outside the post-compaction window; skip serializing the message.
                        if degradation.post_compaction_turns_remaining==0 {return Ok(maho_ext_api::EventResult::None);}
                        raw=serde_json::to_value(message).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        if !extension_wiring::is_monitorable_message(&raw) {return Ok(maho_ext_api::EventResult::None);}
                        content=raw["content"].as_array().into_iter().flatten().map(|part|degradation_monitor::MonitoredMessageContentPart {kind:part["type"].as_str().unwrap_or_default(),text:part["text"].as_str()}).collect();
                        degradation_monitor::begin_recovery(&mut degradation,&degradation_monitor::MonitoredMessageEvent {role:raw["role"].as_str().unwrap_or_default(),content:&content})
                    };
                    // The lock is released before applying: the resulting session_compact handler locks the same mutex.
                    if trigger {
                        let result=match extension_wiring::apply_live_blocking_compaction(&live_api,context,&generation,&warm,&compaction_state,degradation_monitor::RECOVERY_INSTRUCTIONS.into()).await {
                            Ok(result)=>degradation_monitor::RecoveryResult {applied:result==speculative::SpeculativeCompactionResult::Applied,reason:if result==speculative::SpeculativeCompactionResult::Failed {"failed"} else {"rejected"}.into()},
                            Err(_)=>degradation_monitor::RecoveryResult {applied:false,reason:"failed".into()},
                        };
                        if degradation_monitor::should_notify_recovery(&result) {context.ui.notify(degradation_monitor::RECOVERY_NOTIFICATION,maho_ext_api::NotificationType::Warning);}
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
        for kind in [maho_ext_api::EventKind::SessionCompact,maho_ext_api::EventKind::TurnEnd] {
            let degradation=std::sync::Arc::clone(&degradation);
            let compaction_state=std::sync::Arc::clone(&state);
            let live_api=std::sync::Arc::clone(&live_api);
            let generation=std::sync::Arc::clone(&generation);
            let warm=std::sync::Arc::clone(&warm);
            let lane=std::sync::Arc::clone(&lane);
            api.on(kind,std::sync::Arc::new(move |event,context| {
                let warm=std::sync::Arc::clone(&warm);
                let degradation=std::sync::Arc::clone(&degradation);
                let compaction_state=std::sync::Arc::clone(&compaction_state);
                let live_api=std::sync::Arc::clone(&live_api);
                let generation=std::sync::Arc::clone(&generation);
                let lane=std::sync::Arc::clone(&lane);
                Box::pin(async move {
                    let mut state=degradation.lock().await;
                    match event {
                        maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted {..})=>degradation_monitor::reset_on_session_compact(&mut state),
                        maho_ext_api::ExtensionEvent::TurnEnd {..}=>degradation_monitor::handle_turn_end(&mut state),
                        _=>{},
                    }
                    let recover=matches!(event,maho_ext_api::ExtensionEvent::TurnEnd {..})
                        && !lane_policy::live_disables_senpi_compaction(&lane,context)
                        && !state.recovery_triggered_this_cycle
                        && compaction_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last_yield.is_some_and(|last|last.saved_tokens<=0.);
                    drop(state);
                    if recover {
                        let api=std::sync::Arc::clone(&live_api);
                        let context=context.clone();
                        let generation=std::sync::Arc::clone(&generation);
                        let warm=std::sync::Arc::clone(&warm);
                        let compaction_state=std::sync::Arc::clone(&compaction_state);
                        tokio::spawn(async move {
                            let _result=extension_wiring::apply_live_blocking_compaction(&api,&context,&generation,&warm,&compaction_state,degradation_monitor::RECOVERY_INSTRUCTIONS.into()).await;
                        });
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
        for kind in [maho_ext_api::EventKind::SessionShutdown, maho_ext_api::EventKind::SessionCompact] {
            let warm = std::sync::Arc::clone(&warm);
            let generation = std::sync::Arc::clone(&generation);
            let reminder = std::sync::Arc::clone(&reminder);
            let idle = std::sync::Arc::clone(&idle_since_end);
            api.on(kind,std::sync::Arc::new(move |_,_| {
                idle.store(false,std::sync::atomic::Ordering::SeqCst);
                *reminder.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=token_budget_reminder::create_initial_reminder_state();
                generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                let retired = { warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() };
                Box::pin(async move {
                    if let Some(job) = retired { job.cancel().await; }
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
        {
            let warm = std::sync::Arc::clone(&warm);
            let generation = std::sync::Arc::clone(&generation);
            let state = std::sync::Arc::clone(&state);
            let live_api = std::sync::Arc::clone(&live_api);
            let lane = std::sync::Arc::clone(&lane);
            api.on(maho_ext_api::EventKind::ModelSelect,std::sync::Arc::new(move |event,context| {
                let warm=std::sync::Arc::clone(&warm);let generation=std::sync::Arc::clone(&generation);let state=std::sync::Arc::clone(&state);let live_api=std::sync::Arc::clone(&live_api);let lane=std::sync::Arc::clone(&lane);
                Box::pin(async move {
                    let maho_ext_api::ExtensionEvent::ModelSelect(event) = event else {return Ok(maho_ext_api::EventResult::None);};
                    let external = lane_policy::live_disables_senpi_compaction(&lane,context);
                    let mut settings = maho_core::compaction::settings::default_compaction_settings();
                    if !external {
                        settings = extension_wiring::live_settings(context)?;
                    }
                    let (retired,start,epoch) = {
                        let mut job = warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let (mut invalidate,mut start) = (false,false);
                        model_selection::handle_live_compaction_model_select(event,context,&state,job.as_ref().map(|job|&job.snapshot),&settings,external,(||invalidate=true,||start=true))?;
                        let retired = if invalidate {
                            generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                            job.take()
                        } else {None};
                        (retired,start,generation.load(std::sync::atomic::Ordering::SeqCst))
                    };
                    if let Some(job) = retired { job.cancel().await; }
                    if generation.load(std::sync::atomic::Ordering::SeqCst)!=epoch {return Ok(maho_ext_api::EventResult::None);}
                    let mut job = warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if start && job.is_none() {
                        let next = generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
                        *job = speculative_job::start_live_speculative_job(&live_api,context,next,"Proactively compact before the next agent turn.".into())?;
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
        {
            let live_api = std::sync::Arc::clone(&live_api);
            api.on(maho_ext_api::EventKind::MessageEnd, std::sync::Arc::new(move |event, _context| {
                let result = if let maho_ext_api::ExtensionEvent::MessageEnd { message: maho_ext_api::AgentMessage::Llm(message) } = event {
                        lane_policy::collect_compact_boundary_entries(message).into_iter().try_for_each(|entry| {
                            live_api.append_entry(lane_policy::ANTHROPIC_SUBSCRIPTION_COMPACT_ENTRY_TYPE, Some(serde_json::json!({
                                "schema":entry.schema,"sdkSessionId":entry.sdk_session_id,"uuid":entry.uuid,"compactMetadata":entry.compact_metadata,
                            })))
                        })
                } else { Ok(()) };
                Box::pin(async move { result?; Ok(maho_ext_api::EventResult::None) })
            }));
        }
        {
            let latch = std::sync::Arc::new(std::sync::Mutex::new(emergency_prune::EmergencyPruneLatch::default()));
            let reduction_state = std::sync::Arc::new(std::sync::Mutex::new(context_reduction_frontier::ContextReductionState::default()));
            let warm = std::sync::Arc::clone(&warm);
            let generation = std::sync::Arc::clone(&generation);
            let idle_state = std::sync::Arc::clone(&state);
            let live_api = std::sync::Arc::clone(&live_api);
            let idle_since_end = std::sync::Arc::clone(&idle_since_end);
            let idle_lane = std::sync::Arc::clone(&lane);
            api.on(maho_ext_api::EventKind::AgentEnd,std::sync::Arc::new(move |event,context| {
                let warm=std::sync::Arc::clone(&warm);let generation=std::sync::Arc::clone(&generation);let idle_state=std::sync::Arc::clone(&idle_state);let live_api=std::sync::Arc::clone(&live_api);let idle_since_end=std::sync::Arc::clone(&idle_since_end);let idle_lane=std::sync::Arc::clone(&idle_lane);
                Box::pin(async move {
                    let maho_ext_api::ExtensionEvent::AgentEnd {aborted,will_retry,..} = event else {return Ok(maho_ext_api::EventResult::None);};
                    if lane_policy::live_disables_senpi_compaction(&idle_lane,context) || !matches!(context.mode,maho_ext_api::ExtensionMode::Tui | maho_ext_api::ExtensionMode::Rpc) {return Ok(maho_ext_api::EventResult::None);}
                    let settings = extension_wiring::live_settings(context)?;
                    let usage = context.get_context_usage()?;
                    let (last_yield,tripped) = {let state=idle_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);(state.last_yield,circuit_breaker::is_tripped(&state,chrono::Utc::now().timestamp_millis() as f64))};
                    let decision = idle::IdleCompactionDecision {will_retry:will_retry.unwrap_or(false),aborted:aborted.unwrap_or(false),settings:&settings,tokens:usage.as_ref().and_then(|usage|usage.tokens).map(|tokens|tokens as f64),context_window:usage.as_ref().map_or_else(||context.model.as_ref().map_or(200000,|model|model.context_window),|usage|usage.context_window) as f64,breaker_tripped:tripped,last_yield,mode:context.mode};
                    let above_threshold = idle::should_run_idle_compaction(&decision);
                    if !above_threshold && openai_remote_model::is_openai_remote_compaction_model(context.model.as_ref()) {return Ok(maho_ext_api::EventResult::None);}
                    let (retired,action,epoch) = {
                        let mut job = warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let action = orchestration::resolve_idle_warm_action(&decision,job.as_ref().map(|job|job.armed_at_tokens as f64));
                        let retired = if action == orchestration::IdleWarmAction::Replace {
                            generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                            job.take()
                        } else {None};
                        (retired,action,generation.load(std::sync::atomic::Ordering::SeqCst))
                    };
                    if let Some(job) = retired {job.cancel().await;}
                    if generation.load(std::sync::atomic::Ordering::SeqCst)!=epoch {return Ok(maho_ext_api::EventResult::None);}
                    let watching = {
                    let mut job = warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if (above_threshold || action != orchestration::IdleWarmAction::None) && job.is_none() {
                        let next = generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
                        *job = speculative_job::start_live_speculative_job(&live_api,context,next,idle::IDLE_COMPACTION_INSTRUCTIONS.into())?;
                    }
                    job.as_ref().cloned()
                    };
                    if let Some(mut watching) = watching {
                        idle_since_end.store(true,std::sync::atomic::Ordering::SeqCst);
                        let context=context.clone();let warm=std::sync::Arc::clone(&warm);let generation=std::sync::Arc::clone(&generation);let idle=std::sync::Arc::clone(&idle_since_end);let state=std::sync::Arc::clone(&idle_state);let api=std::sync::Arc::clone(&live_api);
                        tokio::spawn(async move {
                            let mut attempt=0;
                            let settled=loop {
                                let settled=watching.settled().await;
                                watching.cancel().await;
                                if settled.error.is_none() {break settled;}
                                if !idle.load(std::sync::atomic::Ordering::SeqCst) || generation.load(std::sync::atomic::Ordering::SeqCst)!=watching.generation {return;}
                                let Ok(Some(usage))=context.get_context_usage() else {return;};
                                let (last_yield,tripped)={let state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);(state.last_yield,circuit_breaker::is_tripped(&state,chrono::Utc::now().timestamp_millis() as f64))};
                                let decision=idle::IdleCompactionDecision {will_retry:false,aborted:false,settings:&watching.snapshot.preparation.settings,tokens:usage.tokens.map(|tokens|tokens as f64),context_window:usage.context_window as f64,breaker_tripped:tripped,last_yield,mode:context.mode};
                                if !idle_retry::should_retry_idle_warmup(&idle_retry::IdleWarmupRetryDecision {attempt,transient:settled.error.as_ref().is_some_and(|error|transient_failure::is_transient_summarization_failure(error.classification,&error.message)),is_idle:context.is_idle(),breaker_tripped:tripped,still_warm_eligible:idle::should_warm_at_idle(&decision)}) {return;}
                                tokio::time::sleep(std::time::Duration::from_millis(idle_retry::IDLE_WARMUP_RETRY_DELAY_MS)).await;
                                if !idle.load(std::sync::atomic::Ordering::SeqCst) || !context.is_idle() || generation.load(std::sync::atomic::Ordering::SeqCst)!=watching.generation {return;}
                                let retired = {
                                    let mut current=warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                    if current.as_ref().is_none_or(|job|job.generation!=watching.generation) {return;}
                                    current.take()
                                };
                                if let Some(job) = retired {job.cancel().await;}
                                if !idle.load(std::sync::atomic::Ordering::SeqCst) || !context.is_idle() || generation.load(std::sync::atomic::Ordering::SeqCst)!=watching.generation {return;}
                                let mut current=warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                if current.is_some() {return;}
                                let next=generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
                                let Ok(Some(replacement))=speculative_job::start_live_speculative_job(&api,&context,next,idle::IDLE_COMPACTION_INSTRUCTIONS.into()) else {return;};
                                watching=replacement.clone();*current=Some(replacement);attempt+=1;
                            };
                            let Some(compaction)=settled.result else {return;};
                            if !idle.load(std::sync::atomic::Ordering::SeqCst) || !context.is_idle() || generation.load(std::sync::atomic::Ordering::SeqCst)!=watching.generation {return;}
                            if warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_none_or(|job|job.generation!=watching.generation) {return;}
                            let (last_yield,tripped)={let state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);(state.last_yield,circuit_breaker::is_tripped(&state,chrono::Utc::now().timestamp_millis() as f64))};
                            let Ok(Some(usage))=context.get_context_usage() else {return;};
                            if tripped || !policy::should_trigger_compaction(usage.tokens.map(|tokens|tokens as f64),usage.context_window as f64,&watching.snapshot.preparation.settings,last_yield) {return;}
                            if matches!(speculative::apply_generated_compaction(&context,Some(&watching.snapshot),generation.load(std::sync::atomic::Ordering::SeqCst),Some(compaction),None).await,Ok(speculative::SpeculativeCompactionResult::Applied)) {
                                let mut job=warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                if job.as_ref().is_some_and(|job|job.generation==watching.generation) {*job=None;}
                            }
                        });
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
            let state = std::sync::Arc::clone(&state);
            let logger = std::sync::Arc::clone(&logger);
            let context_lane = std::sync::Arc::clone(&lane);
            let context_reduction_state = std::sync::Arc::clone(&reduction_state);
            api.on(maho_ext_api::EventKind::Context, std::sync::Arc::new(move |event, context| {
                let context_reduction_state = std::sync::Arc::clone(&context_reduction_state);
                let result = (|| {
                    let maho_ext_api::ExtensionEvent::Context { messages, entry_ids } = event else { return Ok(maho_ext_api::EventResult::None); };
                    let lane_owns_compaction = lane_policy::live_disables_senpi_compaction(&context_lane, context);
                    let append_only_transcript = lane_policy::live_has_append_only_transcript(context);
                    let usage = context.get_context_usage()?;
                    let settings = extension_wiring::live_settings(context)?;
                    let window = usage.as_ref().map_or_else(||context.model.as_ref().map_or(200_000, |model|model.context_window), |usage|usage.context_window);
                    let tokens = usage.and_then(|usage|usage.tokens).map(|tokens|tokens as f64);
                    let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let breaker_fallback = !lane_owns_compaction && circuit_breaker::is_tripped(&state, chrono::Utc::now().timestamp_millis() as f64)
                        && tokens.is_some_and(|tokens|tokens >= window as f64 * policy::compute_effective_threshold(window as f64, state.last_yield));
                    let raw = messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    let provider_native_path = openai_remote_model::is_openai_remote_compaction_model(context.model.as_ref());
                    let mut reduction_state = context_reduction_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    // Only the provider-native replay boundary reads the branch; skip the full branch copy otherwise.
                    let branch_entries = if provider_native_path { speculative::branch_values(context) } else { Vec::new() };
                    let output = context_pipeline::build_compaction_context(context_pipeline::CompactionContextInput {
                        messages: &raw, entry_ids, model: context.model.as_ref(), branch_entries: &branch_entries, context_window: window,
                        prompt_context_window: extension_wiring::get_prompt_context_window(window as f64, context.model.as_ref().map(|model|model.max_tokens as f64)) as u64,
                        usage_tokens: tokens, provider_native_path,
                        tool_admission_enabled: settings.ideal.tool_admission_enabled != Some(false), breaker_fallback, lane_owns_compaction,
                        context_reduction_state: Some(&mut reduction_state), context_overhead_tokens: 0,
                        unreduced_messages: None,
                        reduction_ceiling_tokens: None, reduction_block_budget_ratio: None, append_only_transcript,
                        emergency_prune_latch: &mut latch.lock().unwrap_or_else(std::sync::PoisonError::into_inner), reminder: None,
                        now: chrono::Utc::now().timestamp_millis(),
                    }).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    let mut logger=logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let logger=logger.get_or_insert_with(||log::CompactionLogger::new(Some(&context.agent_dir),None,None));
                    if breaker_fallback {logger.debug("breaker_deterministic_fallback",serde_json::json!({"route":"context-event","tokens":tokens.unwrap_or(0.)}).as_object());}
                    if let Some((before,after))=output.emergency_prune_tokens {logger.debug("emergency_prune",serde_json::json!({"route":"context-event","tokensBefore":before,"tokens":after}).as_object());}
                    let messages = output.messages.into_iter().map(maho_ext_api::AgentMessage::from).collect::<Vec<_>>();
                    Ok(maho_ext_api::EventResult::Context { messages: Some(messages) })
                })();
                Box::pin(async move { result })
            }));
        }
        {
            let restoration = std::sync::Arc::clone(&restoration);
            api.on(maho_ext_api::EventKind::ToolCall, std::sync::Arc::new(move |event, _context| {
                if let maho_ext_api::ExtensionEvent::ToolCall(event) = event {
                    restoration_tracker::track_tool_call(&mut restoration.lock().unwrap_or_else(std::sync::PoisonError::into_inner), &event.tool_name, &event.input);
                }
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }));
        }
        {
            let pending = std::sync::Arc::clone(&pending);
            let live_api = std::sync::Arc::clone(&live_api);
            let state = std::sync::Arc::clone(&state);
            let warm = std::sync::Arc::clone(&warm);
            let generation = std::sync::Arc::clone(&generation);
            let lane = std::sync::Arc::clone(&lane);
            api.on(maho_ext_api::EventKind::SessionBeforeCompact, std::sync::Arc::new(move |event, context| {
                let (claimed,retired) = if let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event {
                    if event.signal.is_aborted() {(None,None)} else {
                        let mut job = warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let claimed = speculative_job::claim_live_warm_job(&mut job,event,context);
                        let retired = job.take();
                        generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                        (claimed,retired)
                    }
                } else {(None,None)};
                let gate = if let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event {
                    let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !event.signal.is_aborted() && !lane_policy::live_owns_compaction(&lane, context, event.reason) {
                        Some(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::ExternalOwner), reason: Some(lane_policy::SDK_NATIVE_LANE_REJECTION_REASON.into()), ..Default::default() })
                    } else if !event.signal.is_aborted() && per_turn_cap::should_reject_by_cap(&state).cancel {
                        Some(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::PerTurnCap), reason: Some("absolute compaction cap reached for this session".into()), ..Default::default() })
                    } else if !event.signal.is_aborted() && circuit_breaker::is_tripped(&state, chrono::Utc::now().timestamp_millis() as f64) && !circuit_breaker::should_bypass(&state, false, Some(event.reason)) {
                        Some(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::CircuitBreaker), ..Default::default() })
                    } else { None }
                } else { None };
                if let Some(gate) = gate {
                    return Box::pin(async move {
                        if let Some(job) = retired {job.cancel().await;}
                        if let Some(job) = claimed {job.cancel().await;}
                        Ok(maho_ext_api::EventResult::SessionBefore(gate))
                    });
                }
                let result = if let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event {
                    if event.signal.is_aborted() { Ok(()) } else {
                        checkpoint_state::capture_live_agent_checkpoint(&live_api, context).map(|checkpoint| {
                            let mut pending = pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            let metadata = (checkpoint, todo_bridge::create_todo_snapshot(context));
                            if let Some((_, value)) = pending.iter_mut().find(|(id, _)| id == &event.request_id) {
                                *value = metadata;
                            } else {
                                pending.push((event.request_id.clone(), metadata));
                            }
                            if pending.len() > 8 { pending.remove(0); }
                        })
                    }
                } else { Ok(()) };
                let live_api = std::sync::Arc::clone(&live_api);
                let pending = std::sync::Arc::clone(&pending);
                Box::pin(async move {
                    if let Some(job) = retired {job.cancel().await;}
                    if let Err(error) = result {
                        if let Some(job) = claimed {job.cancel().await;}
                        return Err(error);
                    }
                    let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { return Ok(maho_ext_api::EventResult::None); };
                    if event.signal.is_aborted() {
                        if let Some(job) = claimed {job.cancel().await;}
                        return Ok(maho_ext_api::EventResult::None);
                    }
                    if let Some(job) = claimed {
                        let settled = tokio::select! {
                            biased;
                            () = event.signal.cancelled() => {job.cancel().await;return Ok(maho_ext_api::EventResult::None);}
                            settled = job.settled() => settled,
                        };
                        job.cancel().await;
                        if let Some(compaction) = settled.result {
                            return Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {compaction:Some(compaction),..Default::default()}));
                        }
                    }
                    let result = extension_wiring::generate_core_route_compaction(&live_api, context, event).await;
                    if !matches!(&result, Ok(maho_ext_api::EventResult::SessionBefore(result)) if result.compaction.is_some()) {
                        pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(id, _)|id != &event.request_id);
                    }
                    result
                })
            }));
        }
        {
            let pending = std::sync::Arc::clone(&pending);
            let live_api = std::sync::Arc::clone(&live_api);
            let restoration = std::sync::Arc::clone(&restoration);
            api.on(maho_ext_api::EventKind::SessionCompact, std::sync::Arc::new(move |event, context| {
                if let maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Rejected { request_id, .. }) = event {
                    pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(id, _)|id != request_id);
                }
                let result = if let maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted { request_id, .. }) = event {
                    let metadata = {
                        let mut pending = pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        pending.iter().position(|(id, _)| id == request_id).map(|index| pending.remove(index).1)
                    };
                    let persisted = if let Some((checkpoint, todos)) = metadata {
                        checkpoint_state::persist_checkpoint(&live_api, &checkpoint)
                            .and_then(|()| live_api.append_entry(todo_bridge::TODO_SNAPSHOT_CUSTOM_TYPE, Some(todos)))
                            .and_then(|()| todo_bridge::restore_todos_if_missing(&live_api, context))
                    } else { Ok(()) };
                    persisted.and_then(|()| {
                        let settings = extension_wiring::live_settings(context)?;
                        let maho_ext_api::ExtensionEvent::SessionCompact(compact) = event else { return Ok(()); };
                        extension_wiring::prepare_accepted_restoration(&mut restoration.lock().unwrap_or_else(std::sync::PoisonError::into_inner), context, compact, &settings)
                    })
                } else { Ok(()) };
                Box::pin(async move { result?; Ok(maho_ext_api::EventResult::None) })
            }));
        }
        {
            let directive = std::sync::Arc::new(std::sync::Mutex::new(checkpoint_state::RestorationDirectiveState::default()));
            let restoration = std::sync::Arc::clone(&restoration);
            let state = std::sync::Arc::clone(&state);
            let live_api = std::sync::Arc::clone(&live_api);
            let warm = std::sync::Arc::clone(&warm);
            let generation = std::sync::Arc::clone(&generation);
            let idle_since_end = std::sync::Arc::clone(&idle_since_end);
            let reminder = std::sync::Arc::clone(&reminder);
            let lane = std::sync::Arc::clone(&lane);
            api.on_with_options(maho_ext_api::EventKind::BeforeAgentStart, std::sync::Arc::new(move |event, context| {
                // senpi: a preview composes a prompt no turn follows, so no compaction, reminder, or restoration.
                if matches!(event, maho_ext_api::ExtensionEvent::BeforeAgentStart(event) if event.preview) {
                    return Box::pin(async { Ok(maho_ext_api::EventResult::None) });
                }
                idle_since_end.store(false,std::sync::atomic::Ordering::SeqCst);
                // PERF-DIVERGENCE(senpi compaction): upstream hands every session entry to the checkpoint lookup; we only materialize `custom` entries (the only ones it inspects) and move the owned strings instead of cloning them. The chosen checkpoint is identical.
                let entries: Vec<_> = context.session_manager.get_entries().into_iter().filter(|entry| entry.kind == "custom").map(|entry| {
                    let mut value = entry.data;
                    value["type"] = serde_json::Value::String(entry.kind);
                    value["id"] = serde_json::Value::String(entry.id);
                    value
                }).collect();
                let checkpoint = extension_wiring::recent_checkpoint(&entries, chrono::Utc::now().timestamp_millis() as f64);
                let pending = restoration_tracker::consume_pending_payload(&mut restoration.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                let message = checkpoint_state::attach_restoration_directive(&mut directive.lock().unwrap_or_else(std::sync::PoisonError::into_inner), checkpoint.as_ref(), pending);
                let result = message.map(|message| Ok(maho_ext_api::CustomMessage {
                    custom_type: message["customType"].as_str().unwrap_or_default().to_owned(),
                    content: vec![maho_ext_api::ToolContent::text(message["content"].as_str().unwrap_or_default())],
                    display: message["display"].as_bool().unwrap_or(false), details: message.get("details").cloned(),
                })).transpose().map(|message| maho_ext_api::EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult { system_prompt_parts: None, message, system_prompt: None }));
                let state = std::sync::Arc::clone(&state);
                let live_api = std::sync::Arc::clone(&live_api);
                let warm = std::sync::Arc::clone(&warm);
                let generation = std::sync::Arc::clone(&generation);
                let reminder = std::sync::Arc::clone(&reminder);
                let lane = std::sync::Arc::clone(&lane);
                Box::pin(async move {
                    if lane_policy::live_disables_senpi_compaction(&lane, context) {return result;}
                    let maho_ext_api::ExtensionEvent::BeforeAgentStart(event) = event else {return result;};
                    let settings = extension_wiring::live_settings(context)?;
                    let usage = context.get_context_usage()?;
                    let window = usage.as_ref().map_or_else(||context.model.as_ref().map_or(200000,|model|model.context_window),|usage|usage.context_window) as f64;
                    let tokens = usage.and_then(|usage|usage.tokens).map(|tokens|tokens as f64);
                    let additional = extension_wiring::estimate_pending_prompt_tokens(Some(&event.prompt),event.images.as_ref().map_or(0,Vec::len)) as f64;
                    let (last_yield,tripped) = {let state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);(state.last_yield,circuit_breaker::is_tripped(&state,chrono::Utc::now().timestamp_millis() as f64))};
                    let geometry = orchestration::resolve_compaction_geometry(window,&settings,last_yield);
                    let hard = policy::is_at_hard_limit(tokens,window,geometry.reserve_tokens,additional);
                    let threshold = !tripped && policy::should_trigger_compaction(tokens.map(|tokens|tokens+additional),window,&settings,last_yield);
                    let in_flight = warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|job|!job.completed());
                    // senpi: the hard-limit valve runs even with compaction disabled; `enabled` gates only the threshold.
                    if hard || (settings.enabled && threshold && !orchestration::should_defer_grace_band(tokens.unwrap_or(0.)+additional,geometry,window,in_flight,settings.ideal.grace_band_enabled)) {
                        let instructions=if hard {"EMERGENCY: hard context limit reached. Produce an aggressive recovery summary that preserves current goal, constraints, files touched, tool outcomes, and exact next steps. Prefer concise factual state over transcript detail."} else {"Proactively compact before the next agent turn."};
                        extension_wiring::apply_live_blocking_compaction(&live_api,context,&generation,&warm,&state,instructions.into()).await?;
                    } else if settings.enabled && !tripped && !openai_remote_model::is_openai_remote_compaction_model(context.model.as_ref()) && policy::should_start_speculative_compaction(tokens.map(|tokens|tokens+additional),window,&settings,last_yield,Some(geometry.lead_tokens)) {
                        let mut job=warm.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if job.is_none() {let next=generation.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;*job=speculative_job::start_live_speculative_job(&live_api,context,next,"Proactively compact before the next agent turn.".into())?;}
                    }
                    let computed = {
                        let mut previous=reminder.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let computed=token_budget_reminder::compute_token_budget_reminder(token_budget_reminder::TokenBudgetReminderInput {context_tokens:tokens.unwrap_or(0.)+additional,context_window:window,threshold_tokens:geometry.threshold_tokens,lead_tokens:geometry.lead_tokens,compaction_epoch:generation.load(std::sync::atomic::Ordering::SeqCst),state:std::mem::take(&mut *previous)});
                        *previous=computed.next_state;computed.message
                    };
                    let mut result=result?;
                    if let maho_ext_api::EventResult::BeforeAgentStart(output)=&mut result {
                        if let Some(message)=&mut output.message {
                            if settings.ideal.reminder_enabled != Some(false) && let Some(reminder)=computed {message.content.push(maho_ext_api::ToolContent::text(format!("\n\n{reminder}")));}
                        } else {output.system_prompt=orchestration::resolve_reminder_system_prompt(&event.system_prompt,computed.as_deref(),settings.ideal.reminder_enabled);}
                    }
                    Ok(result)
                })
            }), maho_ext_api::ExtensionHandlerOptions { preview_safe: true, ..Default::default() });
        }
        for kind in [maho_ext_api::EventKind::TurnEnd, maho_ext_api::EventKind::AgentEnd,
            maho_ext_api::EventKind::SessionCompact] {
            let state = std::sync::Arc::clone(&state);
            let lane = std::sync::Arc::clone(&lane);
            api.on(kind, std::sync::Arc::new(move |event, context| {
                let state = std::sync::Arc::clone(&state);
                let lane = std::sync::Arc::clone(&lane);
                Box::pin(async move {
                    let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let previous = std::mem::take(&mut *state);
                    *state = match event {
                        maho_ext_api::ExtensionEvent::TurnEnd { .. } | maho_ext_api::ExtensionEvent::AgentEnd { .. } => state::reset_turn_counter(previous, ""),
                        maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted { compaction_entry, .. }) => {
                            let mut next = circuit_breaker::record_success(per_turn_cap::increment_accepted(previous));
                            if let Some(yield_data) = compaction_entry.data.get("details").and_then(|details| details.get("structuralYield"))
                                && let (Some(saved_tokens), Some(savings_ratio), Some(tokens_before)) = (
                                    yield_data.get("savedTokens").and_then(serde_json::Value::as_f64),
                                    yield_data.get("savingsRatio").and_then(serde_json::Value::as_f64),
                                    compaction_entry.data.get("tokensBefore").and_then(serde_json::Value::as_f64)) {
                                next.last_yield = Some(policy::CompactionYield { saved_tokens, tokens_before });
                                if r#yield::is_ineffective_compaction(r#yield::StructuralYield { saved_tokens, savings_ratio, tokens_before }) {
                                    next = per_turn_cap::increment_ineffective(next);
                                }
                            }
                            next
                        }
                        maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Rejected { reason, rejection_cause, .. })
                            if *rejection_cause != maho_ext_api::CompactionRejectionCause::ExternalOwner
                                && lane_policy::live_owns_compaction(&lane, context, *reason) => {
                                circuit_breaker::record_failure(previous, chrono::Utc::now().timestamp_millis() as f64, Some(*reason), None)
                            }
                        _ => previous,
                    };
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
    }
}
