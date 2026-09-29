pub mod message_inspection_error;
pub mod pending_tool_turn;
pub mod prompt_message_state;
pub mod queue;
pub mod recent_dispatches;
pub mod reservations;
pub mod route_resolver;
pub mod semantic_dedupe;
pub mod session_idle_dispatch;
pub mod timing;
pub mod types;

use std::sync::Arc;

use serde_json::Value;

use crate::logger::log;
use crate::session_idle_settle::DEFAULT_SESSION_IDLE_SETTLE_MS;

pub use message_inspection_error::*;
pub use pending_tool_turn::*;
pub use prompt_message_state::*;
pub use queue::*;
pub use recent_dispatches::*;
pub use reservations::*;
pub use route_resolver::*;
pub use semantic_dedupe::*;
pub use session_idle_dispatch::*;
pub use timing::*;
pub use types::*;

fn has_object_session_path(input: &Value) -> Option<&str> {
    input
        .get("path")
        .and_then(Value::as_object)
        .and_then(|path_obj| path_obj.get("id"))
        .and_then(Value::as_str)
}

fn is_object_path_type_error(error_message: &str) -> bool {
    error_message.contains("The \"path\" property must be of type string")
        && (error_message.contains("got object") || error_message.contains("got undefined"))
}

async fn dispatch_with_path_compatibility<F, Fut>(
    dispatch: F,
    input: &Value,
) -> Result<Value, String>
where
    F: Fn(&Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    match dispatch(input).await {
        Ok(val) => Ok(val),
        Err(err) => {
            if !is_object_path_type_error(&err) {
                return Err(err);
            }
            let Some(id) = has_object_session_path(input) else {
                return Err(err);
            };
            let mut retry_input = input.clone();
            if let Some(map) = retry_input.as_object_mut() {
                map.insert("path".to_string(), Value::String(id.to_string()));
            }
            dispatch(&retry_input).await
        }
    }
}

