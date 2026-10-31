//! Port of `goal/rpc-wait-surface.ts` (senpi 903b8336).

use crate::{
    monitor_continuation_types::DelayedContinuationKind,
    stale_context::is_stale_extension_context_error, wait_progress::GoalWaitKind,
};
use maho_ext_api::{ExtensionApi, ExtensionFailure, ExtensionFuture, JsonValue};
use serde_json::json;
use std::sync::Arc;

/// RPC request a client sends to run the armed continuation now instead of at its due time (senpi#3196).
pub const GOAL_CONTINUATION_FIRE_NOW_REQUEST: &str = "goal_continuation_fire_now";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalContinuationFireNowReason {
    NotArmed,
    Held,
}

/// `GoalContinuationFireNowResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalContinuationFireNowResult {
    Fired {
        kind: DelayedContinuationKind,
    },
    NotFired {
        reason: GoalContinuationFireNowReason,
    },
}

fn wait_kind_name(kind: GoalWaitKind) -> &'static str {
    match kind {
        GoalWaitKind::Monitor => "monitor",
        GoalWaitKind::UserGrace => "userGrace",
    }
}

impl GoalContinuationFireNowResult {
    /// Wire shape: `{ fired: true, kind }` or `{ fired: false, reason }`.
    pub fn to_json(self) -> JsonValue {
        match self {
            Self::Fired { kind } => json!({"fired": true, "kind": wait_kind_name(kind)}),
            Self::NotFired { reason } => json!({"fired": false, "reason": match reason {
                GoalContinuationFireNowReason::NotArmed => "not_armed",
                GoalContinuationFireNowReason::Held => "held",
            }}),
        }
    }
}

/// Publishes a continuation event to in-process listeners and, as an `extension_event`, to RPC
/// clients, so a desktop thread shows the same wait the TUI's ticker shows. A retired runtime
/// cannot reach a client; its staleness is expected there and is dropped.
pub fn publish_goal_continuation_event(
    pi: &ExtensionApi,
    name: &str,
    data: &JsonValue,
) -> Result<(), ExtensionFailure> {
    pi.events.emit(name, data);
    match pi.rpc_emit(name, data) {
        Err(error) if !is_stale_extension_context_error(&error) => Err(error),
        _ => Ok(()),
    }
}

pub type GoalFireNow =
    Arc<dyn Fn() -> ExtensionFuture<'static, GoalContinuationFireNowResult> + Send + Sync>;

pub fn register_goal_fire_now_request(
    pi: &mut ExtensionApi,
    fire_now: GoalFireNow,
) -> Result<(), ExtensionFailure> {
    pi.rpc_handle(
        GOAL_CONTINUATION_FIRE_NOW_REQUEST,
        Arc::new(move |_params| {
            let fire_now = fire_now.clone();
            Box::pin(async move { fire_now().await.map(GoalContinuationFireNowResult::to_json) })
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{EventBus, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension};
    use std::sync::Mutex;

    fn api() -> ExtensionApi {
        ExtensionApi::new(
            LoadedExtension::new("goal", "/tmp".into(), Default::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        )
    }

    #[test]
    fn publish_reaches_in_process_and_rpc_listeners() {
        let api = api();
        let seen: Arc<Mutex<Vec<(String, JsonValue)>>> = Arc::default();
        let mut subscriptions = Vec::new();
        for channel in ["goal_continuation_scheduled", "senpi:extension-rpc-event"] {
            let seen = seen.clone();
            let name = channel.to_owned();
            subscriptions.push(api.events.on(
                channel,
                Arc::new(move |data| seen.lock().unwrap().push((name.clone(), data.clone()))),
            ));
        }
        publish_goal_continuation_event(
            &api,
            "goal_continuation_scheduled",
            &json!({"delayMs": 5}),
        )
        .unwrap();
        let seen = seen.lock().unwrap().clone();
        drop(subscriptions);
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0],
            ("goal_continuation_scheduled".into(), json!({"delayMs": 5}))
        );
        assert_eq!(seen[1].1["name"], "goal_continuation_scheduled");
        assert_eq!(seen[1].1["data"], json!({"delayMs": 5}));
    }

    #[test]
    fn rpc_emit_errors_that_are_not_staleness_surface() {
        let api = api();
        assert!(publish_goal_continuation_event(&api, " ", &json!({})).is_err());
    }

    #[test]
    fn fire_now_results_serialize_like_upstream() {
        assert_eq!(
            GoalContinuationFireNowResult::Fired {
                kind: GoalWaitKind::Monitor
            }
            .to_json(),
            json!({"fired": true, "kind": "monitor"})
        );
        assert_eq!(
            GoalContinuationFireNowResult::Fired {
                kind: GoalWaitKind::UserGrace
            }
            .to_json(),
            json!({"fired": true, "kind": "userGrace"})
        );
        assert_eq!(
            GoalContinuationFireNowResult::NotFired {
                reason: GoalContinuationFireNowReason::Held
            }
            .to_json(),
            json!({"fired": false, "reason": "held"})
        );
        assert_eq!(
            GoalContinuationFireNowResult::NotFired {
                reason: GoalContinuationFireNowReason::NotArmed
            }
            .to_json(),
            json!({"fired": false, "reason": "not_armed"})
        );
    }

    #[tokio::test]
    async fn fire_now_request_is_registered_and_answers() {
        let mut api = api();
        register_goal_fire_now_request(
            &mut api,
            Arc::new(|| {
                Box::pin(async {
                    Ok(GoalContinuationFireNowResult::Fired {
                        kind: GoalWaitKind::Monitor,
                    })
                })
            }),
        )
        .unwrap();
        let handler = api
            .registered
            .rpc_handlers
            .get(GOAL_CONTINUATION_FIRE_NOW_REQUEST)
            .cloned()
            .expect("handler");
        assert_eq!(
            handler(JsonValue::Null).await.unwrap(),
            json!({"fired": true, "kind": "monitor"})
        );
        assert!(
            register_goal_fire_now_request(
                &mut api,
                Arc::new(|| Box::pin(async {
                    Ok(GoalContinuationFireNowResult::NotFired {
                        reason: GoalContinuationFireNowReason::NotArmed,
                    })
                }))
            )
            .is_err()
        );
    }
}
