use serde_json::{Value,json};
use crate::checkpoint_state::get_latest_checkpoint;
pub fn resolved_settings(
    legacy: &maho_ext_api::CompactionSettings,
    resolved: Option<&maho_ext_api::ResolvedCompactionSettings>,
) -> Result<maho_core::compaction::settings::CompactionSettings, maho_ext_api::ExtensionFailure> {
    let mut settings = maho_core::compaction::settings::default_compaction_settings();
    settings.enabled = legacy.enabled;
    settings.reserve_tokens = i64::try_from(legacy.reserve_tokens).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    settings.keep_recent_tokens = i64::try_from(legacy.keep_recent_tokens).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    let Some(resolved) = resolved else {
        settings.speculative_enabled = Some(false);
        settings.idle_compaction_enabled = Some(false);
        settings.restoration_enabled = Some(false);
        settings.ideal.grace_band_enabled = Some(false);
        settings.ideal.tool_admission_enabled = Some(false);
        settings.ideal.reminder_enabled = Some(false);
        settings.ideal.reserve_scaling_enabled = Some(false);
        return Ok(settings);
    };
    settings.enabled = resolved.enabled;
    settings.reserve_tokens = i64::try_from(resolved.reserve_tokens).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    settings.keep_recent_tokens = i64::try_from(resolved.keep_recent_tokens).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    settings.speculative_enabled = Some(resolved.speculative_enabled);
    settings.speculative_fraction = Some(resolved.speculative_fraction);
    settings.speculative_cooldown_ms = Some(resolved.speculative_cooldown_ms);
    settings.restoration_enabled = Some(resolved.restoration_enabled);
    settings.restoration_max_items = Some(resolved.restoration_max_items);
    settings.restoration_max_tokens_per_item = Some(resolved.restoration_max_tokens_per_item);
    settings.restoration_max_total_tokens = Some(resolved.restoration_max_total_tokens);
    settings.restoration_context_ratio = Some(resolved.restoration_context_ratio);
    settings.idle_compaction_enabled = Some(resolved.idle_compaction_enabled);
    settings.summarization_max_duration_ms = resolved.summarization_max_duration_ms;
    settings.ideal = maho_core::compaction::ideal_settings::IdealCompactionSettings {
        grace_band_enabled: Some(resolved.grace_band_enabled), tool_admission_enabled: Some(resolved.tool_admission_enabled),
        reminder_enabled: Some(resolved.reminder_enabled), reserve_scaling_enabled: Some(resolved.reserve_scaling_enabled),
        speculative_lead_tokens: resolved.speculative_lead_tokens,
    };
    Ok(settings)
}
pub fn live_settings(context: &maho_ext_api::ExtensionContext) -> Result<maho_core::compaction::settings::CompactionSettings, maho_ext_api::ExtensionFailure> {
    let resolved = context.get_resolved_compaction_settings()?;
    resolved_settings(&context.get_compaction_settings()?, resolved.as_ref())
}
pub fn estimate_pending_prompt_tokens(prompt:Option<&str>,image_count:usize)->usize {prompt.unwrap_or_default().encode_utf16().count().div_ceil(4)+image_count*1200}
pub fn get_prompt_context_window(window:f64,max_tokens:Option<f64>)->f64 {
    match max_tokens {Some(max) if max.is_finite() && max>0.0 && window>0.0 => window-max.min((window*0.5).floor()),_=>window}
}
pub fn with_additional_tokens(usage:&Value,additional:f64)->Value {
    let Some(tokens)=usage.get("tokens").and_then(Value::as_f64) else {return usage.clone();};
    if additional<=0.0 {return usage.clone();}
    let mut result=usage.clone();result["tokens"]=json!(tokens+additional);
    if let Some(window)=usage.get("contextWindow").and_then(Value::as_f64).filter(|w|*w>0.0) {result["percent"]=json!((tokens+additional)/window*100.0);}
    result
}
pub fn is_monitorable_message(message:&Value)->bool {message.get("content").is_some_and(Value::is_array)}
pub fn is_aborted_assistant_message(message:&Value)->bool {message.get("role").and_then(Value::as_str)==Some("assistant") && message.get("stopReason").and_then(Value::as_str)==Some("aborted")}
pub fn is_required_compaction_fallback_reason(reason:&str)->bool {matches!(reason,"manual"|"threshold"|"overflow")}
pub fn recent_checkpoint(entries:&[Value],now:f64)->Option<Value> {
    let checkpoint=get_latest_checkpoint(entries)?;
    let timestamp=checkpoint.get("timestamp").and_then(Value::as_f64)?;
    if timestamp!=0.0 && now-timestamp<=60000.0 {Some(checkpoint)} else {None}
}
pub fn compaction_feedback(applied:bool,reason:&str,aborted:bool,remote_fallback:Option<&str>)->Option<Value> {
    if applied || reason=="rejected" {return None;}
    let parts:Vec<_>=remote_fallback.into_iter().chain(std::iter::once(reason)).filter(|p|!p.is_empty()).collect();
    let mut result=json!({"reason":"extension","aborted":aborted});
    if !aborted && !parts.is_empty() {result["errorMessage"]=json!(format!("Compaction did not apply: {}",parts.join("; local fallback ")));}
    Some(result)
}