pub async fn dispatch_internal_prompt<C>(
    args: InternalPromptDispatchArgs<C>,
) -> InternalPromptDispatchResult
where
    C: PromptGateClient + 'static,
{
    let clock = args.clock.unwrap_or_else(current_clock);
    let dedupe_key = args
        .dedupe_key
        .unwrap_or_else(|| create_semantic_prompt_dedupe_key(&args.input));
    let queue_retry_ms = args.queue_retry_ms.unwrap_or(DEFAULT_PROMPT_QUEUE_RETRY_MS);
    let post_dispatch_hold_ms = args
        .post_dispatch_hold_ms
        .unwrap_or(DEFAULT_PROMPT_ASYNC_POST_DISPATCH_HOLD_MS);
    let semantic_dedupe_hold_ms =
        args.semantic_dedupe_hold_ms
            .unwrap_or(if post_dispatch_hold_ms > 0 {
                DEFAULT_PROMPT_SEMANTIC_DEDUPE_HOLD_MS
            } else {
                0
            });
    let dispatch_timeout_ms = args
        .dispatch_timeout_ms
        .unwrap_or(DEFAULT_PROMPT_DISPATCH_TIMEOUT_MS);
    let settle_ms = args.settle_ms.unwrap_or(DEFAULT_SESSION_IDLE_SETTLE_MS);
    let session_name = match args.mode {
        InternalPromptDispatchMode::Async => PromptSessionName::PromptAsync,
        InternalPromptDispatchMode::Sync => PromptSessionName::Prompt,
    };

    let resolved = match try_resolve_dispatch_client_sync(&args.session_id) {
        Some(res) => res,
        None => resolve_dispatch_client(&args.session_id).await,
    };

    let client_arc = Arc::new(args.client);

    let has_target_method = match args.mode {
        InternalPromptDispatchMode::Async => {
            if resolved.route == PromptDispatchRoute::Live {
                resolved
                    .live_client
                    .as_ref()
                    .is_some_and(|c| c.prompt_async.is_some())
                    || client_arc.has_prompt_async()
            } else {
                client_arc.has_prompt_async()
            }
        }
        InternalPromptDispatchMode::Sync => {
            if resolved.route == PromptDispatchRoute::Live {
                resolved
                    .live_client
                    .as_ref()
                    .is_some_and(|c| c.prompt.is_some())
                    || client_arc.has_prompt()
            } else {
                client_arc.has_prompt()
            }
        }
    };

    if !has_target_method {
        log(
            &format!("[prompt-async-gate] {} unavailable", session_name.as_str()),
            Some(&serde_json::json!({
                "sessionID": &args.session_id,
                "source": &args.source,
            })),
        );
        return InternalPromptDispatchResult::Unavailable;
    }

    if resolved.reason == PromptDispatchRouteReason::Unavailable {
        log(
            LIVE_ROUTE_UNAVAILABLE_LOG,
            Some(&serde_json::json!({
                "sessionID": &args.session_id,
                "source": &args.source,
            })),
        );
    }

    let is_live_route = resolved.route == PromptDispatchRoute::Live;
    let live_client_opt = resolved.live_client.clone();
    let original_client = client_arc.clone();
    let mode = args.mode;
    let sid_for_dispatch = args.session_id.clone();
    let src_for_dispatch = args.source.clone();

    let raw_dispatch: BoxDispatchFn = Arc::new(move |dispatch_input| {
        let sid = sid_for_dispatch.clone();
        let src = src_for_dispatch.clone();
        let live_client = live_client_opt.clone();
        let orig_client = original_client.clone();
        Box::pin(async move {
            if is_live_route {
                log(
                    LIVE_ROUTE_DISPATCH_LOG,
                    Some(&serde_json::json!({
                        "sessionID": &sid,
                        "source": &src,
                    })),
                );
                let live_res = match mode {
                    InternalPromptDispatchMode::Async => match &live_client {
                        Some(lc) => match &lc.prompt_async {
                            Some(f) => f(&dispatch_input).await,
                            None => Err("no live prompt_async client".to_string()),
                        },
                        None => Err("no live client".to_string()),
                    },
                    InternalPromptDispatchMode::Sync => match &live_client {
                        Some(lc) => match &lc.prompt {
                            Some(f) => f(&dispatch_input).await,
                            None => Err("no live prompt client".to_string()),
                        },
                        None => Err("no live client".to_string()),
                    },
                };
                match live_res {
                    Ok(val) => Ok(val),
                    Err(err) => {
                        if is_pre_send_connection_failure(&err) {
                            mark_live_route_unavailable(&format!("dispatch:{err}"));
                            return match mode {
                                InternalPromptDispatchMode::Async => {
                                    orig_client.prompt_async(&dispatch_input).await
                                }
                                InternalPromptDispatchMode::Sync => {
                                    orig_client.prompt(&dispatch_input).await
                                }
                            };
                        }
                        Err(err)
                    }
                }
            } else {
                match mode {
                    InternalPromptDispatchMode::Async => {
                        orig_client.prompt_async(&dispatch_input).await
                    }
                    InternalPromptDispatchMode::Sync => orig_client.prompt(&dispatch_input).await,
                }
            }
        })
    });

    let raw_dispatch_for_compat = raw_dispatch.clone();
    let dispatch_fn: BoxDispatchFn = Arc::new(move |dispatch_input| {
        let raw = raw_dispatch_for_compat.clone();
        Box::pin(async move {
            dispatch_with_path_compatibility(|inp| raw(inp.clone()), &dispatch_input).await
        })
    });

    let queue_behavior = args.queue_behavior.unwrap_or(match args.mode {
        InternalPromptDispatchMode::Sync => InternalPromptQueueBehavior::Defer,
        InternalPromptDispatchMode::Async => InternalPromptQueueBehavior::Enqueue,
    });
    let dispatch_timeout_prefix = format!("[prompt-async-gate] {} dispatch", session_name.as_str());

    if queue_behavior == InternalPromptQueueBehavior::Defer {
        let active_reservation = get_active_reservation(&args.session_id, clock.now_ms());
        if let Some(res) = active_reservation {
            return InternalPromptDispatchResult::Reserved {
                reserved_by: res.source,
            };
        }

        let queued_by = get_queued_prompt_blocker(&args.session_id);
        if queued_by.is_some() || is_prompt_queue_draining(&args.session_id) {
            return InternalPromptDispatchResult::Reserved {
                reserved_by: queued_by.unwrap_or_else(|| args.source.clone()),
            };
        }

        if let Some(recent_result) = coalesce_recent_semantic_prompt_dispatch(
            &args.session_id,
            &dedupe_key,
            &args.source,
            clock.now_ms(),
        ) {
            return recent_result;
        }

        let dispatch_fn_clone = dispatch_fn.clone();
        let defer_result = dispatch_after_session_idle(DispatchAfterSessionIdleArgs {
            session_name,
            client: client_arc.as_ref(),
            session_id: &args.session_id,
            input: &args.input,
            source: &args.source,
            dedupe_key: &dedupe_key,
            settle_ms,
            post_dispatch_hold_ms,
            semantic_dedupe_hold_ms,
            dispatch_timeout_ms,
            check_status: args.check_status != Some(false),
            check_tool_state: args.check_tool_state != Some(false),
            dispatch: move |inp: &Value| {
                let inp = inp.clone();
                async move { dispatch_fn_clone(inp).await }
            },
            clock: clock.as_ref(),
        })
        .await;

        if let InternalPromptDispatchResult::Failed { error, .. } = &defer_result
            && resolved.route == PromptDispatchRoute::Live
        {
            let msg = error.get("message").and_then(Value::as_str).unwrap_or("");
            if msg.starts_with(&dispatch_timeout_prefix) {
                mark_live_route_unavailable("timeout");
            }
        }
        return defer_result;
    }

    if args.queue != Some(false) {
        if let Some(recent_result) = coalesce_recent_semantic_prompt_dispatch(
            &args.session_id,
            &dedupe_key,
            &args.source,
            clock.now_ms(),
        ) {
            return recent_result;
        }

        let c_stat = client_arc.clone();
        let status_fn: Option<BoxStatusFn> = if client_arc.has_status() {
            Some(Arc::new(move || {
                let c = c_stat.clone();
                Box::pin(async move { c.session_status().await })
            }))
        } else {
            None
        };
        let c_msg = client_arc.clone();
        let messages_fn: Option<BoxMessagesFn> = if client_arc.has_messages() {
            Some(Arc::new(move |sid, q| {
                let c = c_msg.clone();
                Box::pin(async move { c.session_messages(&sid, &q).await })
            }))
        } else {
            None
        };
        let queued_client = QueuedClient {
            status_fn,
            messages_fn,
        };

        let queue_id = next_prompt_queue_id();
        let queued_prompt = QueuedInternalPrompt {
            id: queue_id,
            session_id: args.session_id,
            session_name,
            client: queued_client,
            input: args.input,
            source: args.source,
            dedupe_key,
            settle_ms,
            post_dispatch_hold_ms,
            semantic_dedupe_hold_ms,
            dispatch_timeout_ms,
            queue_retry_ms,
            check_status: args.check_status != Some(false),
            check_tool_state: args.check_tool_state != Some(false),
            dispatch: dispatch_fn,
            clock: Some(clock.clone()),
        };
        return enqueue_internal_prompt(queued_prompt, clock.as_ref()).await;
    }

    if let Some(recent_result) = coalesce_recent_semantic_prompt_dispatch(
        &args.session_id,
        &dedupe_key,
        &args.source,
        clock.now_ms(),
    ) {
        return recent_result;
    }

    let dispatch_fn_clone = dispatch_fn.clone();
    let direct_result = dispatch_after_session_idle(DispatchAfterSessionIdleArgs {
        session_name,
        client: client_arc.as_ref(),
        session_id: &args.session_id,
        input: &args.input,
        source: &args.source,
        dedupe_key: &dedupe_key,
        settle_ms,
        post_dispatch_hold_ms,
        semantic_dedupe_hold_ms,
        dispatch_timeout_ms,
        check_status: args.check_status != Some(false),
        check_tool_state: args.check_tool_state != Some(false),
        dispatch: move |inp: &Value| {
            let inp = inp.clone();
            async move { dispatch_fn_clone(inp).await }
        },
        clock: clock.as_ref(),
    })
    .await;

    if let InternalPromptDispatchResult::Failed { error, .. } = &direct_result
        && resolved.route == PromptDispatchRoute::Live
    {
        let msg = error.get("message").and_then(Value::as_str).unwrap_or("");
        if msg.starts_with(&dispatch_timeout_prefix) {
            mark_live_route_unavailable("timeout");
        }
    }
    direct_result
}

