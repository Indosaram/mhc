pub type SendCallback = std::sync::Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;
use crate::session_attribution::{SessionActivityRegistry, ToolAttributionSpans};
pub fn session_event_record(
    event: &maho_ext_api::AgentSessionEvent,
) -> Result<serde_json::Value, serde_json::Error> {
    use maho_ext_api::{
        AgentSessionEvent as E, CompactionReason as R, CompactionRejectionCause as C,
    };
    use serde_json::json;
    let reason = |value: &R| match value {
        R::Manual => "manual",
        R::Threshold => "threshold",
        R::Overflow => "overflow",
        R::PrePrompt => "pre_prompt",
        R::Branch => "branch",
        R::Extension => "extension",
    };
    let budget = |value: &maho_ext_api::ModelBudget| {
        let mut record = json!({"contextWindow":value.context_window,"liveContextTokens":value.live_context_tokens,"requiredTokens":value.required_tokens,"shortfallTokens":value.shortfall_tokens});
        if let Some(profile) = &value.safety_margin_profile {
            record["safetyMarginProfile"] = profile.clone().into();
        }
        record
    };
    macro_rules! optional {
        ($record:ident,$key:literal,$value:expr) => {
            if let Some(value) = $value {
                $record[$key] = serde_json::to_value(value)?;
            }
        };
    }
    Ok(match event {
        E::SessionPaused { pause_seq, kind, at, notice_count, persisted } => json!({"type":"session_paused","pauseSeq":pause_seq,"kind":kind,"at":at,"noticeCount":notice_count,"persisted":persisted}),
        E::SessionResumed { pause_seq, cause, at } => json!({"type":"session_resumed","pauseSeq":pause_seq,"cause":cause,"at":at}),
        E::SessionNotice { notice_ids, notice_count, disposition } => json!({"type":"session_notice","noticeIds":notice_ids,"noticeCount":notice_count,"disposition":disposition}),
        E::Agent(event) => serde_json::to_value(event)?,
        // senpi agent-session.ts: subscribers get `{ ...agent_end, willRetry }`; abort provenance is RPC enrichment.
        E::AgentEnd {
            messages,
            will_retry,
            ..
        } => json!({"type":"agent_end","messages":messages,"willRetry":will_retry}),
        E::MessageUpdate {
            message,
            assistant_message_event,
        } => {
            json!({"type":"message_update","message":message,"assistantMessageEvent":assistant_message_event})
        }
        E::AgentSettled => json!({"type":"agent_settled"}),
        E::AgentIdle => json!({"type":"agent_idle"}),
        E::SessionAbort => json!({"type":"session_abort"}),
        E::ResumeCompactionRequired { projection, notice } => {
            json!({"type":"resume_compaction_required","projection":projection,"notice":notice})
        }
        E::ResumeContextReduced {
            tokens_before,
            tokens_after,
            dropped_entries,
            notice,
        } => {
            json!({"type":"resume_context_reduced","tokensBefore":tokens_before,"tokensAfter":tokens_after,"droppedEntries":dropped_entries,"notice":notice})
        }
        E::CompactionStart {
            reason: r,
            request_id,
        } => {
            let mut record = json!({"type":"compaction_start","reason":reason(r)});
            optional!(record, "requestId", request_id);
            record
        }
        E::CompactionProgress {
            reason: r,
            delta,
            text,
        } => {
            let mut record = json!({"type":"compaction_progress","reason":reason(r)});
            optional!(record, "delta", delta);
            optional!(record, "text", text);
            record
        }
        E::CompactionEnd {
            reason: r,
            result,
            aborted,
            will_retry,
            error_message,
            rejection_cause,
            request_id,
            accepted,
        } => {
            let mut record = json!({"type":"compaction_end","reason":reason(r),"aborted":aborted,"willRetry":will_retry});
            if let Some(result) = result {
                let mut value = json!({"summary":result.summary,"firstKeptEntryId":result.first_kept_entry_id,"tokensBefore":result.tokens_before});
                optional!(value, "details", &result.details);
                record["result"] = value;
            }
            optional!(record, "errorMessage", error_message);
            optional!(record, "requestId", request_id);
            optional!(record, "accepted", accepted);
            if let Some(cause) = rejection_cause {
                record["rejectionCause"] = match cause {
                    C::CancelledByExtension => "cancelled-by-extension",
                    C::ExternalOwner => "external-owner",
                    C::WouldOverflow => "would-overflow",
                    C::CircuitBreaker => "circuit-breaker",
                    C::PerTurnCap => "per-turn-cap",
                    C::StaleRevision => "stale-revision",
                }
                .into();
            }
            record
        }
        E::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            json!({"type":"auto_retry_start","attempt":attempt,"maxAttempts":max_attempts,"delayMs":delay_ms,"errorMessage":error_message})
        }
        E::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => {
            let mut record = json!({"type":"auto_retry_end","success":success,"attempt":attempt});
            optional!(record, "finalError", final_error);
            record
        }
        E::SessionToolChild {
            state,
            generation,
            pid,
        } => json!({"type":"session_tool_child","state":state,"generation":generation,"pid":pid}),
        E::OffloadModuleStuck {
            module_path,
            export,
            deadline_ms,
            files,
            notice,
        } => {
            json!({"type":"offload_module_stuck","modulePath":module_path,"export":export,"deadlineMs":deadline_ms,"files":files,"notice":notice})
        }
        E::ContinuationError { error_message } => {
            json!({"type":"continuation_error","errorMessage":error_message})
        }
        E::RetryFallbackApplied {
            from,
            to,
            chain_key,
            reason,
            limit,
        } => {
            let mut record = json!({"type":"retry_fallback_applied","from":from,"to":to,"chainKey":chain_key,"reason":reason});
            if let Some(limit) = limit {
                record["limit"] = serde_json::to_value(limit)?;
            }
            record
        }
        E::RetryFallbackExhausted {
            chain_key,
            last_error,
        } => json!({"type":"retry_fallback_exhausted","chainKey":chain_key,"lastError":last_error}),
        E::RetryFallbackSucceeded { model, chain_key } => {
            json!({"type":"retry_fallback_succeeded","model":model,"chainKey":chain_key})
        }
        E::RetryFallbackReverted { from, to, cause } => {
            let mut record = json!({"type":"retry_fallback_reverted","from":from,"to":to});
            if let Some(cause) = cause {
                record["cause"] = cause.clone().into();
            }
            record
        }
        E::ModelChanged {
            model,
            thinking_level,
            source,
        } => {
            json!({"type":"model_changed","model":model,"thinkingLevel":thinking_level,"source":match source{maho_ext_api::ModelSelectSource::Set=>"set",maho_ext_api::ModelSelectSource::Cycle=>"cycle",maho_ext_api::ModelSelectSource::Restore=>"restore",maho_ext_api::ModelSelectSource::Fallback=>"fallback",maho_ext_api::ModelSelectSource::FallbackRevert=>"fallback_revert"}})
        }
        E::ModelChangePending {
            model,
            budget: b,
            notice,
        } => {
            let mut record = budget(b);
            record["type"] = "model_change_pending".into();
            record["model"] = json!(model);
            record["notice"] = json!(notice);
            record
        }
        E::ModelChangeRejected {
            model,
            reason,
            detail,
            budget: b,
        } => {
            let mut record = b.as_ref().map_or_else(|| json!({}), budget);
            record["type"] = "model_change_rejected".into();
            record["model"] = json!(model);
            record["reason"] = json!(reason);
            record["detail"] = json!(detail);
            record
        }
        E::ModelChangeSkipped {
            model,
            budget: b,
            direction,
        } => {
            let mut record = budget(b);
            record["type"] = "model_change_skipped".into();
            record["model"] = json!(model);
            record["direction"] = json!(direction);
            record
        }
        E::ServiceTierChanged { tier, fast_mode } => {
            let mut record = json!({"type":"service_tier_changed","fastMode":fast_mode});
            if let Some(tier) = tier {
                record["tier"] = match tier {
                    maho_ext_api::ServiceTier::Auto => "auto",
                    maho_ext_api::ServiceTier::Flex => "flex",
                    maho_ext_api::ServiceTier::Priority => "priority",
                    maho_ext_api::ServiceTier::Ultrafast => "ultrafast",
                }
                .into();
            }
            record
        }
        // senpi: core emits `{ type, level }`; `thinkingSelection` is RPC enrichment.
        E::ThinkingLevelChanged { level, .. } => {
            json!({"type":"thinking_level_changed","level":level})
        }
        E::HighReasoningWarning {
            model_id,
            provider,
            thinking_level,
        } => {
            json!({"type":"high_reasoning_warning","modelId":model_id,"provider":provider,"thinkingLevel":thinking_level})
        }
        E::ServerFallbackAborted {
            from,
            to,
            chain_configured,
        } => {
            json!({"type":"server_fallback_aborted","from":from,"to":to,"chainConfigured":chain_configured})
        }
        E::SessionSettingsChanged {
            steering_mode,
            follow_up_mode,
            auto_compaction_enabled,
        } => {
            json!({"type":"session_settings_changed","steeringMode":steering_mode,"followUpMode":follow_up_mode,"autoCompactionEnabled":auto_compaction_enabled})
        }
        E::SettingsSourceSelected { selection } => {
            let mut record = selection.clone();
            record["type"] = "settings_source_selected".into();
            record
        }
        E::SessionInfoChanged { name } => {
            let mut record = json!({"type":"session_info_changed"});
            optional!(record, "name", name);
            record
        }
        E::SystemPromptChange {
            system_prompt,
            previous_system_prompt,
            system_prompt_name,
            model,
            previous_model,
        } => {
            let mut record = json!({"type":"system_prompt_change","systemPrompt":system_prompt,"previousSystemPrompt":previous_system_prompt,"model":model});
            optional!(record, "previousModel", previous_model);
            optional!(record, "systemPromptName", system_prompt_name);
            record
        }
        E::EntryAppended { entry } => {
            let mut value = entry.data.clone();
            value["id"] = entry.id.clone().into();
            value["parentId"] = json!(entry.parent_id);
            value["timestamp"] = entry.timestamp.clone().into();
            value["type"] = entry.kind.clone().into();
            json!({"type":"entry_appended","entry":value})
        }
        E::SkillInvocation { skills } => {
            json!({"type":"skill_invocation","skills":skills.iter().map(|skill|json!({"name":skill.name,"path":skill.path,"syntax":skill.syntax})).collect::<Vec<_>>()})
        }
        E::CommandInvocation { command } => json!({"type":"command_invocation","command":command}),
        E::QueueUpdate {
            steering,
            follow_up,
            ordered,
        } => {
            json!({"type":"queue_update","steering":steering,"followUp":follow_up,"ordered":ordered.iter().map(|input|json!({"text":input.text,"mode":match input.mode{maho_ext_api::StreamingBehavior::Steer=>"steer",maho_ext_api::StreamingBehavior::FollowUp=>"followUp"},"enqueueOrder":input.enqueue_order})).collect::<Vec<_>>()})
        }
        E::ToolHookStatus(event) => {
            let mut record = json!({"type":"tool_hook_status","hookRunId":event.hook_run_id,"hookName":match event.hook_name{maho_ext_api::ToolHookName::PreToolUse=>"PreToolUse",maho_ext_api::ToolHookName::PostToolUse=>"PostToolUse"},"toolName":event.tool_name,"toolCallId":event.tool_call_id,"extensionPath":event.extension_path,"statusMessage":event.status_message,"startedAt":event.started_at});
            match &event.phase {
                maho_ext_api::ToolHookPhase::Start => record["phase"] = "start".into(),
                maho_ext_api::ToolHookPhase::Update => record["phase"] = "update".into(),
                maho_ext_api::ToolHookPhase::End {
                    completed_at,
                    status,
                    error_message,
                } => {
                    record["phase"] = "end".into();
                    record["completedAt"] = json!(completed_at);
                    record["status"] = match status {
                        maho_ext_api::ToolHookStatus::Completed => "completed",
                        maho_ext_api::ToolHookStatus::Blocked => "blocked",
                        maho_ext_api::ToolHookStatus::Failed => "failed",
                    }
                    .into();
                    optional!(record, "errorMessage", error_message);
                }
            }
            record
        }
        E::AuthLoginUrl { provider, url } => {
            json!({"type":"auth_login_url","provider":provider,"url":url})
        }
        E::AuthLoginEnd {
            provider,
            success,
            error,
        } => {
            let mut record = json!({"type":"auth_login_end","provider":provider,"success":success});
            optional!(record, "error", error);
            record
        }
        E::SummarizationRetryScheduled {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            json!({"type":"summarization_retry_scheduled","attempt":attempt,"maxAttempts":max_attempts,"delayMs":delay_ms,"errorMessage":error_message})
        }
        E::SummarizationRetryAttemptStart { source, reason: r } => {
            let mut record = json!({"type":"summarization_retry_attempt_start","source":source});
            if let Some(value) = r {
                record["reason"] = reason(value).into();
            }
            record
        }
        E::SummarizationRetryFinished => json!({"type":"summarization_retry_finished"}),
        E::RetryProbeScheduled {
            selector,
            at_ms,
            probe_index,
        } => {
            json!({"type":"retry_probe_scheduled","selector":selector,"atMs":at_ms,"probeIndex":probe_index})
        }
        E::RetryProbeResult {
            selector,
            ok,
            error_message,
        } => {
            let mut record = json!({"type":"retry_probe_result","selector":selector,"ok":ok});
            optional!(record, "errorMessage", error_message);
            record
        }
        E::ModelConfigError {
            model,
            provider,
            error_excerpt,
        } => {
            json!({"type":"model_config_error","model":model,"provider":provider,"errorExcerpt":error_excerpt})
        }
        E::SessionRecovered {
            cause,
            backup_path,
            dropped_bytes,
            notice,
        } => {
            json!({"type":"session_recovered","cause":cause,"backupPath":backup_path,"droppedBytes":dropped_bytes,"notice":notice})
        }
        E::BashExecutionUpdate { id, delta } => {
            let mut record = json!({"type":"bash_execution_update","delta":delta});
            optional!(record, "id", id);
            record
        }
    })
}
/// The binding's acceptance step (senpi `createRpcSessionBinding.enqueueRecord`): accepted before the
/// writer serializes or routes the record; a `session_replaced` is the next incarnation's bootstrap.
///
/// Rust records carry no object identity, so a record's recovery carrier is the one the session
/// attached when it produced it (`AgentSession::recovery_carrier`, core-session lane).
pub struct BindingAcceptance {
    lane: crate::session_registry::SharedAcceptanceLane,
    session: maho_core::agent_session::AgentSession,
}
impl BindingAcceptance {
    pub fn new(
        lane: crate::session_registry::SharedAcceptanceLane,
        session: maho_core::agent_session::AgentSession,
    ) -> Self {
        session.set_session_recovery_epoch(
            lane.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recovery_epoch(),
        );
        Self { lane, session }
    }
    pub fn accept(&self, record: &serde_json::Value) {
        let mut lane = self
            .lane
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if record.get("type").and_then(serde_json::Value::as_str) == Some("session_replaced") {
            let transcript = self
                .session
                .with_session_manager_mut(|manager| manager.begin_transcript_journal());
            lane.bootstrap(
                crate::session_acceptance_lane::SessionBootstrap { transcript },
                None,
            );
            self.session
                .set_session_recovery_epoch(lane.recovery_epoch());
            return;
        }
        let transitions = self
            .session
            .with_session_manager_mut(|manager| manager.drain_transcript());
        lane.apply_transcript(&transitions);
        let carrier = self.session.recovery_carrier(record);
        lane.accept(record, carrier.as_ref());
    }
}
/// senpi `createRpcSessionBinding.writeRecord`: a record produced after the entry's provider scope closed is
/// dropped instead of reaching the writer, with one stderr line per binding (senpi#2871: a late record from an
/// unawaited prompt must not resurrect a closed session's stream).
pub fn closed_scope_record_sink(
    session_id: String,
    scope: maho_ai::node::provider_scope::ProviderScope,
    sink: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
) -> std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync> {
    let dropped_after_close = std::sync::atomic::AtomicBool::new(false);
    std::sync::Arc::new(move |record| {
        if scope.state() == maho_ai::node::provider_scope::ScopeState::Closed {
            if !dropped_after_close.swap(true, std::sync::atomic::Ordering::SeqCst) {
                eprintln!(
                    "senpi rpc host: dropped a record for closed session {session_id} at {}",
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                );
            }
            return;
        }
        sink(record);
    })
}
pub struct BindingRecords {
    tools: ToolAttributionSpans,
}
impl BindingRecords {
    pub fn new(session_id: String, activity: SessionActivityRegistry) -> Self {
        Self {
            tools: ToolAttributionSpans::new(session_id, activity),
        }
    }
    pub fn enqueue_records(
        &mut self,
        chunk: &str,
        mut enqueue: impl FnMut(serde_json::Value),
    ) -> Result<(), serde_json::Error> {
        for line in chunk.split('\n').filter(|line| !line.is_empty()) {
            let record = serde_json::from_str(line)?;
            self.tools.observe(&record);
            enqueue(record);
        }
        Ok(())
    }
    pub fn dispose(&mut self) {
        self.tools.close_all();
    }
    pub fn deliver_event(
        &mut self,
        session_id: &str,
        event: &maho_ext_api::AgentSessionEvent,
        fanout: &mut crate::session_event_fanout::SessionEventFanout,
        writer: &crate::session_event_writer::SessionWriterActor,
    ) -> Result<(), String> {
        let record = session_event_record(event).map_err(|error| error.to_string())?;
        self.tools.observe(&record);
        for record in fanout.deliver_owned(session_id, None, false, record)? {
            writer.enqueue(session_id, record)?;
        }
        Ok(())
    }
    pub fn deliver_records(
        &mut self,
        session_id: &str,
        chunk: &str,
        fanout: &mut crate::session_event_fanout::SessionEventFanout,
        writer: &crate::session_event_writer::SessionWriterActor,
    ) -> Result<(), String> {
        for line in chunk.split('\n').filter(|line| !line.is_empty()) {
            let record: serde_json::Value =
                serde_json::from_str(line).map_err(|error| error.to_string())?;
            self.tools.observe(&record);
            for record in fanout.deliver_owned(session_id, None, false, record)? {
                writer.enqueue(session_id, record)?;
            }
        }
        Ok(())
    }
}
/// A session-owned adapter around the classic command loop and extension-UI wiring: the
/// classic handler stays the single source of command semantics, and its records are routed
/// through the host's session record router (senpi `createRpcSessionBinding`).
pub struct RpcSessionBinding {
    session_id: String,
    handler: crate::connection_handler::RpcConnectionHandler,
    sink: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
    subscription: std::sync::Mutex<Option<maho_core::agent_session::AgentSessionSubscription>>,
    hold_subscription: std::sync::Mutex<Option<maho_core::event_bus::EventSubscription>>,
    security_subscription: std::sync::Mutex<Option<maho_core::event_bus::EventSubscription>>,
    relocation_subscription:
        std::sync::Mutex<Option<maho_core::agent_session::AgentSessionSubscription>>,
    /// senpi `sink.writeStructured`: the session's structured lane, held only behind the compact-updates flag.
    structured_subscription:
        std::sync::Mutex<Option<maho_core::structured_session_lane::StructuredSubscription>>,
    connection_roles: Option<SharedConnectionRoles>,
    /// senpi `bindMcpForms(session, ui)`: the session's private form binding and the writer state that owns it.
    mcp_forms: std::sync::Mutex<
        Option<(
            crate::session_event_writer::McpFormsUnbind,
            std::sync::Arc<crate::session_event_writer::SessionMcpPresenters>,
        )>,
    >,
    /// The writer's presenter state: `handle` answers a private `mcp_form_*` record there, never in the session.
    presenters: std::sync::Mutex<
        Option<(
            std::sync::Arc<crate::session_event_writer::SessionMcpPresenters>,
            PresenterReply,
        )>,
    >,
}
/// Writes one private `mcp_form_reply` raw to the sending connection, outside every session queue.
pub type PresenterReply = std::sync::Arc<dyn Fn(Option<&str>, &serde_json::Value) + Send + Sync>;
/// The connection roles one host (or one worker isolate) keeps, shared with every binding it creates (senpi#3811).
/// The correlation stage a binding's records pass through (senpi `new ClientMessageEvents(output, () =>
/// ClientAdmissions.forSession(session).hasIdentities)`). Only a ledger that already exists is read.
fn new_client_message_events(
    sink: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
    route: String,
) -> crate::client_message_events::ClientMessageEvents {
    crate::client_message_events::ClientMessageEvents::new(
        move |record, _carrier| sink(record),
        move || {
            crate::client_admissions::ClientAdmissions::existing(&route)
                .is_some_and(|admissions| admissions.has_identities())
        },
    )
}
pub type SharedConnectionRoles =
    std::sync::Arc<std::sync::Mutex<crate::connection_roles::ConnectionRoles>>;
