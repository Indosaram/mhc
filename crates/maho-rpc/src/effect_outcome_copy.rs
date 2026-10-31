//! The user-facing copy of every effect outcome that is not a plain success (senpi
//! `rpc/effect-outcome-copy.ts`): the code, what happened in plain words, and what the caller can do. One
//! table so a client renders the same words the host wrote.
use crate::effect_record::{EffectOutcome, EffectOutcomeKind};

/// One row of the `EFFECT_OUTCOMES` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutcomeCopy {
    pub kind: EffectOutcomeKind,
    pub reason: &'static str,
    pub detail: &'static str,
    pub action: Option<&'static str>,
}

impl OutcomeCopy {
    pub fn outcome(&self) -> EffectOutcome {
        EffectOutcome {
            kind: self.kind,
            reason: Some(self.reason.to_owned()),
            detail: Some(self.detail.to_owned()),
            action: self.action.map(str::to_owned),
            abort_source: None,
            aborted_by: None,
        }
    }
}

/// senpi `EFFECT_OUTCOMES`, one constant per key.
pub mod effect_outcomes {
    use super::*;

    pub const INPUT_INTERRUPTED_BY_REOPEN: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::InterruptedByReopen,
        reason: "interrupted_by_reopen",
        detail: "the session was reopened while this input's turn was running, so that turn never finished",
        action: Some(
            "read the transcript for what the turn produced, then send the input again with a new clientMessageId",
        ),
    };

    pub const INPUT_CLEARED: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::Cleared,
        reason: "cleared",
        detail: "the queue was cleared (clear_queue, or abort or unload_session with clearQueue) before this queued input was delivered",
        action: Some("send the input again with a new clientMessageId if it is still needed"),
    };

    pub const QUEUED_INPUT_ALREADY_DELIVERED: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::AlreadyDelivered,
        reason: "already_delivered",
        detail: "the queued message had already been handed to the agent, so it can no longer be removed or edited",
        action: Some("send a follow-up message to correct it, or abort the turn"),
    };

    pub const QUEUED_INPUT_NOT_FOUND: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::NotFound,
        reason: "not_found",
        detail: "no queued message has this queuedId in this session",
        action: Some(
            "read queuedId values from get_state or queue_update and retry with one that is still queued",
        ),
    };

    pub const QUEUED_INPUT_CONFLICT: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::Conflict,
        reason: "conflict",
        detail: "the queued message was changed after the revision this request was based on, so nothing was changed",
        action: Some(
            "read the input's current text and revision from get_state or queue_update, then retry with that expectedRevision if the change still applies",
        ),
    };

    pub const INPUT_DROPPED_BY_REPLACE: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::DroppedByReplace,
        reason: "dropped_by_replace",
        detail: "the session was replaced (new_session, switch_session or fork) before this queued input was delivered",
        action: Some("send the input again with a new clientMessageId on the current session"),
    };

    pub const ABORT_INTERRUPTED_BY_REOPEN: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::InterruptedByReopen,
        reason: "interrupted_by_reopen",
        detail: "the session was reopened, so the turn this abort targeted is no longer running",
        action: None,
    };

    pub const SET_MODEL_INTERRUPTED_BY_REOPEN: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::InterruptedByReopen,
        reason: "interrupted_by_reopen",
        detail: "the host restarted before the model switch was applied",
        action: Some("send set_model again with a new effectId"),
    };

    pub const SET_MODEL_SUPERSEDED: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::Superseded,
        reason: "superseded",
        detail: "a newer model selection replaced this switch before it was applied",
        action: Some(
            "read the current model, then send set_model with a new effectId if you still want this model",
        ),
    };

    pub const SET_MODEL_DROPPED_BY_REPLACE: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::DroppedByReplace,
        reason: "dropped_by_replace",
        detail: "the session was replaced (new_session, switch_session or fork) before this model switch was applied",
        action: Some(
            "read the current model, then send set_model with a new effectId on the current session",
        ),
    };

    pub const SET_THINKING_INTERRUPTED_BY_REOPEN: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::InterruptedByReopen,
        reason: "interrupted_by_reopen",
        detail: "the host restarted before the thinking level was applied",
        action: Some("send set_thinking_level again with a new effectId"),
    };

    pub const DEQUEUE_INTERRUPTED_BY_REOPEN: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::InterruptedByReopen,
        reason: "interrupted_by_reopen",
        detail: "the session was reopened before the queued input was removed",
        action: Some("re-read queue_update.ordered and retry with a new effectId"),
    };

    pub const EDIT_QUEUED_INTERRUPTED_BY_REOPEN: OutcomeCopy = OutcomeCopy {
        kind: EffectOutcomeKind::InterruptedByReopen,
        reason: "interrupted_by_reopen",
        detail: "the session was reopened before the queued input was edited",
        action: Some("re-read queue_update.ordered and retry with a new effectId"),
    };
}

