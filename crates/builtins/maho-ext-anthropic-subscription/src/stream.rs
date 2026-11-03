use maho_ai::{
    model::Model,
    types::{
        AssistantMessageEvent as Event, ContentBlock, Context, DoneReason, ErrorReason,
        SimpleStreamOptions, StopReason, TextContent,
    },
    utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream},
};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

pub const NATIVE_COMPACTION_WHILE_SENPI_OWNS: &str = "Claude Code compacted this session natively although senpi owns compaction on this lane (compactionOwner: \"senpi\"); the turn was stopped so only one side compacts. Run /compact, or set anthropicSubscriptionProvider.compactionOwner to \"sdk\" to let Claude Code compact.";

fn lane_name(lane: crate::auth_lane::TokenInjection) -> &'static str {
    match lane {
        crate::auth_lane::TokenInjection::Ambient => "ambient",
        crate::auth_lane::TokenInjection::OAuthSlots => "oauth-slots",
        crate::auth_lane::TokenInjection::ConfigDir => "config-dir",
    }
}

/// The auth-block recovery `reload`: the stored slots, so recovery sees what it wrote.
fn stored_slots_reload<'a>(
    pool: &'a crate::auth_lane::ManagedPool,
    store: &'a dyn maho_ai::auth::types::CredentialStore,
) -> Box<
    dyn Fn() -> crate::auth_block_recovery::BorrowedFuture<'a, Vec<crate::accounts::AccountSlot>>
        + Send
        + Sync
        + 'a,
> {
    Box::new(
        move || -> crate::auth_block_recovery::BorrowedFuture<'a, Vec<crate::accounts::AccountSlot>> {
            Box::pin(pool.reload_accounts(store))
        },
    )
}

/// senpi `recoverAuthBlockedSlots` before the turn's first attempt: an auth-blocked account due for its re-probe is
/// refreshed first, so the probe sends fresh material.
async fn recover_auth_blocks(
    pool: &mut crate::auth_lane::ManagedPool,
    deps: &StreamDeps,
    signal: maho_ai::utils::abort::AbortSignal,
    model: &str,
) -> anyhow::Result<()> {
    let flow = deps.refresh.clone();
    let refresher: crate::auth_block_recovery::SlotRefresher = Arc::new(
        move |refresh: String,
              signal: maho_ai::utils::abort::AbortSignal|
              -> crate::auth_block_recovery::RecoveryFuture<
            maho_ai::auth::types::OAuthCredential,
        > {
            let flow = flow.clone();
            Box::pin(async move {
                flow.refresh(
                    &maho_ai::auth::types::OAuthCredential::new(String::new(), refresh, 0.0),
                    &signal,
                )
                .await
            })
        },
    );
    let now = (deps.now)() as f64;
    let model = model.to_owned();
    let pool_ref = &*pool;
    let store = deps.store.as_ref();
    let accounts = crate::auth_block_recovery::recover_auth_blocked_slots(
        crate::auth_block_recovery::AuthBlockRecoveryInput {
            store,
            provider_id: crate::auth_lane::PROVIDER_ID,
            accounts: pool.accounts.clone(),
            refresher,
            signal,
            is_grant_rejected: Arc::new(crate::auth_lane::is_grant_rejected),
            reload: stored_slots_reload(pool_ref, store),
            is_usable: Box::new(move |account: &crate::accounts::AccountSlot| {
                !crate::affinity::is_blocked_for(account, now, Some(&model))
            }),
            now,
            transient_error: Box::new(|error: anyhow::Error| {
                if crate::auth_lane::is_store_busy(&error) {
                    error
                } else {
                    crate::auth_lane::refresh_failure(&error)
                }
            }),
        },
    )
    .await?;
    pool.accounts = accounts;
    Ok(())
}

pub struct StreamDeps {
    pub executable: PathBuf,
    pub source: Option<crate::executable::ExecutableSource>,
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

/// A turn that fails before any SDK work (senpi `stream.ts` catch path): one error event carrying
/// `message`, then the end of the stream.
pub fn failed_stream(model: &Model, message: &str) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let mut output = crate::stream_events::StreamEventContext::new(
        model.clone(),
        maho_ai::utils::diagnostics::now_ms(),
        BTreeMap::new(),
    )
    .output;
    output.stop_reason = StopReason::Error;
    output.error_message = Some(message.to_owned());
    stream.push(Event::Error {
        reason: ErrorReason::Error,
        error: output,
    });
    stream.end(None);
    stream
}

