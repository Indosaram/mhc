//! Compaction policy primitives ported from senpi's builtin compaction extension.
pub mod checkpoint_state;
pub mod circuit_breaker;
pub mod context_pipeline;
pub mod context_reduction;
pub mod context_reduction_frontier;
pub mod context_reduction_handoff;
pub mod context_reduction_lifecycle;
pub mod context_reduction_state;
pub mod degradation_monitor;
pub mod deterministic_fallback;
pub mod emergency_prune;
pub mod extension_wiring;
pub mod fallback_failed_turn_normalization;
pub mod idle;
pub mod idle_retry;
pub mod lane_policy;
pub mod log;
pub mod model_selection;
pub mod model_usability_budget;
pub mod openai_remote;
pub mod openai_remote_convert;
pub mod openai_remote_dependencies;
pub mod openai_remote_model;
pub mod openai_remote_responses_v2;
pub mod openai_remote_schema;
pub mod openai_remote_timeout;
pub mod orchestration;
pub mod overflow_retry;
pub mod per_turn_cap;
pub mod policy;
pub mod prompts;
pub mod rejected_recovery;
pub mod repair_tool_pairs;
pub mod restoration_tracker;
pub mod resume_admission;
pub mod resume_slice;
pub mod retained_message_safety;
pub mod speculation_lead;
pub mod speculative;
pub mod speculative_job;
pub mod speculative_summary;
pub mod state;
pub mod summarization_retry;
pub mod summarization_turn_order;
pub mod switch_admission;
pub mod task_intent;
pub mod todo_bridge;
pub mod token_budget_reminder;
pub mod tool_admission;
pub mod tool_truncation;
pub mod transient_failure;
pub mod r#yield;

const EMERGENCY_COMPACTION_INSTRUCTIONS: &str = "EMERGENCY: hard context limit reached. Produce an aggressive recovery summary that preserves current goal, constraints, files touched, tool outcomes, and exact next steps. Prefer concise factual state over transcript detail.";

#[derive(Default)]
pub struct CompactionExtension;