pub fn end_compaction_feedback(
    context: &maho_ext_api::ExtensionContext,
    signal: Option<maho_ext_api::AbortSignal>,
    applied: bool,
    reason: &str,
    remote_fallback: Option<&str>,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let aborted = signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted);
    let Some(feedback) = compaction_feedback(applied, reason, aborted, remote_fallback) else { return Ok(()); };
    context.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Extension,
        signal,
        aborted: Some(aborted),
        error_message: feedback.get("errorMessage").and_then(Value::as_str).map(str::to_owned),
    })
}

pub fn create_blocking_remote_compaction_event(
    context: &maho_ext_api::ExtensionContext,
    preparation: maho_ext_api::CompactionPreparation,
    request_id: String,
    custom_instructions: String,
    signal: maho_ext_api::AbortSignal,
) -> maho_ext_api::SessionBeforeCompactEvent {
    maho_ext_api::SessionBeforeCompactEvent {
        reason: maho_ext_api::CompactionReason::Extension,
        will_retry: false,
        request_id,
        preparation,
        branch_entries: context.session_manager.get_branch(),
        custom_instructions: Some(custom_instructions),
        signal,
    }
}

pub struct AbortLink {
    source: maho_ai::utils::abort::AbortSignal,
    listener: maho_ai::utils::abort::ListenerId,
}
impl Drop for AbortLink {
    fn drop(&mut self) { self.source.remove_abort_listener(self.listener); }
}
pub fn link_abort_signal(source: Option<&maho_ai::utils::abort::AbortSignal>, target: &maho_ai::utils::abort::AbortController) -> Option<AbortLink> {
    let source = source?;
    if source.aborted() { target.abort(None); return None; }
    let target = target.clone();
    let listener = source.add_abort_listener(move |_|target.abort(None));
    Some(AbortLink { source: source.clone(), listener })
}

pub fn create_live_blocking_remote_compaction_event(
    context: &maho_ext_api::ExtensionContext,
    preparation: maho_ext_api::CompactionPreparation,
    custom_instructions: String,
    signal: maho_ext_api::AbortSignal,
) -> maho_ext_api::SessionBeforeCompactEvent {
    create_blocking_remote_compaction_event(context, preparation, uuid::Uuid::new_v4().to_string(), custom_instructions, signal)
}

pub fn summarization_tools(api: &maho_ext_api::ExtensionApi) -> Vec<maho_ai::types::Tool> {
    let Ok(definitions) = api.get_all_tools() else { return Vec::new(); };
    let Ok(active) = api.get_active_tools() else { return Vec::new(); };
    active.iter().filter_map(|name|definitions.iter().find(|tool|&tool.name == name)).map(|tool|maho_ai::types::Tool {
        name: tool.name.clone(), description: tool.description.clone(), parameters: tool.parameters.clone(), freeform: None, constrained_sampling: None,
    }).collect()
}