pub fn stream_anthropic_subscription(
    model: Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
    deps: StreamDeps,
) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let events = stream.clone();
    tokio::spawn(async move {
        let signal = options
            .as_ref()
            .and_then(|options| options.stream.request.signal.clone());
        let mut mapper = crate::stream_events::StreamEventContext::new(
            model.clone(),
            (deps.now)(),
            BTreeMap::new(),
        );
        let request = options
            .as_ref()
            .and_then(|options| options.stream.request.env.as_ref());
        let pool = crate::auth_lane::managed_pool(
            deps.store.as_ref(),
            &deps.settings,
            &deps.environment,
            request,
        )
        .await;
        let (mut pool, pool_error) = match pool {
            Ok(pool) => (pool, None),
            Err(error) => (None, Some(error)),
        };
        let mut pool_error = pool_error;
        let session = options.as_ref().and_then(|options| {
            options
                .stream
                .request
                .affinity_session_id
                .as_deref()
                .or(options.stream.session_id.as_deref())
        });
        let account_fallback = crate::settings::account_fallback(&deps.settings);
        // senpi `residentAuthLaneMessages`: the account this session's SDK transcript lives under is preferred while
        // it can serve (senpi#2891). Only the resident lane passes it; a one-shot turn has no transcript to follow.
        let resident_lane = options.as_ref().is_some_and(|options| {
            options.stream.request.stream_kind == Some(maho_ai::types::StreamKind::Main)
        }) && deps
            .settings
            .values
            .get("resumeMode")
            .and_then(serde_json::Value::as_str)
            != Some("off");
        let preferred_account = match options
            .as_ref()
            .and_then(|options| options.stream.session_id.as_deref())
            .filter(|_| resident_lane)
        {
            Some(session) => {
                let registry = deps.registry.lock().await;
                registry
                    .entries
                    .get(session)
                    .map(|entry| entry.account_name.clone())
                    .or_else(|| {
                        registry
                            .bindings
                            .get(session)
                            .map(|binding| binding.account_name)
                    })
            }
            None => None,
        };
        if pool_error.is_none()
            && let Some(pool) = pool.as_mut()
            && let Err(error) = recover_auth_blocks(
                pool,
                &deps,
                signal
                    .clone()
                    .unwrap_or_else(|| maho_ai::utils::abort::AbortController::new().signal()),
                &model.id,
            )
            .await
        {
            pool_error = Some(error);
        }
        let mut failover = pool.as_ref().map(|pool| {
            crate::failover::FailoverTurn::new(
                &pool.accounts,
                crate::failover::FailoverTurnOptions {
                    store: deps.store.as_ref(),
                    provider: crate::auth_lane::PROVIDER_ID,
                    model: Some(&model.id),
                    session_key: session,
                    account_fallback: Some(account_fallback),
                    base_block_ms: crate::failover::DEFAULT_RATE_LIMIT_BLOCK_MS,
                    now: (deps.now)() as f64,
                },
            )
        });
        let mut retry_account = None;
        let mut started = false;
        let mut asked_legacy_long_context = false;
        let mut long_context_retried = false;
        let mut cold_seed_attempt = false;
        let mut cold_seed_estimate = None;
        let mut native_compaction_stopped = false;
        let senpi_owns_compaction =
            crate::settings::resolve_compaction_owner(&deps.settings) == "senpi";
        let context_window =
            crate::context_window::planned_context_window_now(&model.id, model.context_window);
        let mut started_at = 0.0;
        // account-context.ts: the account of the attempt that produced the turn's error, never another attempt's.
        let mut attempt_account: Option<crate::account_context::AccountContext>;
        let outcome = loop {
            let mut query = None;
            let mut selected = None;
            let mut visible = false;
            attempt_account = None;
            let outcome: anyhow::Result<()> = async {
            if let Some(error) = pool_error.take() { return Err(error); }
            crate::session_cwd::require_anthropic_subscription_cwd(Some(&deps.cwd))?;
            if let Some(signal) = &signal { signal.throw_if_aborted()?; }
            started_at = (deps.now)() as f64;
            let (lane, environment, account, credential_digest) = match pool.as_ref() {
                Some(pool) => {
                    let accounts = &failover.as_ref().expect("managed failover").accounts;
                    let cli_pin = crate::account_command::session_pin(session);
                    let rotation_consented = maho_core::provider_consent::rotation_consented_in_agent_dir(
                        &deps.agent_dir.to_string_lossy(),
                        crate::auth_lane::PROVIDER_ID,
                    );
                    let mut slot = if let Some(name) = retry_account.take() {
                        accounts.iter().find(|slot| slot.name == name).cloned().ok_or_else(|| anyhow::anyhow!("retry account disappeared"))?
                    } else if accounts.len() > 1 && !rotation_consented {
                        let pin = cli_pin.as_deref().or(pool.pinned_account.as_deref());
                        pin.and_then(|name| accounts.iter().find(|s| s.name == name))
                            .or_else(|| accounts.first())
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!("no accounts available"))?
                    } else {
                        crate::affinity::select_account(accounts, &crate::affinity::AffinityOptions { session_id: session, pinned_account: cli_pin.as_deref().or(pool.pinned_account.as_deref()), pin_source: if cli_pin.is_some() { Some(crate::affinity::PinSource::Cli) } else { pool.pin_source }, account_fallback: Some(account_fallback), preferred_account: preferred_account.as_deref(), account_order: pool.account_order.as_deref(), model: Some(&model.id), ..Default::default() }, (deps.now)() as f64)?
                    };
                    selected = Some(slot.clone());
                    pool.refresh_for_attempt(deps.store.as_ref(), &mut slot, (deps.now)() as f64, signal.clone().unwrap_or_else(|| maho_ai::utils::abort::AbortController::new().signal()), deps.refresh.clone()).await?;
                    // The failover verdict compares the token this attempt sent, so it carries the refreshed material.
                    selected = Some(slot.clone());
                    let prepared = crate::auth_lane::prepared_environment(&pool.environment, pool.lane, &slot, &deps.agent_dir, (deps.now)() as u64)?;
                    // auth-lane.ts: `pinSource: input.pinnedAccount !== undefined ? "cli" : pool.pinSource`.
                    let attempt = crate::account_context::AttemptAccount { account_name: &slot.name, accounts, auth_lane: pool.lane, pin_source: if cli_pin.is_some() { Some(crate::affinity::PinSource::Cli) } else { pool.pin_source } };
                    attempt_account = Some(crate::account_context::attempt_account_context(&attempt));
                    crate::account_context::log_attempt_account(&attempt, options.as_ref().and_then(|options| options.stream.session_id.as_deref()), &deps.agent_dir);
                    (pool.lane, prepared.env, slot.name, Some(prepared.credential_digest))
                },
                None => {
                    let attempt = crate::account_context::AttemptAccount { account_name: "ambient", accounts: &[], auth_lane: crate::auth_lane::TokenInjection::Ambient, pin_source: None };
                    attempt_account = Some(crate::account_context::attempt_account_context(&attempt));
                    crate::account_context::log_attempt_account(&attempt, options.as_ref().and_then(|options| options.stream.session_id.as_deref()), &deps.agent_dir);
                    (crate::auth_lane::TokenInjection::Ambient, crate::auth_lane::ambient_environment(&deps.environment, request), "ambient".into(), None)
                },
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
            if let Some(session) = options.as_ref().and_then(|options|options.stream.session_id.as_deref()) {
                let mode=crate::settings::resolve_prompt_mode(&deps.settings);
                if let Some(guidance)=crate::guidance::PRESET_APPEND_DEPRECATION.lock().expect("prompt guidance").guidance(&mode.mode,mode.conflict,session) {
                    mapper.output.diagnostics.get_or_insert_with(Vec::new).push(maho_ai::utils::diagnostics::create_assistant_message_diagnostic("claude_sdk_oauth_deprecation",&maho_ai::utils::diagnostics::Thrown::Value(json!(guidance)),None));
                }
            }
            let mut configuration = crate::options::build_query_configuration(crate::options::QueryOptionsInput { model: &model_json, context: &context_json, stream_options: &options_json, settings: &deps.settings, lane, cwd: &deps.cwd, agent_dir: &deps.agent_dir, config_directory: ".omo", tools: Some(&resolved.sdk_tools), executable: deps.executable.to_str() })?;
            asked_legacy_long_context = crate::context_window::asks_legacy_long_context(configuration["model"].as_str());
            if options.as_ref().is_none_or(|options| options.tool_choice != Some(maho_ai::types::ToolChoice::None)) && !resolved.custom_tools.is_empty() { configuration["customTools"] = json!(resolved.custom_tools); }
            let resident = options.as_ref().filter(|_| resident_lane).and_then(|options| options.stream.session_id.as_deref());
            if resident.is_none() && options.as_ref().is_some_and(|options| options.stream.request.stream_kind == Some(maho_ai::types::StreamKind::Main)) {
                mapper.output.diagnostics.get_or_insert_with(Vec::new).push(maho_ai::utils::diagnostics::AssistantMessageDiagnostic { kind: "claude_sdk_oauth_session_continuity".into(), timestamp: (deps.now)(), error: None, details: Some({ let off = deps.settings.values.get("resumeMode").and_then(serde_json::Value::as_str) == Some("off"); let mut details = json!({"kind":"disabled","reason":if off {"resume_mode_off"} else {"registry_miss"}}); if off { details["settingSource"] = json!(deps.settings.resume_mode_source.unwrap_or("global")); } details }.as_object().expect("details").clone()) });
            }
            if let Some(session) = resident {
                let lane_name = lane_name(lane);
                let mut registry = deps.registry.lock().await;
                let mut saw_stream = false;
                // senpi checks `compact_boundary` in the loop both lanes share; the resident turn delivers through a
                // callback, so the boundary is recorded here and the turn is stopped once the registry returns it.
                let mut native_compaction = false;
                let result = registry.turn(crate::session_stream::ResidentInput { session, account: &account, model: &model.id, context: &context_json, options: &configuration, executable: &deps.executable, environment: &environment, auth_lane: lane_name, custom: &resolved.to_sdk, tool_note: tool_note.as_deref(), now: (deps.now)() as u64, transcript_available: true, signal: signal.as_ref(), credential_digest: credential_digest.as_deref(), context_window }, |message| {
                    crate::auth_attempt::record_limit_windows(&message, &account);
                    if native_compaction { return; }
                    if senpi_owns_compaction && message["type"] == "system" && message["subtype"] == "compact_boundary" { native_compaction = true; return; }
                    if !started { events.push(Event::Start { partial: mapper.output.clone() }); started = true; }
                    if message["type"] == "stream_event" { saw_stream = true; visible |= matches!(message["event"]["type"].as_str(), Some("content_block_start" | "content_block_delta" | "content_block_stop")); if let Some(event) = mapper.apply_stream_event(&message["event"]) { events.push(event); } }
                    else if message["type"] == "assistant" { mapper.note_response_id(message["message"]["id"].as_str()); if !saw_stream && message["message"]["usage"].is_object() { crate::stream_protocol::update_usage(&model, &mut mapper.output, &message["message"]["usage"], crate::stream_protocol::UsageScope::RequestStart); } }
                    else if message["type"] == "result" {
                        crate::stream_protocol::update_usage(&model, &mut mapper.output, &message["usage"], crate::stream_protocol::UsageScope::Query);
                        if mapper.output.stop_reason != StopReason::ToolUse && !message["stop_reason"].is_null() { mapper.output.stop_reason = crate::stream_protocol::map_stop_reason(message["stop_reason"].as_str()); }
                        if !saw_stream { mapper.output.content.push(ContentBlock::Text(TextContent { text: message["result"].as_str().unwrap_or_default().into(), ..Default::default() })); }
                    }
                }).await;
                if native_compaction {
                    // Nothing after the boundary reached the turn. Close the process that compacted, so the next turn
                    // rebuilds the session from senpi's history instead of resuming the compacted transcript.
                    native_compaction_stopped = true;
                    if let Err(error) = registry.close(session).await {
                        maho_ext_api::diagnostic(&format!("Anthropic Subscription could not close the natively compacted session: {error}"));
                    }
                    anyhow::bail!(NATIVE_COMPACTION_WHILE_SENPI_OWNS);
                }
                if let Some(shape) = registry.dispatch_shapes.get(session) {
                    cold_seed_attempt = shape.cold_seed;
                    cold_seed_estimate = shape.estimated_tokens;
                }
                if let Some((rejected_index, recovered_index)) = registry.checkpoint_recoveries.remove(session) {
                    maho_ext_api::diagnostic(&format!("claude_sdk_oauth_checkpoint_recovered {}", json!({"rejectedIndex":rejected_index,"recoveredIndex":recovered_index})));
                }
                if let Some(generation) = registry.entries.get(session).filter(|entry| entry.evictable()).map(|entry| entry.pump.generation) {
                    if let Some(reaper) = registry.reapers.remove(session) { reaper.abort(); }
                    registry.reapers.insert(session.into(), crate::session_reaper::schedule(&deps.registry, session.into(), generation, deps.now.clone()));
                }
                if let Some(observation) = registry.last_decisions.get(session) {
                    mapper.output.diagnostics.get_or_insert_with(Vec::new).push(maho_ai::utils::diagnostics::AssistantMessageDiagnostic { kind: "claude_sdk_oauth_session_continuity".into(), timestamp: (deps.now)(), error: None, details: Some(serde_json::to_value(observation).expect("observation serialization").as_object().expect("details").clone()) });
                }
                if let Err(error) = &result
                    && let Some(failure) = error.downcast_ref::<crate::errors::SdkResultFailure>()
                    && let Some(usage) = &failure.usage {
                    crate::stream_protocol::update_usage(&model, &mut mapper.output, usage, crate::stream_protocol::UsageScope::Query);
                }
                let result = result?;
                if result.aborted { anyhow::bail!("Operation aborted"); }
                return Ok(());
            }
            // The one-shot prompt is built after this, for the lane this attempt resolved (senpi#2982).
            let one_shot_cache_ttl = {
                let mut host = deps.environment.clone();
                if let Some(request) = request { host.extend(request.iter().map(|(key, value)| (key.clone(), value.clone()))); }
                crate::prompt_cache_ttl::pin_one_shot_prompt_cache_ttl(configuration.as_object_mut().expect("query options"), lane, &host)
            };
            let spawn = crate::sdk_boundary::SdkQueryHandle::spawn(&deps.executable, &configuration, &environment);
            let handle = match &signal {
                Some(signal) => maho_ai::utils::abort::race_with_abort_signal(spawn, signal).await??,
                None => spawn.await?,
            };
            query = Some(handle);
            let messages = context_json["messages"].as_array().ok_or_else(|| anyhow::anyhow!("Context messages are not an array"))?;
            let blocks = crate::prompt_bridge::build_prompt_blocks_with(messages, &resolved.to_sdk, tool_note.as_deref(), one_shot_cache_ttl);
            let blocks = crate::prompt_directive_dedupe::dedupe_registered_directive_blocks(&blocks).blocks;
            let handle = query.as_mut().expect("query started");
            handle.send(crate::prompt_bridge::prompt_message(blocks)).await?;
            let mut saw_stream = false;
            loop {
                let message = match &signal {
                    Some(signal) => maho_ai::utils::abort::race_with_abort_signal(handle.next(), signal).await?,
                    None => handle.next().await,
                };
                let Some(message) = message else { anyhow::bail!("Anthropic Subscription query ended before the turn result"); }; let message = message?;
                crate::auth_attempt::record_limit_windows(&message, &account);
                if let Some(refusal) = crate::refusal::refusal_error(&message) { return Err(refusal.into()); }
                if message["type"] == "assistant" && let Some(failure) = crate::errors::sdk_assistant_failure(&message) { anyhow::bail!(failure); }
                if message["type"] == "result" && let Some(failure) = crate::errors::sdk_result_failure(&message) {
                    if let Some(usage) = &failure.usage { crate::stream_protocol::update_usage(&model, &mut mapper.output, usage, crate::stream_protocol::UsageScope::Query); }
                    return Err(failure.into());
                }
                if !started { events.push(Event::Start { partial: mapper.output.clone() }); started = true; }
                if message["type"] == "stream_event" {
                    saw_stream = true;
                    visible |= matches!(message["event"]["type"].as_str(), Some("content_block_start" | "content_block_delta" | "content_block_stop"));
                    if let Some(event) = mapper.apply_stream_event(&message["event"]) { events.push(event); }
                } else if message["type"] == "assistant" {
                    mapper.note_response_id(message["message"]["id"].as_str());
                    if !saw_stream && message["message"]["usage"].is_object() { crate::stream_protocol::update_usage(&model, &mut mapper.output, &message["message"]["usage"], crate::stream_protocol::UsageScope::RequestStart); }
                } else if message["type"] == "system" && message["subtype"] == "compact_boundary" && senpi_owns_compaction {
                    // The query was started with native auto-compact off: a boundary here means Claude Code compacted
                    // anyway. Fail the turn loudly instead of letting a second owner rewrite the transcript.
                    native_compaction_stopped = true;
                    anyhow::bail!(NATIVE_COMPACTION_WHILE_SENPI_OWNS);
                } else if message["type"] == "result" && message["subtype"] == "success" {
                    crate::stream_protocol::update_usage(&model, &mut mapper.output, &message["usage"], crate::stream_protocol::UsageScope::Query);
                    if mapper.output.stop_reason != StopReason::ToolUse && !message["stop_reason"].is_null() { mapper.output.stop_reason = crate::stream_protocol::map_stop_reason(message["stop_reason"].as_str()); }
                    if !saw_stream { mapper.output.content.push(ContentBlock::Text(TextContent { text: message["result"].as_str().unwrap_or_default().into(), ..Default::default() })); }
                    break;
                }
            }
            Ok(())
        }.await;
            let cleanup = match query {
                Some(query) => query.close().await,
                None => Ok(()),
            };
            let outcome = outcome.and(cleanup);
            let Err(error) = outcome else {
                if let (Some(failover), Some(slot)) = (failover.as_ref(), selected.as_ref()) {
                    failover.on_success(&slot.name, started_at).await;
                }
                break Ok(());
            };
            if signal
                .as_ref()
                .is_some_and(maho_ai::utils::abort::AbortSignal::aborted)
                || native_compaction_stopped
            {
                break Err(error);
            }
            let original = json!({"message":error.to_string()});
            let classification = crate::long_context_retry::classify_for_request(
                &original,
                asked_legacy_long_context,
            );
            let (Some(pool), Some(failover), Some(slot)) =
                (pool.as_mut(), failover.as_mut(), selected)
            else {
                // Ambient turns have no failover to absorb a refused 1M window: record it and retry once at the
                // standard window.
                if classification.kind == crate::errors::SdkErrorKind::LongContextUnavailable
                    && !visible
                    && !long_context_retried
                {
                    long_context_retried = true;
                    crate::long_context::mark_long_context_refused();
                    continue;
                }
                if classification.kind == crate::errors::SdkErrorKind::LongContextUnavailable {
                    break Err(crate::failover::ClassifiedSdkError {
                        classification,
                        original,
                        suppress_turn_retry: visible,
                    }
                    .into());
                }
                break Err(error);
            };
            let step = match failover
                .on_failure(
                    &slot,
                    original,
                    classification,
                    visible,
                    (deps.now)() as f64,
                )
                .await
            {
                Ok(step) => step,
                Err(error) => break Err(error),
            };
            pool.accounts.clone_from(&failover.accounts);
            let (blocked, classification, stop) = match step {
                crate::failover::FailureStep::RetrySame {
                    account,
                    delay_ms,
                    long_context_refused,
                } => {
                    if long_context_refused {
                        crate::long_context::mark_long_context_refused();
                    }
                    if let Some(delay_ms) = delay_ms
                        && !crate::failover::retry_delay(delay_ms, signal.as_ref(), None).await
                    {
                        break Err(failover.last_error());
                    }
                    retry_account = Some(account);
                    continue;
                }
                crate::failover::FailureStep::Reselect => {
                    retry_account = Some(slot.name.clone());
                    continue;
                }
                crate::failover::FailureStep::Rotate {
                    blocked,
                    classification,
                } => (blocked, classification, None),
                crate::failover::FailureStep::Stop {
                    blocked,
                    classification,
                    error,
                } => (blocked, classification, Some(error)),
            };
            crate::account_events::emit_provider_accounts_changed(crate::auth_lane::PROVIDER_ID);
            if let Some(error) = stop {
                break Err(error.into());
            }
            // SAFETY-DIVERGENCE(packages/claude-sdk-oauth/src/stream.ts): upstream rotates across accounts on rate limits without consent; mhc gates automatic rotation behind providerRiskConsent.<provider>:rotation.
            let rotation_consented = maho_core::provider_consent::rotation_consented_in_agent_dir(
                &deps.agent_dir.to_string_lossy(),
                crate::auth_lane::PROVIDER_ID,
            );
            if failover.accounts.len() > 1 && !rotation_consented {
                if classification.kind == crate::errors::SdkErrorKind::RateLimit {
                    let notice = maho_core::provider_consent::append_rotation_notice(
                        &error.to_string(),
                        crate::auth_lane::PROVIDER_ID,
                        failover.accounts.len(),
                    );
                    break Err(anyhow::anyhow!("{notice}"));
                }
                break Err(error);
            }
            let cli_pin = crate::account_command::session_pin(session);
            let next = match crate::affinity::select_account(
                &failover.accounts,
                &crate::affinity::AffinityOptions {
                    session_id: session,
                    pinned_account: cli_pin.as_deref().or(pool.pinned_account.as_deref()),
                    pin_source: if cli_pin.is_some() {
                        Some(crate::affinity::PinSource::Cli)
                    } else {
                        pool.pin_source
                    },
                    account_fallback: Some(account_fallback),
                    preferred_account: preferred_account.as_deref(),
                    account_order: pool.account_order.as_deref(),
                    model: Some(&model.id),
                    ..Default::default()
                },
                (deps.now)() as f64,
            ) {
                Ok(next) => next,
                Err(error) => break Err(error.into()),
            };
            retry_account = Some(next.name.clone());
            crate::account_events::emit_provider_account_failover(
                crate::auth_lane::PROVIDER_ID,
                &blocked.name,
                &next.name,
                classification.kind,
            );
        };
        match outcome {
            Ok(()) => events.push(Event::Done {
                reason: match mapper.output.stop_reason {
                    StopReason::ToolUse => DoneReason::ToolUse,
                    StopReason::Length => DoneReason::Length,
                    _ => DoneReason::Stop,
                },
                message: mapper.output,
            }),
            Err(error) => {
                // Unwinding the stopped attempt kept a retry checkpoint on the Claude Code session that just compacted
                // natively. Drop it, so the next turn rebuilds the resident session from senpi's own history.
                if native_compaction_stopped
                    && let Some(session) = options
                        .as_ref()
                        .and_then(|options| options.stream.session_id.as_deref())
                {
                    deps.registry.lock().await.bindings.forget(session);
                }
                let aborted = signal
                    .as_ref()
                    .is_some_and(maho_ai::utils::abort::AbortSignal::aborted);
                mapper.output.stop_reason = if aborted {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                let mut message = crate::stream_guidance::with_account_auth_guidance(
                    &error,
                    attempt_account.as_ref(),
                );
                if error
                    .downcast_ref::<crate::affinity::AllAccountsBlockedError>()
                    .is_none()
                    && let Some(guidance) = crate::guidance::version_floor_guidance(
                        &message,
                        deps.source,
                        &deps.executable.to_string_lossy(),
                    )
                {
                    message.push('\n');
                    message.push_str(&guidance);
                }
                mapper.output.error_message = Some(message);
                crate::cold_seed_budget::mark_cold_seed_overflow(
                    &mut mapper.output,
                    context_window,
                    cold_seed_attempt,
                    cold_seed_estimate,
                    options
                        .as_ref()
                        .and_then(|options| options.stream.session_id.as_deref()),
                );
                events.push(Event::Error {
                    reason: if aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error: mapper.output,
                });
            }
        }
        events.end(None);
    });
    stream
}
