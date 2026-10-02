use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use maho_ai::{model::Model, types::{AssistantMessageEvent as Event, ContentBlock, Context, DoneReason, ErrorReason, SimpleStreamOptions, StopReason, TextContent}, utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream}};
use serde_json::json;

pub struct StreamDeps {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub settings: crate::settings::ProviderSettings,
    pub store: Arc<dyn maho_ai::auth::types::CredentialStore>,
    pub refresh: Arc<dyn maho_ai::auth::types::OAuthAuth>,
    pub registry: Arc<tokio::sync::Mutex<crate::session_stream::SessionRegistry>>,
    pub watch: Arc<std::sync::Mutex<crate::tool_watch::ToolWatch>>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

pub fn stream_anthropic_subscription(model: Model, context: Context, options: Option<SimpleStreamOptions>, deps: StreamDeps) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let events = stream.clone();
    tokio::spawn(async move {
        let signal = options.as_ref().and_then(|options| options.stream.request.signal.clone());
        let mut mapper = crate::stream_events::StreamEventContext::new(model.clone(), (deps.now)(), BTreeMap::new());
        let request = options.as_ref().and_then(|options| options.stream.request.env.as_ref());
        let pool = crate::auth_lane::managed_pool(deps.store.as_ref(), &deps.settings, &deps.environment, request).await;
        let (mut pool, pool_error) = match pool { Ok(pool) => (pool, None), Err(error) => (None, Some(error)) };
        let mut pool_error = pool_error;
        let mut attempt = 0;
        let mut started = false;
        let outcome = loop {
        let mut query = None;
        let mut selected = None;
        let mut visible = false;
        let outcome: anyhow::Result<()> = async {
            if let Some(error) = pool_error.take() { return Err(error); }
            if let Some(signal) = &signal { signal.throw_if_aborted()?; }
            let (lane, environment, account) = match pool.as_ref() {
                Some(pool) => {
                    let session = options.as_ref().and_then(|options| options.stream.request.affinity_session_id.as_deref().or(options.stream.session_id.as_deref()));
                    let cli_pin = crate::account_command::session_pin(session);
                    let mut slot = crate::affinity::select_account(&pool.accounts, &crate::affinity::AffinityOptions { session_id: session, pinned_account: cli_pin.as_deref().or(pool.pinned_account.as_deref()), ..Default::default() }, (deps.now)() as f64)?;
                    selected = Some(slot.clone());
                    let refresh = deps.refresh.clone(); let access = slot.access.clone(); let expires = slot.expires;
                    pool.refresh_selected(deps.store.as_ref(), &mut slot, (deps.now)() as f64, signal.clone().unwrap_or_else(|| maho_ai::utils::abort::AbortController::new().signal()), move |token, signal| async move { refresh.refresh(&maho_ai::auth::types::OAuthCredential::new(access, token, expires), &signal).await }).await?;
                    (pool.lane, crate::auth_lane::prepared_environment(&pool.environment, pool.lane, &slot, &deps.agent_dir, (deps.now)() as u64)?, slot.name)
                },
                None => (crate::auth_lane::TokenInjection::Ambient, crate::auth_lane::ambient_environment(&deps.environment, request), "ambient".into()),
            };
            let model_json = serde_json::to_value(&model)?; let context_json = serde_json::to_value(&context)?;
            let resolved = crate::tools::resolve_tools(context_json["tools"].as_array().map(Vec::as_slice));
            mapper.custom_tool_name_to_pi = resolved.to_pi.clone();
            let tool_note = options.as_ref().and_then(|options| options.stream.session_id.as_deref()).and_then(|session| {
                let mut watch = deps.watch.lock().expect("tool watch");
                let messages = context_json["messages"].as_array().expect("messages");
                watch.reconcile(session, messages); watch.prompt_note(session, messages, &resolved.to_sdk)
            });
            let options_json = match &options {
                Some(options) => json!({"toolChoice":options.tool_choice,"reasoning":options.reasoning,"thinkingBudgets":options.thinking_budgets}),
                None => json!({}),
            };
            let mut configuration = crate::options::build_query_configuration(crate::options::QueryOptionsInput { model: &model_json, context: &context_json, stream_options: &options_json, settings: &deps.settings, lane, cwd: &deps.cwd, agent_dir: &deps.agent_dir, config_directory: ".maho", tools: Some(&resolved.sdk_tools), executable: deps.executable.to_str() })?;
            if options.as_ref().is_none_or(|options| options.tool_choice != Some(maho_ai::types::ToolChoice::None)) && !resolved.custom_tools.is_empty() { configuration["customTools"] = json!(resolved.custom_tools); }
            let resident = options.as_ref().filter(|options| options.stream.request.stream_kind == Some(maho_ai::types::StreamKind::Main) && deps.settings.values.get("resumeMode").and_then(serde_json::Value::as_str) != Some("off")).and_then(|options| options.stream.session_id.as_deref());
            if resident.is_none() && options.as_ref().is_some_and(|options| options.stream.request.stream_kind == Some(maho_ai::types::StreamKind::Main)) {
                mapper.output.diagnostics.get_or_insert_with(Vec::new).push(maho_ai::utils::diagnostics::AssistantMessageDiagnostic { kind: "claude_sdk_oauth_session_continuity".into(), timestamp: (deps.now)(), error: None, details: Some(json!({"kind":"disabled","reason":if deps.settings.values.get("resumeMode").and_then(serde_json::Value::as_str) == Some("off") {"resume_mode_off"} else {"registry_miss"}}).as_object().expect("details").clone()) });
            }
            if let Some(session) = resident {
                let lane_name = match lane { crate::auth_lane::TokenInjection::Ambient => "ambient", crate::auth_lane::TokenInjection::OAuthSlots => "oauth-slots", crate::auth_lane::TokenInjection::ConfigDir => "config-dir" };
                let mut registry = deps.registry.lock().await;
                let mut saw_stream = false;
                let result = registry.turn(crate::session_stream::ResidentInput { session, account: &account, model: &model.id, context: &context_json, options: &configuration, executable: &deps.executable, environment: &environment, auth_lane: lane_name, custom: &resolved.to_sdk, tool_note: tool_note.as_deref(), now: (deps.now)() as u64, transcript_available: true, signal: signal.as_ref() }, |message| {
                    if !started { events.push(Event::Start { partial: mapper.output.clone() }); started = true; }
                    if message["type"] == "stream_event" { saw_stream = true; visible |= matches!(message["event"]["type"].as_str(), Some("content_block_start" | "content_block_delta" | "content_block_stop")); if let Some(event) = mapper.apply_stream_event(&message["event"]) { events.push(event); } }
                    else if message["type"] == "result" {
                        crate::stream_protocol::update_usage(&model, &mut mapper.output, &message["usage"]);
                        if mapper.output.stop_reason != StopReason::ToolUse && !message["stop_reason"].is_null() { mapper.output.stop_reason = crate::stream_protocol::map_stop_reason(message["stop_reason"].as_str()); }
                        if !saw_stream { mapper.output.content.push(ContentBlock::Text(TextContent { text: message["result"].as_str().unwrap_or_default().into(), ..Default::default() })); }
                    }
                }).await;
                if let Some(generation) = registry.entries.get(session).filter(|entry| entry.evictable()).map(|entry| entry.pump.generation) {
                    if let Some(reaper) = registry.reapers.remove(session) { reaper.abort(); }
                    registry.reapers.insert(session.into(), crate::session_reaper::schedule(&deps.registry, session.into(), generation, deps.now.clone()));
                }
                if let Some(observation) = registry.last_decisions.get(session) {
                    mapper.output.diagnostics.get_or_insert_with(Vec::new).push(maho_ai::utils::diagnostics::AssistantMessageDiagnostic { kind: "claude_sdk_oauth_session_continuity".into(), timestamp: (deps.now)(), error: None, details: Some(json!({"kind":observation.kind,"reason":observation.reason,"deltaMessages":observation.delta_messages,"payloadBytes":observation.payload_bytes,"collapsedDirectives":observation.collapsed_directives}).as_object().expect("details").clone()) });
                }
                if let Err(error) = &result
                    && let Some(failure) = error.downcast_ref::<crate::errors::SdkResultFailure>()
                    && let Some(usage) = &failure.usage {
                    crate::stream_protocol::update_usage(&model, &mut mapper.output, usage);
                }
                let result = result?;
                if result.aborted { anyhow::bail!("Operation aborted"); }
                return Ok(());
            }
            let spawn = crate::sdk_boundary::SdkQueryHandle::spawn(&deps.executable, &configuration, &environment);
            let handle = match &signal {
                Some(signal) => maho_ai::utils::abort::race_with_abort_signal(spawn, signal).await??,
                None => spawn.await?,
            };
            query = Some(handle);
            let messages = context_json["messages"].as_array().ok_or_else(|| anyhow::anyhow!("Context messages are not an array"))?;
            let blocks = crate::prompt_bridge::build_prompt_blocks(messages, &resolved.to_sdk, tool_note.as_deref());
            let blocks = crate::prompt_directive_dedupe::dedupe_ultrawork_blocks(&blocks).blocks;
            let handle = query.as_mut().expect("query started");
            handle.send(crate::prompt_bridge::prompt_message(blocks)).await?;
            let mut saw_stream = false;
            loop {
                let message = match &signal {
                    Some(signal) => maho_ai::utils::abort::race_with_abort_signal(handle.next(), signal).await?,
                    None => handle.next().await,
                };
                let Some(message) = message else { anyhow::bail!("Anthropic Subscription query ended before the turn result"); }; let message = message?;
                if let Some(refusal) = crate::refusal::refusal_error(&message) { return Err(refusal.into()); }
                if message["type"] == "assistant" && let Some(failure) = crate::errors::sdk_assistant_failure(&message) { anyhow::bail!(failure); }
                if message["type"] == "result" && let Some(failure) = crate::errors::sdk_result_failure(&message) {
                    if let Some(usage) = &failure.usage { crate::stream_protocol::update_usage(&model, &mut mapper.output, usage); }
                    return Err(failure.into());
                }
                if !started { events.push(Event::Start { partial: mapper.output.clone() }); started = true; }
                if message["type"] == "stream_event" {
                    saw_stream = true;
                    visible |= matches!(message["event"]["type"].as_str(), Some("content_block_start" | "content_block_delta" | "content_block_stop"));
                    if let Some(event) = mapper.apply_stream_event(&message["event"]) { events.push(event); }
                } else if message["type"] == "result" && message["subtype"] == "success" {
                    crate::stream_protocol::update_usage(&model, &mut mapper.output, &message["usage"]);
                    if mapper.output.stop_reason != StopReason::ToolUse && !message["stop_reason"].is_null() { mapper.output.stop_reason = crate::stream_protocol::map_stop_reason(message["stop_reason"].as_str()); }
                    if !saw_stream { mapper.output.content.push(ContentBlock::Text(TextContent { text: message["result"].as_str().unwrap_or_default().into(), ..Default::default() })); }
                    break;
                }
            }
            Ok(())
        }.await;
        let cleanup = match query { Some(query) => query.close().await, None => Ok(()) };
        let outcome = outcome.and(cleanup);
        let Err(error) = outcome else { break Ok(()); };
        if signal.as_ref().is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { break Err(error); }
        let (Some(pool), Some(slot)) = (pool.as_mut(), selected) else { break Err(error); };
        let original = json!({"message":error.to_string()});
        let classification = crate::errors::classify_sdk_error(&original);
        if !classification.retryable { break Err(crate::failover::ClassifiedSdkError {classification,original,suppress_turn_retry:visible}.into()); }
        let blocked = crate::failover::blocked_account(&slot, classification, (deps.now)() as f64, attempt, crate::failover::DEFAULT_RATE_LIMIT_BLOCK_MS, &original);
        if let Err(error) = crate::failover::persist_block(deps.store.as_ref(), crate::auth_lane::PROVIDER_ID, blocked.clone()).await { break Err(error); }
        for account in &mut pool.accounts { if account.name == blocked.name { *account = blocked.clone(); } }
        crate::account_events::emit_provider_accounts_changed(crate::auth_lane::PROVIDER_ID);
        if visible { break Err(crate::failover::ClassifiedSdkError { classification, original, suppress_turn_retry:true }.into()); }
        attempt += 1;
        if attempt as usize >= pool.accounts.len() { break Err(error); }
        let session = options.as_ref().and_then(|options| options.stream.request.affinity_session_id.as_deref().or(options.stream.session_id.as_deref()));
        let cli_pin = crate::account_command::session_pin(session);
        let next = match crate::affinity::select_account(&pool.accounts, &crate::affinity::AffinityOptions { session_id:session, pinned_account:cli_pin.as_deref().or(pool.pinned_account.as_deref()), ..Default::default() }, (deps.now)() as f64) { Ok(next) => next, Err(error) => break Err(error.into()) };
        crate::account_events::emit_provider_account_failover(crate::auth_lane::PROVIDER_ID, &slot.name, &next.name, classification.kind);
        };
        match outcome {
            Ok(()) => events.push(Event::Done { reason: match mapper.output.stop_reason { StopReason::ToolUse => DoneReason::ToolUse, StopReason::Length => DoneReason::Length, _ => DoneReason::Stop }, message: mapper.output }),
            Err(error) => { let aborted = signal.as_ref().is_some_and(maho_ai::utils::abort::AbortSignal::aborted); mapper.output.stop_reason = if aborted { StopReason::Aborted } else { StopReason::Error }; mapper.output.error_message = Some(crate::stream_guidance::with_auth_guidance(&error)); events.push(Event::Error { reason: if aborted { ErrorReason::Aborted } else { ErrorReason::Error }, error: mapper.output }); },
        }
        events.end(None);
    });
    stream
}