pub fn release_prompt_async_reservation(
    session_id: &str,
    source: &str,
    options: Option<&PromptAsyncReservationReleaseOptions>,
) -> bool {
    let existing = get_prompt_reservation(session_id);
    let Some(existing) = existing else {
        return false;
    };

    if !reservation_source_matches_options(&existing.source, source, options) {
        log(
            "[prompt-async-gate] promptAsync reservation release skipped for different source",
            Some(&serde_json::json!({
                "sessionID": session_id,
                "source": source,
                "reservedBy": existing.source,
            })),
        );
        return false;
    }

    delete_prompt_reservation(session_id);
    delete_recent_prompt_dispatch(session_id, &existing.dedupe_key);
    release_in_flight_prompt_matching_dedupe(session_id, &existing.dedupe_key);
    let clock = current_clock();
    schedule_prompt_queue_drain(session_id, 0, clock.as_ref());
    log(
        "[prompt-async-gate] promptAsync reservation released",
        Some(&serde_json::json!({
            "sessionID": session_id,
            "source": source,
            "reservedBy": existing.source,
        })),
    );
    true
}

pub fn release_all_prompt_async_reservations_for_testing() {
    clear_prompt_reservations_for_testing();
    clear_prompt_queue_state_for_testing();
    clear_recent_prompt_dispatches_for_testing();
    reset_prompt_gate_timing_for_testing();
}

pub fn is_internal_prompt_dispatch_accepted(result: &InternalPromptDispatchResult) -> bool {
    matches!(
        result,
        InternalPromptDispatchResult::Dispatched { .. }
            | InternalPromptDispatchResult::Queued { .. }
    )
}