/// Builds an RPC binding for one live session: input lines route through the handler and every
/// produced record goes to `sink`, which tags it with the routing id and fans it out to the
/// connections attached to the session (senpi `SessionEventWriter.enqueue`).
pub async fn create_rpc_session_binding(
    session_id: String,
    session: maho_core::agent_session::AgentSession,
    sink: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
    capabilities: Vec<String>,
    connection_roles: Option<SharedConnectionRoles>,
) -> RpcSessionBinding {
    let hold_sink = sink.clone();
    let hold_subscription = session.on_extension_event(
        maho_core::agent_session::SESSION_HOLD_CHANNEL,
        std::sync::Arc::new(move |record| hold_sink(record.clone())),
    );
    let security_sink = sink.clone();
    let security_subscription = session.on_extension_event(
        "session:security-notices",
        std::sync::Arc::new(move |snapshot| {
            security_sink(serde_json::json!({"type":"security_notices","snapshot":snapshot}))
        }),
    );
    sink(serde_json::json!({"type":"security_notices","snapshot":session.security_notices()}));
    let event_sink = sink.clone();
    let last_abort_source: crate::connection_handler::LastAbortSource = Default::default();
    let (event_session, event_abort_source) = (session.weak_accessor(), last_abort_source.clone());
    // The routing handle is the key this session's admission ledger is published under.
    let event_route = session_id.clone();
    // senpi session-record-stream.ts: records go through client-message correlation, enabled once the session's
    // admission ledger has seen a client identity.
    let correlated = std::sync::Mutex::new(new_client_message_events(
        event_sink.clone(),
        event_route.clone(),
    ));
    let subscription = session.subscribe(std::sync::Arc::new(move |event| {
        let record = match event_session() {
            Some(session) => crate::connection_handler::rpc_session_event_record_routed(
                &session,
                event,
                &event_abort_source,
                Some(&event_route),
            ),
            None => session_event_record(event),
        };
        match record {
            Ok(record) => correlated
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .accept(record, None),
            Err(error) => {
                event_sink(serde_json::json!({"type":"rpc_error","error":error.to_string()}))
            }
        }
    }));
    // A hosted session keeps the native `ctx.shutdown()` effect (dispose this session); it never
    // ends the multi-session host process.
    let shutdown_session = session.weak_accessor();
    let shutdown: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
        if let Some(session) = shutdown_session() {
            tokio::spawn(async move {
                session.dispose().await;
            });
        }
    });
    let mut handler =
        crate::connection_handler::RpcConnectionHandler::create_with_shutdown_handler(
            session,
            sink.clone(),
            capabilities,
            Some(shutdown),
        )
        .await;
    handler.last_abort_source = last_abort_source;
    // A hosted session reports dialog registration and settlement to dialog observers (senpi `uiRequestChanged`).
    handler.report_ui_request_changes(sink.clone());
    if let Some(roles) = &connection_roles {
        handler = handler.with_connection_roles(roles.clone());
    }
    RpcSessionBinding {
        session_id,
        handler,
        sink,
        subscription: std::sync::Mutex::new(Some(subscription)),
        hold_subscription: std::sync::Mutex::new(Some(hold_subscription)),
        security_subscription: std::sync::Mutex::new(Some(security_subscription)),
        relocation_subscription: Default::default(),
        structured_subscription: Default::default(),
        connection_roles,
        mcp_forms: Default::default(),
        presenters: Default::default(),
    }
}
impl RpcSessionBinding {
    /// senpi session-binding.ts `writeStructured` (only when `structuredLaneEnabled()`): the session's structured
    /// records, tagged with this routing handle, go to `lanes` while `scope` is open; never to a legacy destination.
    pub fn install_structured_lane(
        &self,
        session: &maho_core::agent_session::AgentSession,
        lanes: crate::session_structured_lanes::SessionStructuredLanes,
        scope: maho_ai::node::provider_scope::ProviderScope,
    ) {
        let tag = self.session_id.clone();
        let output: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync> =
            std::sync::Arc::new(move |mut record: serde_json::Value| {
                if scope.state() == maho_ai::node::provider_scope::ScopeState::Closed {
                    return;
                }
                record["sessionId"] = tag.clone().into();
                lanes.accept(&tag, &record);
            });
        // An agent built outside the session layer has no structured lane; the legacy records still flow.
        let subscription = crate::structured_session_records::subscribe_structured_session_records(
            session, output,
        )
        .ok();
        *self
            .structured_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = subscription;
    }
    pub fn keep_relocation_subscription(
        &self,
        subscription: maho_core::agent_session::AgentSessionSubscription,
    ) {
        *self
            .relocation_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(subscription);
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    /// The roles registry authority checks read (senpi `options.connectionRoles`).
    pub fn connection_roles(&self) -> Option<&SharedConnectionRoles> {
        self.connection_roles.as_ref()
    }
    pub fn ui(&self) -> &std::sync::Arc<crate::connection_handler::RpcExtensionUi> {
        self.handler.ui()
    }
    /// senpi session-binding `bindMcpForms`: with presenters enabled, binds this session's private forms with
    /// `admitHumanAnswer` as the admission, attaching the connections already attached; `handle` then answers
    /// private records through the writer. A no-op when the deployment does not enforce presenters.
    pub fn bind_mcp_forms(
        &self,
        presenters: &std::sync::Arc<crate::session_event_writer::SessionMcpPresenters>,
        session: &maho_core::agent_session::AgentSession,
        attached: &[String],
        reply: PresenterReply,
    ) {
        *self
            .presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((std::sync::Arc::clone(presenters), reply));
        let admitted = session.weak_accessor();
        let unbind = presenters.bind(
            &self.session_id,
            &session.session_id(),
            std::sync::Arc::new(move || {
                if let Some(session) = admitted() {
                    session.admit_human_answer();
                }
            }),
            attached,
        );
        let previous = std::mem::replace(
            &mut *self
                .mcp_forms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            unbind.map(|unbind| (unbind, std::sync::Arc::clone(presenters))),
        );
        if let Some((previous, presenters)) = previous {
            previous.unbind(&presenters);
        }
    }
    /// Feed one inbound command; a response the handler produced is routed tagged to this session.
    pub async fn handle(&self, command: &serde_json::Value) -> Result<(), String> {
        let connection = crate::multi_session_host::CURRENT_CONNECTION
            .try_with(Clone::clone)
            .ok()
            .flatten();
        // senpi `if (writer.handleMcpForm(command)) return`: private records never reach the session handler.
        let presenters = self
            .presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some((presenters, write)) = presenters
            && let Some(reply) = presenters.handle(command, connection.as_deref())
        {
            write(connection.as_deref(), &reply);
            return Ok(());
        }
        let line = serde_json::to_string(command).map_err(|error| error.to_string())?;
        if command["type"] == "extension_ui_response"
            && let Some(connection) = &connection
            && let Some(id) = crate::extension_ui_response::answered_ui_request_id(command)
        {
            self.handler.bind_grant_connection(id, connection);
        }
        if let Some(response) = self
            .handler
            .handle_input_line_for_connection(&line, connection.as_deref())
            .await
            .map_err(|error| error.to_string())?
        {
            for record in response.split('\n').filter(|record| !record.is_empty()) {
                let value: serde_json::Value =
                    serde_json::from_str(record).map_err(|error| error.to_string())?;
                (self.sink)(value);
            }
        }
        Ok(())
    }
    pub fn install_relocation(
        &self,
        session: &maho_core::agent_session::AgentSession,
        fanout: std::sync::Arc<std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>>,
    ) {
        self.handler
            .register_grant_presentation(&session.session_id());
        let tag = self.session_id.clone();
        let offer_fanout = fanout.clone();
        let offerer: std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync> =
            std::sync::Arc::new(move || {
                let fanout = offer_fanout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                fanout
                    .attached_connections(&tag)
                    .into_iter()
                    .find(|id| fanout.connection_has(id, "session_relocation"))
            });
        let send_fanout = fanout;
        let tag = self.session_id.clone();
        let send: SendCallback =
            std::sync::Arc::new(move |id, mut record| {
                record["sessionId"] = tag.clone().into();
                if let Err(error) = send_fanout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .enqueue_control(id, &record)
                {
                    eprintln!("relocation delivery failed: {error}");
                }
            });
        session.set_relocation_host(Some(std::sync::Arc::new(
            crate::session_relocation_adapter::SessionRelocationAdapter {
                relocation: self.handler.relocation().clone(),
                session: session.weak_accessor(),
                offerer,
                send,
            },
        )));
    }
    pub fn cancel_grant(&self, id: &str) {
        self.handler.cancel_grant(id);
    }
    pub fn connection_detached(&self, connection: &str) {
        self.handler.connection_detached(connection);
    }
    pub fn relocation(&self) -> &crate::session_relocation_legacy::RpcSessionRelocation {
        self.handler.relocation()
    }
    pub fn cancel_pending_extension_ui_requests(&self) {
        self.handler.cancel_pending_extension_ui_requests();
    }
    pub async fn dispose(&self) {
        let forms = self
            .mcp_forms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((unbind, presenters)) = forms {
            unbind.unbind(&presenters);
        }
        self.hold_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.security_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.relocation_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.structured_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.handler.dispose().await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // senpi session-record-stream.ts: correlation runs once the route's admission ledger has a client identity; a
    // turn's records then carry the input that caused them.
    #[tokio::test]
    async fn binding_records_are_correlated_once_the_route_has_client_identities() {
        use crate::client_admissions::ClientAdmissions;
        use crate::effect_session::EffectSession;
        let route = "binding-correlation";
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let sink_written = written.clone();
        let sink: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync> =
            std::sync::Arc::new(move |record| sink_written.lock().unwrap().push(record));
        let mut events = new_client_message_events(sink, route.into());
        let user = serde_json::json!({"type":"message_start","message":{"role":"user","clientMessageId":"m1"}});
        let turn = serde_json::json!({"type":"turn_end"});
        // No ledger yet: records pass through untouched.
        events.accept(user.clone(), None);
        events.accept(turn.clone(), None);
        assert_eq!(*written.lock().unwrap(), vec![user.clone(), turn.clone()]);
        written.lock().unwrap().clear();
        let session: std::sync::Arc<dyn EffectSession> = std::sync::Arc::new(
            crate::client_admissions::test_session::FakeSession::new(route),
        );
        let ledger = ClientAdmissions::for_session(&session).await;
        ledger
            .admit(
                &serde_json::json!({"type":"prompt","message":"hi","clientMessageId":"m1"}),
                |_| -> futures::future::BoxFuture<
                    'static,
                    Result<crate::client_admission_record::AdmissionDisposition, String>,
                > {
                    Box::pin(async {
                        Ok(crate::client_admission_record::AdmissionDisposition::Started)
                    })
                },
            )
            .await
            .unwrap();
        events.accept(user, None);
        events.accept(turn, None);
        assert_eq!(written.lock().unwrap()[1]["clientMessageId"], "m1");
        ClientAdmissions::forget(route);
    }
    #[test]
    fn attribution_commits_before_event_publication_and_dispose_closes_spans() {
        let activity = SessionActivityRegistry::default();
        let mut binding = BindingRecords::new("session".into(), activity.clone());
        let mut published = 0;
        binding.enqueue_records("\n{\"type\":\"tool_execution_start\",\"toolCallId\":\"id\",\"toolName\":\"bash\"}\n",|record|{published+=1;assert_eq!(record["toolName"],"bash");assert_eq!(activity.since(activity.mark()).unwrap().tool.as_deref(),Some("bash"));}).unwrap();
        assert_eq!(published, 1);
        binding.dispose();
        assert!(activity.since(activity.mark()).is_none());
    }
    #[test]
    fn a_record_after_the_scope_closed_is_dropped_not_written() {
        let scope = maho_ai::node::provider_scope::ProviderScope::new();
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let into = written.clone();
        let sink = closed_scope_record_sink(
            "s".into(),
            scope.clone(),
            std::sync::Arc::new(move |record| into.lock().unwrap().push(record)),
        );
        sink(serde_json::json!({"type":"agent_start"}));
        scope.close();
        sink(serde_json::json!({"type":"agent_end"}));
        sink(serde_json::json!({"type":"agent_end"}));
        assert_eq!(*written.lock().unwrap(), vec![serde_json::json!({"type":"agent_start"})]);
    }
    #[test]
    fn malformed_record_stops_stream_before_later_publication() {
        let activity = SessionActivityRegistry::default();
        let mut binding = BindingRecords::new("s".into(), activity);
        let mut records = vec![];
        assert!(
            binding
                .enqueue_records("{}\ninvalid\n{}", |record| records.push(record))
                .is_err()
        );
        assert_eq!(records.len(), 1);
    }
}
