//! Port of senpi `packages/coding-agent/src/core/pending-input-projection.ts`.

use serde::Serialize;

use crate::pending_input_types::{
    DeliveryCertainty, DeliveryWitness, PendingInput, PendingInputFailure, PendingInputMode,
    PendingInputPhase, SupersededVersion,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingInputState {
    Queued,
    Edited,
    Sending,
    Delivered,
    Removed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingInputAction {
    Edit,
    Remove,
    Copy,
    Retry,
    CheckDelivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingInputStatusCode {
    QueuedFollowUp,
    QueuedSteer,
    Edited,
    Sending,
    Delivered,
    Applied,
    Removed,
    Failed,
    DeliveryUnconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputProjection {
    pub client_message_id: String,
    pub state: PendingInputState,
    pub text: String,
    pub requested_mode: PendingInputMode,
    pub revision: u64,
    pub status_code: PendingInputStatusCode,
    pub status_text: String,
    pub actions: Vec<PendingInputAction>,
    /// True while the input still waits; surfaces draw the pending outline only then.
    pub pending_decoration: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<PendingInputFailure>,
    pub superseded_versions: Vec<SupersededVersion>,
}

/// What every surface renders for one input; the actions offered are decided here, never by a client.
pub fn project_pending_input(input: &PendingInput) -> PendingInputProjection {
    use PendingInputAction::{CheckDelivery, Copy, Edit, Remove, Retry};
    let base = |state,
                status_code,
                status_text: String,
                actions: Vec<PendingInputAction>,
                pending_decoration| {
        PendingInputProjection {
            client_message_id: input.client_message_id.clone(),
            state,
            text: input.text.clone(),
            requested_mode: input.requested_mode,
            revision: input.revision,
            status_code,
            status_text,
            actions,
            pending_decoration,
            failure: None,
            superseded_versions: input.superseded.clone(),
        }
    };
    match input.phase {
        PendingInputPhase::Queued => {
            let (code, text) = queued_status(input.requested_mode);
            if input.has_edits {
                base(
                    PendingInputState::Edited,
                    PendingInputStatusCode::Edited,
                    format!("Edited - {text}"),
                    vec![Edit, Remove, Copy],
                    true,
                )
            } else {
                base(
                    PendingInputState::Queued,
                    code,
                    text.to_owned(),
                    vec![Edit, Remove, Copy],
                    true,
                )
            }
        }
        PendingInputPhase::Sending => base(
            PendingInputState::Sending,
            PendingInputStatusCode::Sending,
            "Sending".into(),
            vec![Copy],
            true,
        ),
        PendingInputPhase::Delivered => {
            if input.delivered_by == Some(DeliveryWitness::Handled) {
                base(
                    PendingInputState::Delivered,
                    PendingInputStatusCode::Applied,
                    "Applied".into(),
                    Vec::new(),
                    false,
                )
            } else {
                base(
                    PendingInputState::Delivered,
                    PendingInputStatusCode::Delivered,
                    String::new(),
                    Vec::new(),
                    false,
                )
            }
        }
        PendingInputPhase::Removed => base(
            PendingInputState::Removed,
            PendingInputStatusCode::Removed,
            "Removed".into(),
            vec![Copy],
            false,
        ),
        PendingInputPhase::Failed => match &input.failure {
            Some(failure) if failure.delivery == DeliveryCertainty::NotDelivered => {
                PendingInputProjection {
                    failure: Some(failure.clone()),
                    ..base(
                        PendingInputState::Failed,
                        PendingInputStatusCode::Failed,
                        format!("Failed: {}", failure.reason),
                        vec![Retry, Remove, Copy],
                        true,
                    )
                }
            }
            failure => PendingInputProjection {
                failure: failure.clone(),
                ..base(
                    PendingInputState::Failed,
                    PendingInputStatusCode::DeliveryUnconfirmed,
                    "Delivery could not be confirmed".into(),
                    vec![CheckDelivery, Copy],
                    true,
                )
            },
        },
    }
}

fn queued_status(mode: PendingInputMode) -> (PendingInputStatusCode, &'static str) {
    match mode {
        PendingInputMode::Steer => (
            PendingInputStatusCode::QueuedSteer,
            "Steering - reaches the agent at its next step",
        ),
        PendingInputMode::FollowUp => (
            PendingInputStatusCode::QueuedFollowUp,
            "Queued - sent after this turn",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending_input::fixtures::{edit, queued, replay, submit, submit_event, unsent};
    use crate::pending_input_types::PendingInputEvent;

    #[test]
    fn projects_queued_follow_up_and_steer_status() {
        let projection = project_pending_input(&queued());
        assert_eq!(
            projection.status_code,
            PendingInputStatusCode::QueuedFollowUp
        );
        assert_eq!(projection.status_text, "Queued - sent after this turn");
        let mut steer = submit_event();
        steer.requested_mode = PendingInputMode::Steer;
        let steering = replay(&[PendingInputEvent::Submit(steer)]);
        assert_eq!(
            project_pending_input(&steering).status_code,
            PendingInputStatusCode::QueuedSteer
        );
    }

    #[test]
    fn projects_edited_and_failed_status_text() {
        let edited = replay(&[submit(), edit("e1", 1, "x")]);
        assert_eq!(
            project_pending_input(&edited).status_text,
            "Edited - Queued - sent after this turn"
        );
        let failed = project_pending_input(&unsent());
        assert_eq!(failed.status_text, "Failed: target closed");
        assert_eq!(
            failed.actions,
            vec![
                PendingInputAction::Retry,
                PendingInputAction::Remove,
                PendingInputAction::Copy
            ]
        );
    }
}