pub fn prepare_accepted_restoration(
    state: &mut crate::restoration_tracker::RestorationTrackerState,
    context: &maho_ext_api::ExtensionContext,
    event: &maho_ext_api::SessionCompactEvent,
    settings: &maho_core::compaction::settings::CompactionSettings,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let maho_ext_api::SessionCompactEvent::Accepted { reason, compaction_entry, .. } = event else { return Ok(()); };
    if settings.restoration_enabled == Some(false) { return Ok(()); }
    let entries = crate::speculative::branch_values(context);
    let first_kept = compaction_entry.data["firstKeptEntryId"].as_str();
    let kept: Vec<_> = entries.iter().position(|entry|entry["id"].as_str() == first_kept).map_or_else(Vec::new, |index| {
        entries[index..].iter().filter(|entry|entry["type"] == "message").map(|entry|entry["message"].clone()).collect()
    });
    let usage = context.get_context_usage()?;
    let window = usage.as_ref().map_or_else(||context.model.as_ref().map_or(200000,|model|model.context_window),|usage|usage.context_window) as f64;
    let restoration = crate::restoration_tracker::RestorationSettings {
        max_items: settings.restoration_max_items, max_tokens_per_item: settings.restoration_max_tokens_per_item,
        max_total_tokens: settings.restoration_max_total_tokens, context_ratio: settings.restoration_context_ratio,
    };
    let reason = match reason { maho_ext_api::CompactionReason::Manual => "manual", maho_ext_api::CompactionReason::Threshold => "threshold", maho_ext_api::CompactionReason::Overflow => "overflow", maho_ext_api::CompactionReason::Branch => "branch", maho_ext_api::CompactionReason::PrePrompt => "pre-prompt", maho_ext_api::CompactionReason::Extension => "extension" };
    crate::restoration_tracker::prepare_pending_payload(state, &crate::restoration_tracker::PreparePendingPayloadOptions {
        accepted: true, reason, compaction_entry_id: &compaction_entry.id, context_window: window,
        usage_tokens: usage.and_then(|usage|usage.tokens).map(|tokens|tokens as f64),
        reserve_tokens: crate::policy::resolve_effective_reserve_tokens(window,settings.reserve_tokens as f64,settings.ideal.reserve_scaling_enabled), settings: &restoration, kept_messages: &kept,
    });
    Ok(())
}

