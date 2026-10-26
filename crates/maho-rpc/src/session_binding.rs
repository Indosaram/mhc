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
        } => {
            json!({"type":"retry_fallback_applied","from":from,"to":to,"chainKey":chain_key,"reason":reason})
        }
        E::RetryFallbackExhausted {
            chain_key,
            last_error,
        } => json!({"type":"retry_fallback_exhausted","chainKey":chain_key,"lastError":last_error}),
        E::RetryFallbackSucceeded { model, chain_key } => {
            json!({"type":"retry_fallback_succeeded","model":model,"chainKey":chain_key})
        }
        E::RetryFallbackReverted { from, to } => {
            json!({"type":"retry_fallback_reverted","from":from,"to":to})
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
        E::BashExecutionUpdate { id, delta } => {
            let mut record = json!({"type":"bash_execution_update","delta":delta});
            optional!(record, "id", id);
            record
        }
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
    connection_roles: Option<SharedConnectionRoles>,
}
/// The connection roles one host (or one worker isolate) keeps, shared with every binding it creates (senpi#3811).
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
    let subscription = session.subscribe(std::sync::Arc::new(move |event| {
        let record = match event_session() {
            Some(session) => crate::connection_handler::rpc_session_event_record(
                &session,
                event,
                &event_abort_source,
            ),
            None => session_event_record(event),
        };
        match record {
            Ok(record) => event_sink(record),
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
        connection_roles,
    }
}
impl RpcSessionBinding {
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
    /// Feed one inbound command; a response the handler produced is routed tagged to this session.
    pub async fn handle(&self, command: &serde_json::Value) -> Result<(), String> {
        let line = serde_json::to_string(command).map_err(|error| error.to_string())?;
        let connection = crate::multi_session_host::CURRENT_CONNECTION
            .try_with(Clone::clone)
            .ok()
            .flatten();
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
