//! The session-control answers a multi-session host and a terminal control endpoint share (senpi
//! `rpc/session-control-actions-data.ts`): the model catalog, the thinking-level refusal, and the
//! `interrupt` outcome.
use maho_ai::types::ModelThinkingLevel;
use maho_core::agent_session::AgentSession;
use serde::Serialize;
use serde_json::{Value, json};
use std::future::Future;

/// The refusal of a level the active model cannot run; callers match this text.
pub fn unsupported_thinking_level(level: &Value) -> String {
    let level = match level {
        Value::String(text) => text.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    };
    format!("Thinking level {level} is not supported by the active model.")
}

/// `get_available_models`: each model with the values the session itself acts on (#3666), a models.json or
/// extension definition may declare the priority pairing without putting it on the model.
pub fn available_models_data(session: &AgentSession) -> Value {
    let registry = session.model_registry();
    let thinking_enabled = session.thinking_level() != ModelThinkingLevel::Off;
    let models: Vec<Value> = registry
        .get_available()
        .into_iter()
        .map(|model| {
            let mut value = json!(model);
            if let Some(tier) = registry.get_service_tier(&model) {
                value["serviceTier"] = json!(tier);
            }
            if let Some(upstream) = registry.get_upstream_model_id(&model) {
                value["upstreamModelId"] = json!(upstream);
            }
            value["supportedThinkingLevels"] = json!(
                maho_core::thinking_levels::get_supported_thinking_levels(&model)
            );
            value["supportsAssistantPrefill"] = json!(
                maho_ai::model::model_supports_assistant_prefill(&model, thinking_enabled)
            );
            value
        })
        .collect();
    json!({ "models": models })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterruptOutcome {
    pub interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Present when the caller asked `background: "stop"`: the interrupted turn's background jobs it stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<Value>,
}

/// `interrupt { turnId?, background? }`: stops the running turn and only that. An idle session answers
/// `interrupted: false`; so does a `turnId` naming a turn that is no longer the running one, which then
/// answers the running turn's id. `stop` resolves once the turn has stopped; with `background: "stop"` the
/// background jobs that turn started are stopped after it, and the answer names them.
pub async fn interrupt_running_turn<F, Fut>(
    session: &AgentSession,
    requested_turn_id: Option<&Value>,
    stop: F,
) -> InterruptOutcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    interrupt_running_turn_with_background(session, requested_turn_id, stop, None).await
}

pub async fn decide_interrupt<F, Fut>(
    active: bool,
    turn_id: String,
    requested_turn_id: Option<&Value>,
    stop: F,
) -> InterruptOutcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    decide_interrupt_with_background(active, turn_id, requested_turn_id, stop, None).await
}

async fn decide_interrupt_with_background<F, Fut>(
    active: bool,
    turn_id: String,
    requested_turn_id: Option<&Value>,
    stop: F,
    background: Option<&Value>,
) -> InterruptOutcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let stops_background = background == Some(&Value::String("stop".into()));
    let nothing = || stops_background.then(|| json!({ "stopped": [], "failed": [] }));
    if !active {
        return InterruptOutcome {
            interrupted: false,
            turn_id: None,
            background: nothing(),
        };
    }
    if let Some(Value::String(requested)) = requested_turn_id
        && *requested != turn_id
    {
        return InterruptOutcome {
            interrupted: false,
            turn_id: Some(turn_id),
            background: nothing(),
        };
    }
    stop().await;
    InterruptOutcome {
        interrupted: true,
        turn_id: Some(turn_id),
        background: nothing(),
    }
}

pub async fn interrupt_running_turn_with_background<F, Fut>(
    session: &AgentSession,
    requested_turn_id: Option<&Value>,
    stop: F,
    background: Option<&Value>,
) -> InterruptOutcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let turn_id = session.external_admission().turn_epoch().to_string();
    let active = session.is_streaming() || session.retry_attempt() != 0;
    decide_interrupt_with_background(active, turn_id, requested_turn_id, stop, background).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn the_refusal_text_names_the_level() {
        assert_eq!(
            unsupported_thinking_level(&json!("xhigh")),
            "Thinking level xhigh is not supported by the active model."
        );
    }

    #[tokio::test]
    async fn idle_and_stale_turns_are_not_interrupted() {
        let stopped = AtomicBool::new(false);
        let idle = decide_interrupt(false, "3".into(), None, || async {
            stopped.store(true, Ordering::SeqCst)
        })
        .await;
        assert_eq!(
            idle,
            InterruptOutcome {
                interrupted: false,
                turn_id: None,
                background: None,
            }
        );
        let stale = decide_interrupt(true, "3".into(), Some(&json!("2")), || async {
            stopped.store(true, Ordering::SeqCst)
        })
        .await;
        assert_eq!(
            stale,
            InterruptOutcome {
                interrupted: false,
                turn_id: Some("3".into()),
                background: None,
            }
        );
        assert!(!stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn the_running_turn_is_stopped_for_a_matching_or_absent_id() {
        let stopped = AtomicBool::new(false);
        let outcome = decide_interrupt(true, "3".into(), Some(&json!("3")), || async {
            stopped.store(true, Ordering::SeqCst)
        })
        .await;
        assert_eq!(
            serde_json::to_value(&outcome).expect("wire"),
            json!({"interrupted":true,"turnId":"3"})
        );
        assert!(stopped.load(Ordering::SeqCst));
        // A non-string turnId does not name a turn.
        let any = decide_interrupt(true, "3".into(), Some(&json!(3)), || async {}).await;
        assert!(any.interrupted);
    }
}