pub async fn generate_core_route_compaction(
    api: &maho_ext_api::ExtensionApi,
    context: &maho_ext_api::ExtensionContext,
    event: &maho_ext_api::SessionBeforeCompactEvent,
) -> Result<maho_ext_api::EventResult, maho_ext_api::ExtensionFailure> {
    use maho_ext_api::{EventResult, SessionBeforeEventResult};
    if event.signal.is_aborted() { return Ok(EventResult::None); }
    let Some(model) = context.model.clone() else { return Ok(EventResult::None); };
    if event.reason != maho_ext_api::CompactionReason::Branch && crate::openai_remote_model::is_openai_remote_compaction_model(Some(&model)) {
        let key = context.model_registry.get_api_key_for_provider(&model.provider).await?;
        let branch = crate::speculative::branch_values(context);
        let messages = maho_core::session_manager::build_session_context(&branch, None).messages;
        let messages = messages.into_iter().map(serde_json::from_value).collect::<Result<Vec<maho_ext_api::AgentMessage>,_>>().map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        let prepared = context.prepare_provider_request(messages).await?;
        let raw = prepared.messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>,_>>().map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        let session_id = context.session_manager.session_id();
        let system_prompt = context.get_system_prompt();
        if let Some(request) = crate::openai_remote::create_openai_remote_compaction_request(Some(&model),&system_prompt,&branch,Some(&raw),event.preparation.tokens_before,Some(session_id),None) {
            let configured = model.headers.clone().unwrap_or_default().into_iter().map(|(key,value)|(key,Some(value))).collect();
            let transformed = (prepared.transform_headers)(configured).await?;
            if let Some(headers) = crate::openai_remote_model::create_live_openai_remote_compaction_headers(&model,key.as_deref(),&transformed,Some(session_id)).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?
                && let Some(origin) = crate::openai_remote_model::openai_remote_compaction_origin(&model,&headers) {
                let controller = maho_ai::utils::abort::AbortController::new();
                let signal = controller.signal();
                let runner:crate::openai_remote_dependencies::OpenAiResponsesStreamRunner = std::sync::Arc::new(|model,context,options|maho_ai::compat::stream_simple(model,context,Some(options.clone())));
                let client = reqwest::Client::new();
                let emit = |data|api.events.emit(crate::openai_remote::SENPI_COMPACTION_EVENT,&data);
                let remote = tokio::select! {
                    () = event.signal.cancelled() => {controller.abort(None);return Ok(EventResult::None);}
                    result = crate::openai_remote::run_remote_compaction(crate::openai_remote::RemoteCompactionOptions {
                        model:&model,request:&request,request_id:&event.request_id,first_kept_entry_id:&event.preparation.first_kept_entry_id,
                        system_prompt:&system_prompt,session_id,api_key:key,headers,extra_body:None,origin:serde_json::json!({"endpoint":origin.endpoint,"trustDomain":origin.trust_domain,"authTenantFingerprint":origin.auth_tenant_fingerprint}),
                        signal:&signal,timeout:std::time::Duration::from_secs(15),now_ms:chrono::Utc::now().timestamp_millis() as u64,client:&client,runner:&runner,provider_request:Some(&prepared),
                    },&emit) => result,
                };
                if event.signal.is_aborted() {return Ok(EventResult::None);}
                match remote {
                    Ok(Some(compaction)) => return Ok(EventResult::SessionBefore(SessionBeforeEventResult {compaction:Some(compaction),..Default::default()})),
                    Ok(None) => {},
                    Err(error) => return Err(maho_ext_api::ExtensionFailure::new(error)),
                }
            }
        }
    }
    let convert = |messages: &[maho_ext_api::AgentMessage]|messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()));
    let messages = convert(&event.preparation.messages_to_summarize)?;
    let prefix = convert(&event.preparation.turn_prefix_messages)?;
    let resolved = context.get_resolved_compaction_settings()?;
    let settings = resolved_settings(&event.preparation.settings, resolved.as_ref())?;
    let preparation = maho_core::compaction::compaction::CompactionPreparation {
        first_kept_entry_id: event.preparation.first_kept_entry_id.clone(), source_messages: messages.clone(), messages_to_summarize: messages,
        turn_prefix_source_messages: prefix.clone(), is_split_turn: !prefix.is_empty(), turn_prefix_messages: prefix,
        tokens_before: event.preparation.tokens_before as i64, previous_summary: event.preparation.previous_summary.clone(),
        file_ops: Default::default(), settings,
    };
    let reason = match event.reason { maho_ext_api::CompactionReason::Manual => "manual", maho_ext_api::CompactionReason::Threshold => "threshold", maho_ext_api::CompactionReason::Overflow => "overflow", maho_ext_api::CompactionReason::Branch => "branch", _ => "extension" };
    let snapshot = crate::speculative::SpeculativeCompactionSnapshot {
        generation: 0, expected_revision: context.get_message_revision()?, context_window: context.get_context_usage()?.map_or(model.context_window, |usage|usage.context_window), model,
        prompt_variant: crate::speculative::get_prompt_variant(reason, &preparation), preparation,
        branch_entries: crate::speculative::branch_values(context), custom_instructions: event.custom_instructions.clone(),
        system_prompt: Some(context.get_system_prompt()), tools: summarization_tools(api), origin: Some("core-route".into()),
    };
    let key = context.model_registry.get_api_key_for_provider(&snapshot.model.provider).await?;
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    let progress = |delta: &str| { let _ = context.update_compaction(maho_ext_api::UpdateCompactionOptions { reason: event.reason, signal: Some(event.signal.clone()), delta: Some(delta.into()), text: None }); };
    let result = tokio::select! {
        () = event.signal.cancelled() => { controller.abort(None); return Ok(EventResult::None); }
        result = crate::speculative::run_extension_compaction(&snapshot, key, None, Some(&signal), None, &progress) => result,
    };
    if event.signal.is_aborted() { return Ok(EventResult::None); }
    match result {
        Ok(Some(compaction)) => Ok(EventResult::SessionBefore(SessionBeforeEventResult { compaction: Some(compaction), ..Default::default() })),
        Ok(None) => Ok(EventResult::SessionBefore(SessionBeforeEventResult { cancel: Some(true), reason: Some("compaction generator returned no summary".into()), ..Default::default() })),
        Err(error) => {
            let failure = match &error {
                crate::speculative::SummaryGenerationError::Request(response) => crate::speculative::summary_request_failure(response),
                crate::speculative::SummaryGenerationError::Stream(crate::speculative_summary::SummaryStreamError::IdleTimeout) => crate::deterministic_fallback::SummaryFailure::Timeout,
                crate::speculative::SummaryGenerationError::Stream(crate::speculative_summary::SummaryStreamError::DurationBudget) | crate::speculative::SummaryGenerationError::TotalBudget => crate::deterministic_fallback::SummaryFailure::Timeout,
                crate::speculative::SummaryGenerationError::Overflow(_) => crate::deterministic_fallback::SummaryFailure::OverflowExhausted,
                crate::speculative::SummaryGenerationError::EmptySummary(_) => crate::deterministic_fallback::SummaryFailure::EmptySummary,
                _ => crate::deterministic_fallback::SummaryFailure::Other,
            };
            if is_required_compaction_fallback_reason(reason)
                && let Some(failure) = crate::deterministic_fallback::classify_required_compaction_fallback_failure(failure, &error.to_string()) {
                let mut diagnostics = crate::deterministic_fallback::DeterministicFallbackDiagnostic::default();
                let intent = crate::task_intent::resolve_inherited_task_intent(&snapshot.branch_entries);
                if let Some(compaction) = crate::deterministic_fallback::create_required_compaction_fallback(&snapshot.preparation, snapshot.context_window, failure, intent.as_deref(), &snapshot.branch_entries, &mut diagnostics) {
                    context.ui.notify(&crate::deterministic_fallback::format_required_compaction_fallback_notice(failure,Some(&error.to_string())),maho_ext_api::NotificationType::Warning);
                    return Ok(EventResult::SessionBefore(SessionBeforeEventResult { compaction: Some(compaction), ..Default::default() }));
                }
                return Ok(EventResult::SessionBefore(SessionBeforeEventResult {
                    cancel: Some(true), reason: Some(crate::deterministic_fallback::format_required_compaction_fallback_rejection(&diagnostics)), ..Default::default()
                }));
            }
            Ok(EventResult::SessionBefore(SessionBeforeEventResult { cancel: Some(true), reason: Some(error.to_string().replace("senpi:no-turn-retry:","")), ..Default::default() }))
        }
    }
}