fn outcome(kind: EffectOutcomeKind, reason: &str, detail: String, action: &str) -> EffectOutcome {
    EffectOutcome {
        kind,
        reason: Some(reason.to_owned()),
        detail: Some(detail),
        action: Some(action.to_owned()),
        abort_source: None,
        aborted_by: None,
    }
}

pub fn unknown_model_outcome(provider: &str, model_id: &str) -> EffectOutcome {
    outcome(
        EffectOutcomeKind::UnknownModel,
        "unknown_model",
        format!("no available model is named {provider}/{model_id}"),
        "pick a model from get_available_models and send set_model again with a new effectId",
    )
}

pub fn provider_unavailable_outcome(detail: &str) -> EffectOutcome {
    outcome(
        EffectOutcomeKind::ProviderUnavailable,
        "provider_unavailable",
        detail.to_owned(),
        "sign in to the provider or pick another model, then send set_model again with a new effectId",
    )
}

pub fn context_budget_outcome(detail: &str) -> EffectOutcome {
    outcome(
        EffectOutcomeKind::ContextBudget,
        "context_budget",
        detail.to_owned(),
        "compact the session or pick a model with a larger context window, then send set_model with a new effectId",
    )
}

pub fn model_switch_error_outcome(detail: &str) -> EffectOutcome {
    outcome(
        EffectOutcomeKind::Error,
        "error",
        detail.to_owned(),
        "read the error, then send set_model again with a new effectId after correcting its cause",
    )
}

pub fn unsupported_thinking_level_outcome(detail: &str) -> EffectOutcome {
    outcome(
        EffectOutcomeKind::UnsupportedThinkingLevel,
        "unsupported_thinking_level",
        detail.to_owned(),
        "pick a level from get_available_thinking_levels and send it again with a new effectId",
    )
}

pub fn turn_error_outcome(detail: Option<&str>) -> EffectOutcome {
    outcome(
        EffectOutcomeKind::Error,
        "error",
        detail
            .unwrap_or("the turn ended with a provider error")
            .to_owned(),
        "read the error in the transcript, then retry the turn",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_non_success_row_carries_reason_and_detail() {
        for copy in [
            effect_outcomes::INPUT_INTERRUPTED_BY_REOPEN,
            effect_outcomes::INPUT_CLEARED,
            effect_outcomes::QUEUED_INPUT_ALREADY_DELIVERED,
            effect_outcomes::QUEUED_INPUT_NOT_FOUND,
            effect_outcomes::QUEUED_INPUT_CONFLICT,
            effect_outcomes::INPUT_DROPPED_BY_REPLACE,
            effect_outcomes::ABORT_INTERRUPTED_BY_REOPEN,
            effect_outcomes::SET_MODEL_INTERRUPTED_BY_REOPEN,
            effect_outcomes::SET_MODEL_SUPERSEDED,
            effect_outcomes::SET_MODEL_DROPPED_BY_REPLACE,
            effect_outcomes::SET_THINKING_INTERRUPTED_BY_REOPEN,
            effect_outcomes::DEQUEUE_INTERRUPTED_BY_REOPEN,
            effect_outcomes::EDIT_QUEUED_INTERRUPTED_BY_REOPEN,
        ] {
            assert!(!copy.reason.is_empty() && !copy.detail.is_empty());
            assert_ne!(copy.kind, EffectOutcomeKind::Completed);
        }
        assert_eq!(effect_outcomes::ABORT_INTERRUPTED_BY_REOPEN.action, None);
    }

    #[test]
    fn builders_fill_the_wire_shape() {
        let wire = serde_json::to_value(unknown_model_outcome("a", "m")).expect("serialize");
        assert_eq!(wire["kind"], "unknown_model");
        assert_eq!(wire["detail"], "no available model is named a/m");
        assert_eq!(
            turn_error_outcome(None).detail.as_deref(),
            Some("the turn ended with a provider error")
        );
        let cleared = effect_outcomes::INPUT_CLEARED.outcome();
        assert_eq!(
            serde_json::to_value(&cleared).expect("serialize")["kind"],
            "cleared"
        );
    }
}
