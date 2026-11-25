//! Unified close ladder (`thread/control/close-ladder.ts`).

use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::super::tools::ports::{
    BoxFuture, ThreadCloseAnswer, ThreadCloseGraceful, ThreadCloseOptions, ThreadCloseOutcome,
};
use super::port::{ControlError, refuse};
use crate::call_budget::call_deadline::wait_within_call_budget;

#[derive(Debug, Clone)]
pub enum RecordAnswer {
    Ok(Map<String, Value>),
    Gone { reason: String },
}

pub type LadderSend = Box<
    dyn Fn(Map<String, Value>, Option<u64>) -> BoxFuture<Result<RecordAnswer, ControlError>>
        + Send
        + Sync,
>;

pub fn is_session_gone(reason: &str) -> bool {
    matches!(
        reason,
        "unknown_session"
            | "route_unlisted"
            | "session_identity_mismatch"
            | "session_not_attached"
            | "not_attached"
    )
}

const UNLOAD_REFUSE_DELIVERIES: &str = "unload_refuse_deliveries";
const UNLOAD_GRACEFUL: &str = "unload_graceful";
const GRACEFUL_WAIT_MS: u64 = 30_000;
const GRACEFUL_ANSWER_RESERVE_MS: u64 = 10_000;

fn close_effect_id() -> String {
    format!("omo-thread-close:{}", Uuid::new_v4())
}

