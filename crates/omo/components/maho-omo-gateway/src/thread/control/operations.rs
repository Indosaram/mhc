//! Control tool operations (`thread/control/operations.ts`).

use serde_json::{Value, json};

use super::super::contracts::params::ThreadCloseInput;
use super::super::contracts::state::ThreadModel;
use super::super::tools::internals::failure;
use super::super::tools::model_match::{
    ModelMatchResult, exact_model, match_model, model_pattern_failure,
};
use super::super::tools::ports::{ThreadCloseOptions, ThreadCloseOutcome};
use super::interrupt::interrupt_outcome_json;
use super::port::{ControlError, SessionControl, SessionModelState, control_identity, refuse};

pub fn not_live_failure() -> Value {
    failure(
        "not_resumable",
        "The thread has no live owner.",
        "Retry when the target is live.",
        None,
    )
}

pub async fn answered<F, Fut>(body: F) -> Result<Value, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Value, ControlError>>,
{
    match body().await {
        Ok(v) => Ok(v),
        Err(ControlError::Refusal(r)) => Ok(json!({ "kind": "error", "error": r.failure })),
        Err(ControlError::Transport(e)) => Err(e),
    }
}

pub async fn interrupt_thread(
    control: &dyn SessionControl,
    turn_id: Option<&str>,
) -> Result<Value, ControlError> {
    let outcome = control.interrupt(turn_id).await?;
    Ok(interrupt_outcome_json(
        &outcome,
        control.thread_id(),
        control.device(),
    ))
}

pub fn close_options(value: &ThreadCloseInput) -> ThreadCloseOptions {
    ThreadCloseOptions {
        force: value.force,
        graceful: value.graceful,
        wait_ms: value.wait_ms,
    }
}

pub async fn close_thread(
    control: &dyn SessionControl,
    options: ThreadCloseOptions,
) -> Result<Value, ControlError> {
    if options.graceful == Some(true) && options.force == Some(true) {
        return Ok(failure(
            "invalid_arguments",
            "graceful and force exclude each other: a graceful close lets the turn end, a forced one stops it.",
            "Pass one of them.",
            None,
        ));
    }
    if options.wait_ms.is_some() && options.graceful != Some(true) {
        return Ok(failure(
            "invalid_arguments",
            "wait_ms only bounds a graceful close: a close without graceful does not wait for the turn.",
            "Pass graceful: true with wait_ms, or drop wait_ms.",
            None,
        ));
    }
    let answer = control.close(options).await?;
    if answer.outcome == ThreadCloseOutcome::HeldElsewhere {
        return Ok(failure(
            "foreign_live_owner",
            format!(
                "Thread {} has no session file yet and another client still holds it, so only this tool's hold was released.",
                control.thread_id()
            ),
            "Close it from the client that holds it, or send it a message first so it can be closed for every holder.",
            control.device().map(|d| json!({ "device": d })),
        ));
    }
    let closed = answer.outcome == ThreadCloseOutcome::Closed;
    let mut obj = control_identity(control.thread_id(), control.device())
        .as_object()
        .cloned()
        .unwrap_or_default();
    obj.insert("kind".into(), Value::String("ok".into()));
    obj.insert("closed".into(), Value::Bool(closed));
    obj.insert(
        "reason".into(),
        Value::String(if closed { "closed" } else { "not_live" }.to_owned()),
    );
    obj.insert(
        "dropped_deliveries".into(),
        serde_json::to_value(&answer.dropped_deliveries).unwrap_or_else(|_| json!([])),
    );
    if let Some(g) = answer.graceful {
        obj.insert(
            "graceful".into(),
            serde_json::to_value(g).unwrap_or(Value::Null),
        );
    }
    Ok(Value::Object(obj))
}

fn same_model(left: &ThreadModel, right: &ThreadModel) -> bool {
    left.provider == right.provider && left.id == right.id
}

fn partial_model_switch(
    control: &dyn SessionControl,
    selected: &ThreadModel,
    previous: &SessionModelState,
    level: &str,
    reason: &str,
    detail: Option<String>,
) -> Value {
    let mut details = control_identity(control.thread_id(), control.device())
        .as_object()
        .cloned()
        .unwrap_or_default();
    details.insert("reason".into(), Value::String(reason.to_owned()));
    details.insert(
        "previous_model".into(),
        serde_json::to_value(&previous.model).unwrap_or(Value::Null),
    );
    details.insert(
        "previous_thinking_level".into(),
        Value::String(previous.thinking_level.clone()),
    );
    details.insert(
        "selected_model".into(),
        serde_json::to_value(selected).unwrap_or(Value::Null),
    );
    details.insert(
        "requested_thinking_level".into(),
        Value::String(level.to_owned()),
    );
    if let Some(d) = detail {
        details.insert("detail".into(), Value::String(d));
    }
    failure(
        "partial_commit",
        "The model and thinking change did not complete and the prior state could not be confirmed.",
        "Read the target's current model and thinking level before retrying.",
        Some(Value::Object(details)),
    )
}