impl maho_ext_api::Extension for CompactionExtension {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let state = std::sync::Arc::new(std::sync::Mutex::new(state::create_initial_state()));
        let restoration = std::sync::Arc::new(std::sync::Mutex::new(
            restoration_tracker::RestorationTrackerState::default(),
        ));
        let pending = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(
            String,
            (serde_json::Value, serde_json::Value),
        )>::new()));
        let live_api = std::sync::Arc::new(maho_ext_api::ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        let warm = std::sync::Arc::new(std::sync::Mutex::new(
            None::<speculative_job::LiveSpeculativeJob>,
        ));
        let generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let idle_since_end = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reminder = std::sync::Arc::new(std::sync::Mutex::new(
            token_budget_reminder::create_initial_reminder_state(),
        ));
        let degradation = std::sync::Arc::new(tokio::sync::Mutex::new(
            degradation_monitor::create_degradation_monitor_state(),
        ));
        let logger = std::sync::Arc::new(std::sync::Mutex::new(None::<log::CompactionLogger>));
        let lane = std::sync::Arc::new(std::sync::Mutex::new(
            lane_policy::CompactionLanePolicy::default(),
        ));
        {
            let degradation = std::sync::Arc::clone(&degradation);
            let warm = std::sync::Arc::clone(&warm);
            let generation = std::sync::Arc::clone(&generation);
            let live_api = std::sync::Arc::clone(&live_api);
            let lane = std::sync::Arc::clone(&lane);
            let compaction_state = std::sync::Arc::clone(&state);
            api.on(
                maho_ext_api::EventKind::MessageEnd,
                std::sync::Arc::new(move |event, context| {
                    let compaction_state = std::sync::Arc::clone(&compaction_state);
                    let degradation = std::sync::Arc::clone(&degradation);
                    let warm = std::sync::Arc::clone(&warm);
                    let generation = std::sync::Arc::clone(&generation);
                    let live_api = std::sync::Arc::clone(&live_api);
                    let lane = std::sync::Arc::clone(&lane);
                    Box::pin(async move {
                        let maho_ext_api::ExtensionEvent::MessageEnd { message } = event else {
                            return Ok(maho_ext_api::EventResult::None);
                        };
                        if message.as_assistant().is_some_and(|assistant| {
                            assistant.stop_reason == maho_ai::types::StopReason::Aborted
                        }) {
                            generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let retired = {
                                warm.lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .take()
                            };
                            if let Some(job) = retired {
                                job.cancel().await;
                            }
                        }
                        if lane_policy::live_disables_senpi_compaction(&lane, context)? {
                            return Ok(maho_ext_api::EventResult::None);
                        }
                        let content: Vec<_>;
                        let raw;
                        let trigger = {
                            let mut degradation = degradation.lock().await;
                            // begin_recovery is a no-op outside the post-compaction window; skip serializing the message.
                            if degradation.post_compaction_turns_remaining == 0 {
                                return Ok(maho_ext_api::EventResult::None);
                            }
                            raw = serde_json::to_value(message).map_err(|error| {
                                maho_ext_api::ExtensionFailure::new(error.to_string())
                            })?;
                            if !extension_wiring::is_monitorable_message(&raw) {
                                return Ok(maho_ext_api::EventResult::None);
                            }
                            content = raw["content"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|part| degradation_monitor::MonitoredMessageContentPart {
                                    kind: part["type"].as_str().unwrap_or_default(),
                                    text: part["text"].as_str(),
                                })
                                .collect();
                            degradation_monitor::begin_recovery(
                                &mut degradation,
                                &degradation_monitor::MonitoredMessageEvent {
                                    role: raw["role"].as_str().unwrap_or_default(),
                                    content: &content,
                                },
                            )
                        };
                        // The lock is released before applying: the resulting session_compact handler locks the same mutex.
                        if trigger {
                            let result = match extension_wiring::apply_live_blocking_compaction(
                                &live_api,
                                context,
                                &generation,
                                &warm,
                                &compaction_state,
                                degradation_monitor::RECOVERY_INSTRUCTIONS.into(),
                            )
                            .await
                            {
                                Ok(result) => degradation_monitor::RecoveryResult {
                                    applied: result
                                        == speculative::SpeculativeCompactionResult::Applied,
                                    reason: if result
                                        == speculative::SpeculativeCompactionResult::Failed
                                    {
                                        "failed"
                                    } else {
                                        "rejected"
                                    }
                                    .into(),
                                },
                                Err(_) => degradation_monitor::RecoveryResult {
                                    applied: false,
                                    reason: "failed".into(),
                                },
                            };
                            if degradation_monitor::should_notify_recovery(&result) {
                                context.ui.notify(
                                    degradation_monitor::RECOVERY_NOTIFICATION,
                                    maho_ext_api::NotificationType::Warning,
                                );
                            }
                        }
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
        }
        for kind in [
            maho_ext_api::EventKind::SessionCompact,
            maho_ext_api::EventKind::TurnEnd,
        ] {
            let degradation = std::sync::Arc::clone(&degradation);
            let compaction_state = std::sync::Arc::clone(&state);
            let live_api = std::sync::Arc::clone(&live_api);
            let generation = std::sync::Arc::clone(&generation);
            let warm = std::sync::Arc::clone(&warm);
            let lane = std::sync::Arc::clone(&lane);
            api.on(
                kind,
                std::sync::Arc::new(move |event, context| {
                    let warm = std::sync::Arc::clone(&warm);
                    let degradation = std::sync::Arc::clone(&degradation);
                    let compaction_state = std::sync::Arc::clone(&compaction_state);
                    let live_api = std::sync::Arc::clone(&live_api);
                    let generation = std::sync::Arc::clone(&generation);
                    let lane = std::sync::Arc::clone(&lane);
                    Box::pin(async move {
                        let mut state = degradation.lock().await;
                        match event {
                            maho_ext_api::ExtensionEvent::SessionCompact(
                                maho_ext_api::SessionCompactEvent::Accepted { .. },
                            ) => degradation_monitor::reset_on_session_compact(&mut state),
                            maho_ext_api::ExtensionEvent::TurnEnd { .. } => {
                                degradation_monitor::handle_turn_end(&mut state)
                            }
                            _ => {}
                        }
                        let recover = matches!(event, maho_ext_api::ExtensionEvent::TurnEnd { .. })
                            && !lane_policy::live_disables_senpi_compaction(&lane, context)?
                            && !state.recovery_triggered_this_cycle
                            && compaction_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .last_yield
                                .is_some_and(|last| last.saved_tokens <= 0.);
                        drop(state);
                        if recover {
                            let api = std::sync::Arc::clone(&live_api);
                            let context = context.clone();
                            let generation = std::sync::Arc::clone(&generation);
                            let warm = std::sync::Arc::clone(&warm);
                            let compaction_state = std::sync::Arc::clone(&compaction_state);
                            tokio::spawn(async move {
                                let _result = extension_wiring::apply_live_blocking_compaction(
                                    &api,
                                    &context,
                                    &generation,
                                    &warm,
                                    &compaction_state,
                                    degradation_monitor::RECOVERY_INSTRUCTIONS.into(),
                                )
                                .await;
                            });
                        }
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
        }
        for kind in [
            maho_ext_api::EventKind::SessionShutdown,
            maho_ext_api::EventKind::SessionCompact,
        ] {
            let warm = std::sync::Arc::clone(&warm);
            let generation = std::sync::Arc::clone(&generation);
            let reminder = std::sync::Arc::clone(&reminder);
            let idle = std::sync::Arc::clone(&idle_since_end);
            api.on(
                kind,
                std::sync::Arc::new(move |_, _| {
                    idle.store(false, std::sync::atomic::Ordering::SeqCst);
                    *reminder
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        token_budget_reminder::create_initial_reminder_state();
                    generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let retired = {
                        warm.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                    };
                    Box::pin(async move {
                        if let Some(job) = retired {
                            job.cancel().await;
                        }
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
        }
        {
            let warm = std::sync::Arc::clone(&warm);
            let generation = std::sync::Arc::clone(&generation);
            let state = std::sync::Arc::clone(&state);
            let live_api = std::sync::Arc::clone(&live_api);
            let lane = std::sync::Arc::clone(&lane);
            api.on(
                maho_ext_api::EventKind::ModelSelect,
                std::sync::Arc::new(move |event, context| {
                    let warm = std::sync::Arc::clone(&warm);
                    let generation = std::sync::Arc::clone(&generation);
                    let state = std::sync::Arc::clone(&state);
                    let live_api = std::sync::Arc::clone(&live_api);
                    let lane = std::sync::Arc::clone(&lane);
                    Box::pin(async move {
                        let maho_ext_api::ExtensionEvent::ModelSelect(event) = event else {
                            return Ok(maho_ext_api::EventResult::None);
                        };
                        let external = lane_policy::live_disables_senpi_compaction(&lane, context)?;
                        let mut settings =
                            maho_core::compaction::settings::default_compaction_settings();
                        if !external {
                            settings = extension_wiring::live_settings(context)?;
                        }
                        let (retired, start, epoch) = {
                            let mut job = warm
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let state = state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let (mut invalidate, mut start) = (false, false);
                            model_selection::handle_live_compaction_model_select(
                                event,
                                context,
                                &state,
                                job.as_ref().map(|job| &job.snapshot),
                                &settings,
                                external,
                                (|| invalidate = true, || start = true),
                            )?;
                            let retired = if invalidate {
                                generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                job.take()
                            } else {
                                None
                            };
                            (
                                retired,
                                start,
                                generation.load(std::sync::atomic::Ordering::SeqCst),
                            )
                        };
                        if let Some(job) = retired {
                            job.cancel().await;
                        }
                        if generation.load(std::sync::atomic::Ordering::SeqCst) != epoch {
                            return Ok(maho_ext_api::EventResult::None);
                        }
                        let mut job = warm
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if start && job.is_none() {
                            let next =
                                generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            *job = speculative_job::start_live_speculative_job(
                                &live_api,
                                context,
                                next,
                                "Proactively compact before the next agent turn.".into(),
                            )?;
                        }
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
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
            let latch = std::sync::Arc::new(std::sync::Mutex::new(
                emergency_prune::EmergencyPruneLatch::default(),
            ));
            let tools_api = std::sync::Arc::clone(&live_api);
            let request_tools_api = std::sync::Arc::clone(&live_api);
            let replay_boundary = std::sync::Arc::new(std::sync::Mutex::new(
                None::<openai_remote::OpenAiRemoteReplayBoundary>,
            ));
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
                    if lane_policy::live_disables_senpi_compaction(&idle_lane,context)? || !matches!(context.mode,maho_ext_api::ExtensionMode::Tui | maho_ext_api::ExtensionMode::Rpc) {return Ok(maho_ext_api::EventResult::None);}
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
            let policy_state = std::sync::Arc::clone(&state);
            let policy_lane = std::sync::Arc::clone(&lane);
            let log_logger = std::sync::Arc::clone(&logger);
            let lifecycle = context_reduction_lifecycle::register_context_reduction_lifecycle(
                api,
                std::sync::Arc::clone(&tools_api),
                context_reduction_lifecycle::ContextReductionLifecycleOptions {
                    emergency_prune_latch: latch,
                    emergency_instructions: EMERGENCY_COMPACTION_INSTRUCTIONS.into(),
                    get_tools: std::sync::Arc::new(move || {
                        extension_wiring::summarization_tools(&tools_api)
                    }),
                    get_request_tools: std::sync::Arc::new(move || {
                        extension_wiring::request_tools(&request_tools_api)
                    }),
                    get_policy: std::sync::Arc::new(move |context| {
                        let lane_owns_compaction =
                            lane_policy::live_disables_senpi_compaction(&policy_lane, context)?;
                        let append_only_transcript =
                            lane_policy::live_has_append_only_transcript(context)?;
                        let state = policy_state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        Ok(context_reduction_lifecycle::ContextReductionPolicy {
                            last_yield: state.last_yield,
                            breaker_tripped: circuit_breaker::is_tripped(
                                &state,
                                chrono::Utc::now().timestamp_millis() as f64,
                            ),
                            lane_owns_compaction,
                            append_only_transcript,
                        })
                    }),
                    log: std::sync::Arc::new(move |context, event, fields| {
                        let mut logger = log_logger
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let logger = logger.get_or_insert_with(|| {
                            log::CompactionLogger::new(Some(&context.agent_dir), None, None)
                        });
                        logger.debug(event, fields.as_object());
                    }),
                    replay_boundary: std::sync::Arc::clone(&replay_boundary),
                },
            );
            // senpi before_provider_request: replay the stored remote checkpoint in place of the
            // sealed checkpoint-owned items, for the origin this request will actually reach.
            let replay_events = api.events.clone();
            api.on(
                maho_ext_api::EventKind::BeforeProviderRequest,
                std::sync::Arc::new(move |event, context| {
                    let replay_boundary = std::sync::Arc::clone(&replay_boundary);
                    let events = replay_events.clone();
                    Box::pin(async move {
                        let maho_ext_api::ExtensionEvent::BeforeProviderRequest {
                            payload,
                            model,
                            headers,
                        } = event
                        else {
                            return Ok(maho_ext_api::EventResult::None);
                        };
                        let Some(model) = model.as_ref().or(context.model.as_ref()) else {
                            return Ok(maho_ext_api::EventResult::None);
                        };
                        if !openai_remote_model::is_openai_remote_compaction_model(Some(model)) {
                            return Ok(maho_ext_api::EventResult::None);
                        }
                        // With no remote checkpoint on the branch the rewrite returns before it
                        // reads the origin, so skip the auth lookup (an optimisation over senpi).
                        let branch = speculative::branch_values(context);
                        if !openai_remote::has_remote_compaction_checkpoint(&branch) {
                            return Ok(maho_ext_api::EventResult::None);
                        }
                        let Ok(auth) = context.model_registry.get_api_key_and_headers(model).await
                        else {
                            return Ok(maho_ext_api::EventResult::None);
                        };
                        let mut effective = model.clone();
                        if let Some(id) = auth.upstream_model_id.filter(|id| !id.is_empty()) {
                            effective.id = id;
                        }
                        if let Some(base_url) =
                            auth.auth.base_url.filter(|base_url| !base_url.is_empty())
                        {
                            effective.base_url = base_url;
                        }
                        let additional = headers
                            .clone()
                            .or(auth.auth.headers)
                            .unwrap_or_default();
                        let Ok(Some(request_headers)) =
                            openai_remote_model::create_live_openai_remote_compaction_headers(
                                &effective,
                                auth.auth.api_key.as_deref(),
                                &additional,
                                Some(context.session_manager.session_id()),
                            )
                        else {
                            return Ok(maho_ext_api::EventResult::None);
                        };
                        let origin = openai_remote_model::openai_remote_compaction_origin(
                            &effective,
                            &request_headers,
                        );
                        let boundary = replay_boundary
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        Ok(openai_remote::rewrite_openai_payload_with_remote_compaction(
                            payload,
                            Some(&effective),
                            &branch,
                            origin.as_ref(),
                            boundary.as_ref(),
                            &|data: serde_json::Value| {
                                events.emit(openai_remote::SENPI_COMPACTION_EVENT, &data)
                            },
                        )
                        .map_or(
                            maho_ext_api::EventResult::None,
                            maho_ext_api::EventResult::ProviderPayload,
                        ))
                    })
                }),
            );
            // senpi session_compact: an accepted compaction restarts the frontier.
            api.on(
                maho_ext_api::EventKind::SessionCompact,
                std::sync::Arc::new(move |event, _| {
                    if matches!(
                        event,
                        maho_ext_api::ExtensionEvent::SessionCompact(
                            maho_ext_api::SessionCompactEvent::Accepted { .. }
                        )
                    ) {
                        lifecycle.reset();
                    }
                    Box::pin(async { Ok(maho_ext_api::EventResult::None) })
                }),
            );
        }
        {
            let restoration = std::sync::Arc::clone(&restoration);
            api.on(
                maho_ext_api::EventKind::ToolCall,
                std::sync::Arc::new(move |event, _context| {
                    if let maho_ext_api::ExtensionEvent::ToolCall(event) = event {
                        restoration_tracker::track_tool_call(
                            &mut restoration
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner),
                            &event.tool_name,
                            &event.input,
                        );
                    }
                    Box::pin(async { Ok(maho_ext_api::EventResult::None) })
                }),
            );
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
                let claimed_generation = generation.load(std::sync::atomic::Ordering::SeqCst);
                let owns = match event {
                    maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) if !event.signal.is_aborted() => lane_policy::live_owns_compaction(&lane, context, event.reason),
                    _ => Ok(true),
                };
                let owns = match owns {
                    Ok(owns) => owns,
                    Err(error) => return Box::pin(async move {
                        if let Some(job) = retired {job.cancel().await;}
                        if let Some(job) = claimed {job.cancel().await;}
                        Err(error)
                    }),
                };
                let gate = if let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event {
                    let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !event.signal.is_aborted() && !owns {
                        Some(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::ExternalOwner), reason: Some(lane_policy::SDK_NATIVE_LANE_REJECTION_REASON.into()), ..Default::default() })
                    } else if !event.signal.is_aborted() && event.reason != maho_ext_api::CompactionReason::Manual && per_turn_cap::should_reject_by_cap(&state).cancel {
                        Some(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::PerTurnCap), reason: Some("automatic compaction refused after repeated ineffective attempts; run /compact to force one".into()), ..Default::default() })
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
                let generation = std::sync::Arc::clone(&generation);
                Box::pin(async move {
                    if let Some(job) = retired {job.cancel().await;}
                    if let Err(error) = result {
                        if let Some(job) = claimed {job.cancel().await;}
                        return Err(error);
                    }
                    let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { return Ok(maho_ext_api::EventResult::None); };
                    let mut inherited = None;
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
                        // senpi inheritedWarmFailure: only a current, unchanged transcript may reuse the warm failure.
                        inherited = settled.error.filter(|_|generation.load(std::sync::atomic::Ordering::SeqCst)==claimed_generation && job.snapshot.expected_revision==context.get_message_revision().unwrap_or(u64::MAX));
                    }
                    let result = extension_wiring::generate_core_route_compaction(&live_api, context, event, inherited).await;
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
            api.on(
                maho_ext_api::EventKind::SessionCompact,
                std::sync::Arc::new(move |event, context| {
                    if let maho_ext_api::ExtensionEvent::SessionCompact(
                        maho_ext_api::SessionCompactEvent::Rejected { request_id, .. },
                    ) = event
                    {
                        pending
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .retain(|(id, _)| id != request_id);
                    }
                    let result = if let maho_ext_api::ExtensionEvent::SessionCompact(
                        maho_ext_api::SessionCompactEvent::Accepted { request_id, .. },
                    ) = event
                    {
                        let metadata = {
                            let mut pending = pending
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            pending
                                .iter()
                                .position(|(id, _)| id == request_id)
                                .map(|index| pending.remove(index).1)
                        };
                        let persisted = if let Some((checkpoint, todos)) = metadata {
                            checkpoint_state::persist_checkpoint(&live_api, &checkpoint)
                                .and_then(|()| {
                                    live_api.append_entry(
                                        todo_bridge::TODO_SNAPSHOT_CUSTOM_TYPE,
                                        Some(todos),
                                    )
                                })
                                .and_then(|()| {
                                    todo_bridge::restore_todos_if_missing(&live_api, context)
                                })
                        } else {
                            Ok(())
                        };
                        persisted.and_then(|()| {
                            let settings = extension_wiring::live_settings(context)?;
                            let maho_ext_api::ExtensionEvent::SessionCompact(compact) = event
                            else {
                                return Ok(());
                            };
                            extension_wiring::prepare_accepted_restoration(
                                &mut restoration
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                                context,
                                compact,
                                &settings,
                            )
                        })
                    } else {
                        Ok(())
                    };
                    Box::pin(async move {
                        result?;
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
        }
        {
            let directive = std::sync::Arc::new(std::sync::Mutex::new(
                checkpoint_state::RestorationDirectiveState::default(),
            ));
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
                let entries: Vec<_> = context.session_manager.get_entries_of_kinds(&["custom"]).into_iter().map(|entry| {
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
                    if lane_policy::live_disables_senpi_compaction(&lane, context)? {return result;}
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
                    // senpi: the reminder epoch is read back from the branch, so a reload inside the same epoch does not
                    // deliver the notice again.
                    let persisted = token_budget_reminder::get_persisted_reminder_epoch(&context.session_manager.get_branch());
                    let computed = {
                        let mut previous=reminder.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let compaction_epoch=generation.load(std::sync::atomic::Ordering::SeqCst);
                        let state=if persisted.delivered {token_budget_reminder::TokenBudgetReminderState {last_fired_epoch:Some(compaction_epoch),lease:None}} else {std::mem::take(&mut *previous)};
                        let computed=token_budget_reminder::compute_token_budget_reminder(token_budget_reminder::TokenBudgetReminderInput {context_tokens:tokens.unwrap_or(0.)+additional,context_window:window,threshold_tokens:geometry.threshold_tokens,lead_tokens:geometry.lead_tokens,compaction_epoch,state});
                        *previous=computed.next_state;computed.message
                    };
                    let mut result=result?;
                    // orchestration.ts `resolveBeforeAgentStartMessage`: never the system prompt (cache prefix); the
                    // reminder rides a before-prompt message, a hidden `compaction-token-budget` one when there is none.
                    if let Some(reminder)=computed.filter(|reminder| !reminder.is_empty() && settings.ideal.reminder_enabled != Some(false)) {
                        let message=match &mut result {maho_ext_api::EventResult::BeforeAgentStart(output)=>output.message.take(),_=>None};
                        let mut message=match message {
                            Some(mut message)=>{message.content.push(maho_ext_api::ToolContent::text(format!("\n\n{reminder}")));message}
                            None=>maho_ext_api::CustomMessage {custom_type:"compaction-token-budget".to_owned(),content:vec![maho_ext_api::ToolContent::text(reminder)],display:false,details:None},
                        };
                        let mut details=message.details.take().filter(serde_json::Value::is_object).unwrap_or_else(||serde_json::json!({}));
                        details["tokenBudgetReminderEpoch"]=serde_json::Value::String(persisted.epoch);
                        message.details=Some(details);
                        result=maho_ext_api::EventResult::BeforeAgentStartPlaced(maho_ext_api::BeforeAgentStartEventResult {system_prompt_parts:None,message:Some(message),system_prompt:None},maho_ext_api::BeforeAgentStartPlacement::BeforePrompt);
                    }
                    Ok(result)
                })
            }), maho_ext_api::ExtensionHandlerOptions { preview_safe: true, ..Default::default() });
        }
        for kind in [
            maho_ext_api::EventKind::TurnEnd,
            maho_ext_api::EventKind::AgentEnd,
            maho_ext_api::EventKind::SessionCompact,
        ] {
            let state = std::sync::Arc::clone(&state);
            let lane = std::sync::Arc::clone(&lane);
            api.on(
                kind,
                std::sync::Arc::new(move |event, context| {
                    let state = std::sync::Arc::clone(&state);
                    let lane = std::sync::Arc::clone(&lane);
                    Box::pin(async move {
                        let rejected_owned = match event {
                            maho_ext_api::ExtensionEvent::SessionCompact(
                                maho_ext_api::SessionCompactEvent::Rejected {
                                    reason,
                                    rejection_cause,
                                    ..
                                },
                            ) if *rejection_cause
                                != maho_ext_api::CompactionRejectionCause::ExternalOwner =>
                            {
                                lane_policy::live_owns_compaction(&lane, context, *reason)?
                            }
                            _ => false,
                        };
                        let mut state = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let previous = std::mem::take(&mut *state);
                        *state = match event {
                            maho_ext_api::ExtensionEvent::TurnEnd { .. }
                            | maho_ext_api::ExtensionEvent::AgentEnd { .. } => {
                                state::end_turn(previous)
                            }
                            maho_ext_api::ExtensionEvent::SessionCompact(
                                maho_ext_api::SessionCompactEvent::Accepted {
                                    compaction_entry,
                                    ..
                                },
                            ) => per_turn_cap::record_accepted_compaction(
                                previous,
                                r#yield::recorded_yield(compaction_entry),
                            ),
                            maho_ext_api::ExtensionEvent::SessionCompact(
                                maho_ext_api::SessionCompactEvent::Rejected {
                                    reason,
                                    rejection_cause,
                                    ..
                                },
                            ) if *rejection_cause
                                != maho_ext_api::CompactionRejectionCause::ExternalOwner
                                && rejected_owned =>
                            {
                                circuit_breaker::record_failure(
                                    previous,
                                    chrono::Utc::now().timestamp_millis() as f64,
                                    Some(*reason),
                                    None,
                                )
                            }
                            _ => previous,
                        };
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
        }
    }
}