fn strings(val: Option<&Value>) -> Vec<String> {
    val.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn deliveries_of(val: Option<&Value>) -> Option<Vec<String>> {
    let obj = val?.as_object()?;
    Some(strings(obj.get("deliveries")))
}

fn describe(data: &Map<String, Value>) -> String {
    let reason = data
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match data.get("detail").and_then(Value::as_str) {
        Some(detail) => format!("{reason}: {detail}"),
        None => reason.to_owned(),
    }
}

async fn advertises(session_id: &str, send: &LadderSend, capability: &str) -> bool {
    let mut cmd = Map::new();
    cmd.insert("type".into(), Value::String("get_protocol_info".into()));
    cmd.insert("sessionId".into(), Value::String(session_id.to_owned()));
    cmd.insert("observe".into(), Value::Bool(true));
    let Ok(RecordAnswer::Ok(data)) = send(cmd, None).await else {
        return false;
    };
    strings(data.get("capabilities"))
        .iter()
        .any(|c| c == capability)
}

fn graceful_of(data: &Map<String, Value>) -> ThreadCloseGraceful {
    let facts = data.get("graceful").and_then(Value::as_object);
    let dropped = data.get("dropped").and_then(Value::as_object);
    let stopped = facts
        .and_then(|f| f.get("stopped"))
        .and_then(Value::as_object);
    ThreadCloseGraceful {
        turn: facts
            .and_then(|f| f.get("turn"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        waited_ms: facts
            .and_then(|f| f.get("waited_ms"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        stopped_wake_sources: strings(stopped.and_then(|s| s.get("wake_sources"))),
        returned_user_messages: strings(dropped.and_then(|d| d.get("user_messages"))),
    }
}

fn make_unload(session_id: &str, extra: &[(&str, Value)]) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".into(), Value::String("unload_session".into()));
    m.insert("force".into(), Value::Bool(true));
    m.insert("children".into(), Value::String("detach".into()));
    m.insert("sessionId".into(), Value::String(session_id.to_owned()));
    m.insert("effectId".into(), Value::String(close_effect_id()));
    for (k, v) in extra {
        m.insert((*k).to_owned(), v.clone());
    }
    m
}

pub async fn close_ladder(
    session_id: &str,
    send: LadderSend,
    options: ThreadCloseOptions,
    leg_max_wait_ms: Option<u64>,
) -> Result<ThreadCloseAnswer, ControlError> {
    if options.graceful == Some(true) {
        return graceful_close(session_id, &send, options.wait_ms, leg_max_wait_ms).await;
    }

    let first_extra = if options.force == Some(true) {
        vec![("interrupt", Value::Bool(true))]
    } else {
        vec![("deliveries", Value::String("refuse".into()))]
    };

    let mut refused = match send(make_unload(session_id, &first_extra), None).await? {
        RecordAnswer::Gone { .. } => {
            return Ok(ThreadCloseAnswer {
                outcome: ThreadCloseOutcome::NotLive,
                dropped_deliveries: Vec::new(),
                graceful: None,
            });
        }
        RecordAnswer::Ok(data) => {
            if data.get("outcome").and_then(Value::as_str) == Some("unloaded") {
                return Ok(ThreadCloseAnswer {
                    outcome: ThreadCloseOutcome::Closed,
                    dropped_deliveries: deliveries_of(data.get("dropped")).unwrap_or_default(),
                    graceful: None,
                });
            }
            if data.get("reason").and_then(Value::as_str) == Some("session_closing") {
                return Ok(ThreadCloseAnswer {
                    outcome: ThreadCloseOutcome::NotLive,
                    dropped_deliveries: deliveries_of(data.get("dropped")).unwrap_or_default(),
                    graceful: None,
                });
            }
            data
        }
    };

    let reason_str = refused.get("reason").and_then(Value::as_str);
    if options.force != Some(true) && matches!(reason_str, Some("turn_active" | "work_pending")) {
        let pending = deliveries_of(refused.get("pending"));
        if pending.is_none() && !advertises(session_id, &send, UNLOAD_REFUSE_DELIVERIES).await {
            let reason = reason_str.unwrap_or("turn_active");
            let msg = if reason == "turn_active" {
                format!(
                    "Thread {session_id} is still working ({}), and its host's engine cannot name the messages a forced close would drop.",
                    describe(&refused)
                )
            } else {
                format!(
                    "Thread {session_id} holds messages it has not read ({}), and its host's engine cannot name them.",
                    describe(&refused)
                )
            };
            return Err(refuse(
                "turn_active",
                msg,
                "Wait for it to read them, or close it with force: true (anything it has not read is dropped); an engine that names them comes with a newer senpi on that device.",
                Some(json!({
                    "command": "unload_session",
                    "reason": reason,
                    "detail": refused.get("detail"),
                    "pending_deliveries": "unavailable",
                })),
            )
            .into());
        }

        let second = send(
            make_unload(
                session_id,
                &[
                    ("interrupt", Value::Bool(true)),
                    ("deliveries", Value::String("refuse".into())),
                ],
            ),
            None,
        )
        .await?;

        refused = match second {
            RecordAnswer::Gone { .. } => {
                return Ok(ThreadCloseAnswer {
                    outcome: ThreadCloseOutcome::NotLive,
                    dropped_deliveries: Vec::new(),
                    graceful: None,
                });
            }
            RecordAnswer::Ok(data) => {
                if data.get("outcome").and_then(Value::as_str) == Some("unloaded") {
                    return Ok(ThreadCloseAnswer {
                        outcome: ThreadCloseOutcome::Closed,
                        dropped_deliveries: deliveries_of(data.get("dropped")).unwrap_or_default(),
                        graceful: None,
                    });
                }
                if data.get("reason").and_then(Value::as_str) == Some("session_closing") {
                    return Ok(ThreadCloseAnswer {
                        outcome: ThreadCloseOutcome::NotLive,
                        dropped_deliveries: deliveries_of(data.get("dropped")).unwrap_or_default(),
                        graceful: None,
                    });
                }
                data
            }
        };

        if let Some(unread) = deliveries_of(refused.get("pending"))
            && !unread.is_empty()
        {
            return Err(refuse(
                "turn_active",
                format!(
                    "Thread {session_id} has {} message(s) it has not read yet ({}); closing it now would drop them.",
                    unread.len(),
                    unread.join(", ")
                ),
                "Let it read them and retry, or close it with force: true to drop them (the result lists dropped_deliveries).",
                Some(json!({
                    "command": "unload_session",
                    "reason": refused.get("reason"),
                    "pending_deliveries": unread,
                    "retry_with": { "force": true },
                })),
            )
            .into());
        }
    }

    refused_unload(session_id, &refused, &send).await
}

async fn graceful_close(
    session_id: &str,
    send: &LadderSend,
    wait_ms: Option<u64>,
    leg_max_wait_ms: Option<u64>,
) -> Result<ThreadCloseAnswer, ControlError> {
    if !advertises(session_id, send, UNLOAD_GRACEFUL).await {
        return Err(refuse(
            "unsupported",
            format!(
                "The engine holding thread {session_id} cannot close it gracefully: it runs a senpi from before graceful close."
            ),
            "Update omo on that device and restart its host, or close the thread with force: true (its running turn is stopped).",
            Some(json!({
                "command": "unload_session",
                "capability": UNLOAD_GRACEFUL,
            })),
        )
        .into());
    }
    let requested = wait_ms.unwrap_or(GRACEFUL_WAIT_MS);
    let leg_cut = leg_max_wait_ms.filter(|cap| requested > *cap);
    let wait_i64 = wait_within_call_budget(
        leg_cut.unwrap_or(requested) as i64,
        Some(GRACEFUL_ANSWER_RESERVE_MS as i64),
        None,
    )
    .max(0);
    let wait = wait_i64 as u64;
    let capped = leg_cut.filter(|cap| wait == *cap);
    if wait == 0 {
        return Err(refuse(
            "unsupported",
            format!("This call has no time left to wait for thread {session_id}'s turn."),
            "Close it again in a new call.",
            Some(json!({ "command": "unload_session" })),
        )
        .into());
    }
    let cmd = make_unload(
        session_id,
        &[
            ("graceful", Value::Bool(true)),
            ("waitMs", Value::Number(wait.into())),
        ],
    );
    let answered = send(cmd, Some(wait)).await?;
    let data = match answered {
        RecordAnswer::Gone { .. } => {
            return Ok(ThreadCloseAnswer {
                outcome: ThreadCloseOutcome::NotLive,
                dropped_deliveries: Vec::new(),
                graceful: None,
            });
        }
        RecordAnswer::Ok(d) => d,
    };
    let dropped = deliveries_of(data.get("dropped")).unwrap_or_default();
    if data.get("outcome").and_then(Value::as_str) == Some("unloaded") {
        return Ok(ThreadCloseAnswer {
            outcome: ThreadCloseOutcome::Closed,
            dropped_deliveries: dropped,
            graceful: Some(graceful_of(&data)),
        });
    }
    if data.get("reason").and_then(Value::as_str) == Some("session_closing") {
        return Ok(ThreadCloseAnswer {
            outcome: ThreadCloseOutcome::NotLive,
            dropped_deliveries: dropped,
            graceful: None,
        });
    }
    if data.get("timed_out").and_then(Value::as_bool) == Some(true) {
        let cap_note = capped
            .map(|_| format!(" (the most a thread on another device can wait, not the {requested} ms asked for)"))
            .unwrap_or_default();
        let turn_val = data.get("turn").cloned().unwrap_or(Value::Null);
        let waited_val = data.get("waited_ms").cloned().unwrap_or(Value::Null);
        let mut details = json!({
            "command": "unload_session",
            "reason": "turn_active",
            "still_running": true,
            "open": true,
            "turn": turn_val,
            "waited_ms": waited_val,
            "returned_deliveries": dropped,
            "retry_with": { "force": true },
        });
        if let Some(max_ms) = capped
            && let Some(obj) = details.as_object_mut()
        {
            obj.insert(
                "wait_capped".into(),
                json!({ "requested_ms": requested, "max_ms": max_ms }),
            );
        }
        let next = if capped.is_none() {
            "Close it again with a longer wait_ms once its turn is nearly done, or with force: true to stop the turn."
        } else {
            "Close it again once its turn is nearly done, or with force: true to stop the turn."
        };
        return Err(refuse(
            "turn_active",
            format!(
                "Thread {session_id} is still working on turn {turn_val} after {waited_val} ms{cap_note}. It stays open and takes messages again; the {} unread message(s) taken out of its queue are queued for it again.",
                dropped.len()
            ),
            next,
            Some(details),
        )
        .into());
    }
    if data.get("reason").and_then(Value::as_str) == Some("composer_draft") {
        return Err(refuse(
            "unsupported",
            format!(
                "Thread {session_id}'s terminal has unsent text in its composer, and a close would discard it."
            ),
            "Ask the person at that terminal to send or clear it, then close the thread again.",
            Some(json!({ "command": "unload_session", "reason": "composer_draft" })),
        )
        .into());
    }
    refused_unload(session_id, &data, send).await
}

async fn refused_unload(
    session_id: &str,
    refused: &Map<String, Value>,
    send: &LadderSend,
) -> Result<ThreadCloseAnswer, ControlError> {
    let reason = refused
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if reason == "host_draining" {
        return Err(refuse(
            "host_unavailable",
            format!(
                "The host holding thread {session_id} is shutting down and refused to close it."
            ),
            "Retry once that host has restarted.",
            Some(json!({ "command": "unload_session", "reason": reason })),
        )
        .into());
    }
    if reason != "unsupported" {
        let detail = refused
            .get("detail")
            .and_then(Value::as_str)
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        let dropped = deliveries_of(refused.get("dropped"));
        let mut details = json!({
            "command": "unload_session",
            "reason": reason,
            "detail": refused.get("detail"),
        });
        if let Some(d) = dropped
            && !d.is_empty()
            && let Some(obj) = details.as_object_mut()
        {
            obj.insert("dropped_deliveries".into(), json!(d));
        }
        return Err(refuse(
            "unsupported",
            format!("The host holding thread {session_id} refused to close it ({reason}{detail})."),
            "Read the thread and retry once its work settles.",
            Some(details),
        )
        .into());
    }

    let mut close_cmd = Map::new();
    close_cmd.insert("type".into(), Value::String("close_session".into()));
    close_cmd.insert("sessionId".into(), Value::String(session_id.to_owned()));
    if matches!(send(close_cmd, None).await?, RecordAnswer::Gone { .. }) {
        return Ok(ThreadCloseAnswer {
            outcome: ThreadCloseOutcome::NotLive,
            dropped_deliveries: Vec::new(),
            graceful: None,
        });
    }

    let mut probe_cmd = Map::new();
    probe_cmd.insert("type".into(), Value::String("get_state".into()));
    probe_cmd.insert("sessionId".into(), Value::String(session_id.to_owned()));
    let outcome = match send(probe_cmd, None).await? {
        RecordAnswer::Gone { .. } => ThreadCloseOutcome::Closed,
        RecordAnswer::Ok(_) => ThreadCloseOutcome::HeldElsewhere,
    };
    Ok(ThreadCloseAnswer {
        outcome,
        dropped_deliveries: Vec::new(),
        graceful: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn send_returning(frame: Value) -> LadderSend {
        Box::new(move |cmd, _wait| {
            let f = if cmd.get("type").and_then(Value::as_str) == Some("get_protocol_info") {
                json!({ "capabilities": [UNLOAD_GRACEFUL] })
            } else {
                frame.clone()
            };
            Box::pin(
                async move { Ok(RecordAnswer::Ok(f.as_object().cloned().unwrap_or_default())) },
            )
        })
    }

    #[tokio::test]
    async fn graceful_unload_succeeds_with_wait_and_returned_deliveries() {
        let send = send_returning(json!({
            "outcome": "unloaded",
            "dropped": { "deliveries": ["del-1"] },
            "graceful": { "turn": 3, "waited_ms": 120 }
        }));
        let answer = close_ladder(
            "sess-1",
            send,
            ThreadCloseOptions {
                force: None,
                graceful: Some(true),
                wait_ms: Some(5_000),
            },
            Some(5_000),
        )
        .await
        .expect("graceful close ok");
        assert_eq!(answer.outcome, ThreadCloseOutcome::Closed);
        assert_eq!(answer.dropped_deliveries, vec!["del-1".to_owned()]);
        assert_eq!(answer.graceful.unwrap().waited_ms, 120);
    }

    #[tokio::test]
    async fn graceful_unload_caps_wait_to_leg_limit_and_reports_turn_active() {
        let seen_wait = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen_c = seen_wait.clone();
        let send: LadderSend = Box::new(move |cmd, wait| {
            if cmd.get("type").and_then(Value::as_str) == Some("get_protocol_info") {
                return Box::pin(async {
                    Ok(RecordAnswer::Ok(
                        json!({ "capabilities": [UNLOAD_GRACEFUL] })
                            .as_object()
                            .unwrap()
                            .clone(),
                    ))
                });
            }
            *seen_c.lock().unwrap() = cmd
                .get("waitMs")
                .and_then(Value::as_i64)
                .or(wait.map(|w| w as i64));
            Box::pin(async {
                Ok(RecordAnswer::Ok(
                    json!({ "timed_out": true, "turn": "t-9", "waited_ms": 35_000, "dropped": { "deliveries": ["d-1"] } })
                        .as_object()
                        .unwrap()
                        .clone(),
                ))
            })
        });
        let err = close_ladder(
            "sess-1",
            send,
            ThreadCloseOptions {
                force: None,
                graceful: Some(true),
                wait_ms: Some(120_000),
            },
            Some(35_000),
        )
        .await
        .unwrap_err();
        assert_eq!(*seen_wait.lock().unwrap(), Some(35_000));
        match err {
            ControlError::Refusal(r) => {
                assert_eq!(r.failure.code.as_str(), "turn_active");
                let details = r.failure.details.unwrap();
                assert_eq!(details["wait_capped"]["requested_ms"], 120_000);
                assert_eq!(details["wait_capped"]["max_ms"], 35_000);
                assert_eq!(details["returned_deliveries"], json!(["d-1"]));
            }
            other => panic!("expected Refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn graceful_unload_composer_draft_refuses_unsupported() {
        let send = send_returning(json!({ "reason": "composer_draft" }));
        let err = close_ladder(
            "sess-1",
            send,
            ThreadCloseOptions {
                force: None,
                graceful: Some(true),
                wait_ms: Some(1_000),
            },
            None,
        )
        .await
        .unwrap_err();
        match err {
            ControlError::Refusal(r) => {
                assert_eq!(r.failure.code.as_str(), "unsupported");
                assert!(r.failure.message.contains("unsent text"));
            }
            other => panic!("expected Refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn graceful_unload_host_draining_refuses_host_unavailable() {
        let send = send_returning(json!({ "reason": "host_draining" }));
        let err = close_ladder(
            "sess-1",
            send,
            ThreadCloseOptions {
                force: None,
                graceful: Some(true),
                wait_ms: Some(1_000),
            },
            None,
        )
        .await
        .unwrap_err();
        match err {
            ControlError::Refusal(r) => assert_eq!(r.failure.code.as_str(), "host_unavailable"),
            other => panic!("expected Refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn graceful_unload_session_closing_returns_not_live() {
        let send = send_returning(json!({ "reason": "session_closing" }));
        let answer = close_ladder(
            "sess-1",
            send,
            ThreadCloseOptions {
                force: None,
                graceful: Some(true),
                wait_ms: Some(1_000),
            },
            None,
        )
        .await
        .expect("closing -> not_live");
        assert_eq!(answer.outcome, ThreadCloseOutcome::NotLive);
    }
}
