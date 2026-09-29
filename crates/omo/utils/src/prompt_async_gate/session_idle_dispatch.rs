use std::future::Future;

use serde_json::{Value, json};

use crate::logger::log;
use crate::session_idle_settle::is_active_session_status_type;

use super::pending_tool_turn::{
    SessionLatestAssistantBlocksArgs, session_latest_assistant_blocks_internal_prompt,
};
use super::recent_dispatches::remember_recent_prompt_dispatch;
use super::reservations::{
    finish_prompt_reservation, get_active_reservation, next_reservation_token,
    set_prompt_reservation,
};
use super::timing::{Clock, get_prompt_gate_messages_fetch_timeout_ms, with_dispatch_timeout};
use super::types::{
    InternalPromptDispatchResult, PromptAsyncReservation, PromptGateClient, PromptSessionName,
};

pub struct DispatchAfterSessionIdleArgs<'a, C: ?Sized, F> {
    pub session_name: PromptSessionName,
    pub client: &'a C,
    pub session_id: &'a str,
    pub input: &'a Value,
    pub source: &'a str,
    pub dedupe_key: &'a str,
    pub settle_ms: u64,
    pub post_dispatch_hold_ms: u64,
    pub semantic_dedupe_hold_ms: u64,
    pub dispatch_timeout_ms: u64,
    pub check_status: bool,
    pub check_tool_state: bool,
    pub dispatch: F,
    pub clock: &'a dyn Clock,
}

fn check_session_status_active(response: &Value, session_id: &str) -> bool {
    let statuses = response
        .get("data")
        .and_then(Value::as_object)
        .or_else(|| response.as_object());
    statuses
        .and_then(|map| map.get(session_id))
        .and_then(|st| st.get("type"))
        .and_then(Value::as_str)
        .is_some_and(is_active_session_status_type)
}

pub async fn dispatch_after_session_idle<C, F, Fut>(
    args: DispatchAfterSessionIdleArgs<'_, C, F>,
) -> InternalPromptDispatchResult
where
    C: PromptGateClient + ?Sized,
    F: FnOnce(&Value) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let now = args.clock.now_ms();
    let existing = get_active_reservation(args.session_id, now);
    if let Some(res) = existing {
        log(
            &format!(
                "[prompt-async-gate] {} skipped because session is reserved",
                args.session_name.as_str()
            ),
            Some(&json!({
                "sessionID": args.session_id,
                "source": args.source,
                "reservedBy": res.source,
                "reservedAgeMs": now.saturating_sub(res.reserved_at),
            })),
        );
        return InternalPromptDispatchResult::Reserved {
            reserved_by: res.source,
        };
    }

    let reservation = PromptAsyncReservation {
        source: args.source.to_string(),
        dedupe_key: args.dedupe_key.to_string(),
        reserved_at: now,
        token: next_reservation_token(),
        expires_at: None,
    };
    set_prompt_reservation(args.session_id, reservation.clone());
    let mut dispatch_attempted = false;

    let can_read_status = args.check_status && args.client.has_status();
    if args.settle_ms > 0 {
        args.clock.sleep_ms(args.settle_ms).await;
    }

    let mut session_active = false;
    if can_read_status {
        let op_name = format!(
            "[prompt-async-gate] {} isSessionActive",
            args.session_name.as_str()
        );
        let timeout_ms = args.dispatch_timeout_ms.min(5_000);
        let status_res = with_dispatch_timeout(
            args.client.session_status(),
            timeout_ms,
            &op_name,
            args.clock,
        )
        .await;

        if let Ok(response) = status_res {
            session_active = check_session_status_active(&response, args.session_id);
        }
    }

    if session_active {
        log(
            &format!(
                "[prompt-async-gate] {} skipped because session is active",
                args.session_name.as_str()
            ),
            Some(&json!({
                "sessionID": args.session_id,
                "source": args.source,
            })),
        );
        finish_prompt_reservation(
            args.session_id,
            &reservation,
            dispatch_attempted,
            args.post_dispatch_hold_ms,
            args.clock.now_ms(),
        );
        return InternalPromptDispatchResult::Active;
    }

    if args.check_tool_state && args.client.has_messages() {
        let fetch_timeout = args
            .dispatch_timeout_ms
            .min(get_prompt_gate_messages_fetch_timeout_ms());
        let blocks =
            session_latest_assistant_blocks_internal_prompt(SessionLatestAssistantBlocksArgs {
                client: args.client,
                session_id: args.session_id,
                input: args.input,
                session_name: args.session_name,
                source: args.source,
                timeout_ms: fetch_timeout,
                clock: args.clock,
            })
            .await;

        if blocks {
            log(
                &format!(
                    "[prompt-async-gate] {} skipped because latest assistant is still active",
                    args.session_name.as_str()
                ),
                Some(&json!({
                    "sessionID": args.session_id,
                    "source": args.source,
                })),
            );
            finish_prompt_reservation(
                args.session_id,
                &reservation,
                dispatch_attempted,
                args.post_dispatch_hold_ms,
                args.clock.now_ms(),
            );
            return InternalPromptDispatchResult::Active;
        }
    }

    log(
        &format!(
            "[prompt-async-gate] {} dispatching",
            args.session_name.as_str()
        ),
        Some(&json!({
            "sessionID": args.session_id,
            "source": args.source,
        })),
    );
    dispatch_attempted = true;
    let op_name = format!(
        "[prompt-async-gate] {} dispatch",
        args.session_name.as_str()
    );
    let dispatch_future = (args.dispatch)(args.input);
    let dispatch_result = with_dispatch_timeout(
        dispatch_future,
        args.dispatch_timeout_ms,
        &op_name,
        args.clock,
    )
    .await;

    let now_after = args.clock.now_ms();
    match dispatch_result {
        Ok(response) => {
            remember_recent_prompt_dispatch(
                args.session_id,
                args.dedupe_key,
                args.source,
                args.semantic_dedupe_hold_ms,
                now_after,
            );
            log(
                &format!(
                    "[prompt-async-gate] {} dispatched",
                    args.session_name.as_str()
                ),
                Some(&json!({
                    "sessionID": args.session_id,
                    "source": args.source,
                })),
            );
            finish_prompt_reservation(
                args.session_id,
                &reservation,
                dispatch_attempted,
                args.post_dispatch_hold_ms,
                now_after,
            );
            InternalPromptDispatchResult::Dispatched { response }
        }
        Err(err_msg) => {
            if dispatch_attempted {
                remember_recent_prompt_dispatch(
                    args.session_id,
                    args.dedupe_key,
                    args.source,
                    args.semantic_dedupe_hold_ms,
                    now_after,
                );
            }
            log(
                &format!("[prompt-async-gate] {} failed", args.session_name.as_str()),
                Some(&json!({
                    "sessionID": args.session_id,
                    "source": args.source,
                    "error": &err_msg,
                })),
            );
            finish_prompt_reservation(
                args.session_id,
                &reservation,
                dispatch_attempted,
                args.post_dispatch_hold_ms,
                now_after,
            );
            let error_json = json!({
                "name": "Error",
                "message": err_msg,
            });
            InternalPromptDispatchResult::Failed {
                error: error_json,
                dispatch_attempted,
            }
        }
    }
}