async fn model_before_switch(
    control: &dyn SessionControl,
    level: Option<&str>,
) -> Result<Option<SessionModelState>, ControlError> {
    if level.is_none() {
        return Ok(None);
    }
    if let Some(previous) = control.get_model_state().await? {
        return Ok(Some(previous));
    }
    Err(refuse(
        "unsupported",
        "The target did not report a current model and thinking level to restore if the suffix fails.",
        "Retry when the target reports its active model and level, or switch the model without a thinking suffix.",
        Some(json!({ "reason": "model_state_unavailable" })),
    )
    .into())
}

async fn apply_thinking_level(
    control: &dyn SessionControl,
    level: &str,
) -> Result<Option<Value>, ControlError> {
    match control.set_thinking_level(level).await {
        Ok(()) => Ok(None),
        Err(ControlError::Refusal(r))
            if r.failure.code.as_str() == "thinking_level_unsupported" =>
        {
            let supported = control.get_available_thinking_levels().await?;
            Ok(Some(failure(
                "thinking_level_unsupported",
                format!("Thinking level \"{level}\" is not supported by the active model."),
                "Choose a level from the supported list and retry.",
                Some(json!({ "supported": supported })),
            )))
        }
        Err(other) => Err(other),
    }
}

async fn restore_model_state(
    control: &dyn SessionControl,
    selected: &ThreadModel,
    previous: &SessionModelState,
    level: &str,
) -> Option<Value> {
    let mut current = match control.get_model_state().await {
        Ok(Some(st)) => st,
        Ok(None) => {
            return Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_restore_failed",
                Some("model_state_unavailable".to_owned()),
            ));
        }
        Err(err) => {
            let detail = match err {
                ControlError::Refusal(r) => r.failure.code.as_str().to_owned(),
                ControlError::Transport(msg) => msg,
            };
            return Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_restore_failed",
                Some(detail),
            ));
        }
    };

    if !same_model(&current.model, &previous.model) {
        if !same_model(&current.model, selected) {
            return Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_restore_failed",
                Some("active_model_changed".to_owned()),
            ));
        }
        if let Err(err) = control
            .set_model(&previous.model.provider, &previous.model.id)
            .await
        {
            let detail = match err {
                ControlError::Refusal(r) => r.failure.code.as_str().to_owned(),
                ControlError::Transport(msg) => msg,
            };
            return Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_restore_failed",
                Some(detail),
            ));
        }
        current = match control.get_model_state().await {
            Ok(Some(st)) if same_model(&st.model, &previous.model) => st,
            _ => {
                return Some(partial_model_switch(
                    control,
                    selected,
                    previous,
                    level,
                    "model_restore_failed",
                    Some("restore_unconfirmed".to_owned()),
                ));
            }
        };
    }

    if current.thinking_level != previous.thinking_level {
        if let Err(err) = control.set_thinking_level(&previous.thinking_level).await {
            let detail = match err {
                ControlError::Refusal(r) => r.failure.code.as_str().to_owned(),
                ControlError::Transport(msg) => msg,
            };
            return Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_restore_failed",
                Some(detail),
            ));
        }
        match control.get_model_state().await {
            Ok(Some(restored))
                if same_model(&restored.model, &previous.model)
                    && restored.thinking_level == previous.thinking_level => {}
            _ => {
                return Some(partial_model_switch(
                    control,
                    selected,
                    previous,
                    level,
                    "model_restore_failed",
                    Some("restore_unconfirmed".to_owned()),
                ));
            }
        }
    }
    None
}