pub async fn apply_live_blocking_compaction(
    api: &maho_ext_api::ExtensionApi,
    context: &maho_ext_api::ExtensionContext,
    generation: u64,
    instructions: String,
) -> Result<crate::speculative::SpeculativeCompactionResult, maho_ext_api::ExtensionFailure> {
    if context.model.as_ref().is_some_and(|model|matches!(model.provider.as_str(),"cursor" | "cursor-cli-oauth")) && !context.is_idle() {return Ok(crate::speculative::SpeculativeCompactionResult::Rejected);}
    let signal = context.begin_compaction(maho_ext_api::BeginCompactionOptions {reason:maho_ext_api::CompactionReason::Extension})?;
    let Some(mut snapshot) = crate::speculative::create_speculative_compaction_snapshot(context,generation,Some(instructions),summarization_tools(api))? else {
        end_compaction_feedback(context,signal,false,"unavailable",None)?;
        return Ok(crate::speculative::SpeculativeCompactionResult::Unavailable);
    };
    snapshot.origin = Some("blocking".into());
    let key = context.model_registry.get_api_key_for_provider(&snapshot.model.provider).await?;
    let controller = maho_ai::utils::abort::AbortController::new();
    let ai_signal = controller.signal();
    let progress = |delta: &str| {let _ = context.update_compaction(maho_ext_api::UpdateCompactionOptions {reason:maho_ext_api::CompactionReason::Extension,signal:signal.clone(),delta:Some(delta.into()),text:None});};
    let cancelled = async {match &signal {Some(signal)=>signal.cancelled().await,None=>std::future::pending::<()>().await}};
    let generated = tokio::select! {
        () = cancelled => {controller.abort(None);Ok(None)}
        result = crate::speculative::run_extension_compaction(&snapshot,key,None,Some(&ai_signal),None,&progress) => result,
    };
    match generated {
        Ok(compaction) => {
            let result = crate::speculative::apply_generated_compaction(context,Some(&snapshot),generation,compaction,signal.clone()).await?;
            let reason = match result {crate::speculative::SpeculativeCompactionResult::Applied=>"applied",crate::speculative::SpeculativeCompactionResult::Stale=>"stale",crate::speculative::SpeculativeCompactionResult::Rejected=>"rejected",crate::speculative::SpeculativeCompactionResult::Unavailable=>"unavailable",crate::speculative::SpeculativeCompactionResult::Failed=>"failed"};
            end_compaction_feedback(context,signal,result == crate::speculative::SpeculativeCompactionResult::Applied,reason,None)?;
            Ok(result)
        }
        Err(error) => {
            end_compaction_feedback(context,signal,false,"failed",Some(&error.to_string().replace("senpi:no-turn-retry:","")))?;
            Ok(crate::speculative::SpeculativeCompactionResult::Failed)
        }
    }
}