async fn apply_model_thinking(
    control: &dyn SessionControl,
    selected: &ThreadModel,
    previous: &SessionModelState,
    level: &str,
) -> Option<Value> {
    match control.get_model_state().await {
        Ok(Some(active)) if same_model(&active.model, selected) => {}
        Ok(_) => {
            return Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_switch_unconfirmed",
                None,
            ));
        }
        Err(err) => {
            let detail = match err {
                ControlError::Refusal(r) => r.failure.code.as_str().to_owned(),
                ControlError::Transport(msg) => msg,
            };
            return Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_switch_unconfirmed",
                Some(detail),
            ));
        }
    }

    match apply_thinking_level(control, level).await {
        Ok(None) => None,
        Ok(Some(refusal)) => restore_model_state(control, selected, previous, level)
            .await
            .or(Some(refusal)),
        Err(err) => {
            if let Some(partial) = restore_model_state(control, selected, previous, level).await {
                return Some(partial);
            }
            let detail = match err {
                ControlError::Refusal(r) => {
                    return Some(json!({ "kind": "error", "error": r.failure }));
                }
                ControlError::Transport(msg) => msg,
            };
            Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_restore_failed",
                Some(detail),
            ))
        }
    }
}

pub async fn set_model_on(
    control: &dyn SessionControl,
    model: &str,
    provider: Option<&str>,
) -> Result<Value, ControlError> {
    if let Some(err) = model_pattern_failure(model) {
        return Ok(err);
    }
    let first = control.get_available_models(false).await?;
    let mut matched = if first.held {
        if let Some(held_match) = exact_model(&first.models, model, provider) {
            (held_match, None)
        } else {
            let fresh = control.get_available_models(true).await?;
            match match_model(&fresh.models, model, provider) {
                ModelMatchResult::Match {
                    model,
                    thinking_level,
                } => (model, thinking_level),
                ModelMatchResult::Error(err) => return Ok(err),
            }
        }
    } else {
        match match_model(&first.models, model, provider) {
            ModelMatchResult::Match {
                model,
                thinking_level,
            } => (model, thinking_level),
            ModelMatchResult::Error(err) => return Ok(err),
        }
    };

    let mut previous = model_before_switch(control, matched.1.as_deref()).await?;
    let selected = match control.set_model(&matched.0.provider, &matched.0.id).await {
        Ok(s) => s,
        Err(ControlError::Refusal(refusal))
            if refusal.failure.code.as_str() != "idempotency_uncertain" =>
        {
            let Ok(fresh) = control.get_available_models(true).await else {
                return Err(ControlError::Refusal(refusal));
            };
            let rematched = match match_model(&fresh.models, model, provider) {
                ModelMatchResult::Match {
                    model,
                    thinking_level,
                } => (model, thinking_level),
                ModelMatchResult::Error(err) => return Ok(err),
            };
            if rematched.0.provider == matched.0.provider && rematched.0.id == matched.0.id {
                return Err(ControlError::Refusal(refusal));
            }
            match model_before_switch(control, rematched.1.as_deref()).await {
                Ok(prev) => previous = prev,
                Err(ControlError::Refusal(state_err))
                    if state_err
                        .failure
                        .details
                        .as_ref()
                        .and_then(|d| d.get("reason"))
                        .and_then(Value::as_str)
                        == Some("model_state_unavailable") =>
                {
                    return Err(ControlError::Refusal(refusal));
                }
                Err(other) => return Err(other),
            }
            let s = control
                .set_model(&rematched.0.provider, &rematched.0.id)
                .await?;
            matched = rematched;
            s
        }
        Err(other) => return Err(other),
    };

    if let (Some(prev), Some(level)) = (previous.as_ref(), matched.1.as_deref())
        && let Some(refusal) = apply_model_thinking(control, &selected, prev, level).await
    {
        return Ok(refusal);
    }

    let mut obj = control_identity(control.thread_id(), control.device())
        .as_object()
        .cloned()
        .unwrap_or_default();
    obj.insert("kind".into(), Value::String("ok".into()));
    obj.insert(
        "model".into(),
        json!({ "provider": selected.provider, "id": selected.id }),
    );
    Ok(Value::Object(obj))
}

pub async fn set_thinking_level_on(
    control: &dyn SessionControl,
    level: &str,
    scope: Option<&str>,
) -> Result<Value, ControlError> {
    if let Some(refusal) = apply_thinking_level(control, level).await? {
        return Ok(refusal);
    }
    let mut obj = control_identity(control.thread_id(), control.device())
        .as_object()
        .cloned()
        .unwrap_or_default();
    obj.insert("kind".into(), Value::String("ok".into()));
    obj.insert("level".into(), Value::String(level.to_owned()));
    obj.insert(
        "scope".into(),
        Value::String(scope.unwrap_or("session").to_owned()),
    );
    Ok(Value::Object(obj))
}
