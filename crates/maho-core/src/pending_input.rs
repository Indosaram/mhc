//! Port of senpi `packages/coding-agent/src/core/pending-input.ts`.

use crate::pending_input_mutations::{MutationEvent, answer, reduce_mutation};
use crate::pending_input_types::{
    CancelCause, DeliveryCertainty, DeliveryToken, DeliveryWitness, OperationOutcome, PendingInput,
    PendingInputDecision, PendingInputEffect, PendingInputEvent, PendingInputFailure,
    PendingInputHandle, PendingInputOperationResult, PendingInputPhase, RemovalCause, SubmitEvent,
};

/// The one transition function of a pending input (senpi#3549); it never performs I/O.
pub fn reduce_pending_input(
    current: Option<&PendingInput>,
    event: &PendingInputEvent,
) -> PendingInputDecision {
    if let PendingInputEvent::Submit(submit_event) = event {
        return admit_submission(current, submit_event);
    }
    if let Some(mutation) = MutationEvent::from_event(event) {
        return reduce_mutation(current, &mutation);
    }
    match current {
        None => unchanged(None),
        Some(current) => reduce_lifecycle(current, event),
    }
}

fn admit_submission(current: Option<&PendingInput>, event: &SubmitEvent) -> PendingInputDecision {
    if let Some(current) = current {
        if current.fingerprint != event.fingerprint {
            return PendingInputDecision {
                next: Some(current.clone()),
                result: Some(answer(current, OperationOutcome::ClientMessageIdConflict)),
                effects: Vec::new(),
            };
        }
        return PendingInputDecision {
            next: Some(current.clone()),
            result: Some(PendingInputOperationResult {
                replayed: Some(true),
                text: Some(current.text.clone()),
                ..answer(current, OperationOutcome::Replayed)
            }),
            effects: Vec::new(),
        };
    }
    let next = PendingInput {
        client_message_id: event.client_message_id.clone(),
        fingerprint: event.fingerprint.clone(),
        handle: event.handle.clone(),
        revision: 1,
        has_edits: false,
        text: event.text.clone(),
        attachments: event.attachments.clone(),
        requested_mode: event.requested_mode,
        phase: PendingInputPhase::Queued,
        failure: None,
        removed_by: None,
        delivered_by: None,
        attempt: None,
        attempt_count: 0,
        superseded: Vec::new(),
        operations: Default::default(),
    };
    let result = answer(&next, OperationOutcome::Admitted);
    PendingInputDecision {
        next: Some(next),
        result: Some(result),
        effects: vec![PendingInputEffect::Persist],
    }
}

fn reduce_lifecycle(current: &PendingInput, event: &PendingInputEvent) -> PendingInputDecision {
    let when = |condition: bool, next: PendingInput| {
        if condition {
            persist(next)
        } else {
            unchanged(Some(current))
        }
    };
    match event {
        PendingInputEvent::RejectBeforeDispatch { reason } => when(
            current.phase == PendingInputPhase::Queued,
            fail(current, DeliveryCertainty::NotDelivered, reason),
        ),
        PendingInputEvent::Reserve { token } => {
            if current.phase != PendingInputPhase::Queued || !token_fits_input(current, token) {
                return unchanged(Some(current));
            }
            let mut next = current.clone();
            next.phase = PendingInputPhase::Sending;
            next.attempt = Some(token.clone());
            next.attempt_count += 1;
            PendingInputDecision {
                next: Some(next),
                result: None,
                effects: vec![
                    PendingInputEffect::Persist,
                    PendingInputEffect::Dispatch {
                        token: token.clone(),
                    },
                ],
            }
        }
        PendingInputEvent::Release { token } => {
            if !holds_attempt(current, token) {
                return unchanged(Some(current));
            }
            let mut next = current.clone();
            next.attempt = None;
            next.phase = PendingInputPhase::Queued;
            persist(next)
        }
        PendingInputEvent::Consumed { token } => when(
            holds_attempt(current, token),
            deliver(current, DeliveryWitness::Consumed),
        ),
        PendingInputEvent::HandledToken { token } => when(
            holds_attempt(current, token),
            deliver(current, DeliveryWitness::Handled),
        ),
        // Without a token the receipt must name the lifetime of the still-queued input it settles.
        PendingInputEvent::HandledLifetime { lifetime_id } => when(
            current.phase == PendingInputPhase::Queued
                && *lifetime_id == current.handle.lifetime_id,
            deliver(current, DeliveryWitness::Handled),
        ),
        PendingInputEvent::Refused { token, reason } => when(
            holds_attempt(current, token),
            fail(current, DeliveryCertainty::NotDelivered, reason),
        ),
        PendingInputEvent::Ambiguous { token, reason } => when(
            holds_attempt(current, token),
            fail(current, DeliveryCertainty::Uncertain, reason),
        ),
        PendingInputEvent::OwnerLost { token } => when(
            holds_attempt(current, token),
            fail(
                current,
                DeliveryCertainty::Uncertain,
                "the delivering owner was lost",
            ),
        ),
        PendingInputEvent::Witnessed { attempt_id } => when(
            uncertain_about(current, attempt_id),
            deliver(current, DeliveryWitness::Consumed),
        ),
        PendingInputEvent::NonDeliveryProven { attempt_id } => {
            let reason = current
                .failure
                .as_ref()
                .map(|failure| failure.reason.clone())
                .unwrap_or_default();
            when(
                uncertain_about(current, attempt_id),
                fail(current, DeliveryCertainty::NotDelivered, &reason),
            )
        }
        PendingInputEvent::Teardown => when(
            current.phase == PendingInputPhase::Sending,
            fail(
                current,
                DeliveryCertainty::Uncertain,
                "torn down during handoff",
            ),
        ),
        PendingInputEvent::Cancel {
            cause,
            consumer_fenced,
        } => cancel(current, *cause, *consumer_fenced),
        PendingInputEvent::Rebind {
            handle,
            attempts_unsent,
        } => rebind(current, handle, *attempts_unsent),
        PendingInputEvent::Submit(_)
        | PendingInputEvent::Edit { .. }
        | PendingInputEvent::Remove { .. }
        | PendingInputEvent::Retry { .. } => unchanged(Some(current)),
    }
}

fn cancel(
    current: &PendingInput,
    cause: CancelCause,
    consumer_fenced: bool,
) -> PendingInputDecision {
    let removable = current.phase == PendingInputPhase::Queued
        || (current.phase == PendingInputPhase::Sending && consumer_fenced);
    if !removable {
        return if current.phase == PendingInputPhase::Sending {
            persist(fail(
                current,
                DeliveryCertainty::Uncertain,
                "stopped before the handoff was confirmed",
            ))
        } else {
            unchanged(Some(current))
        };
    }
    let mut next = current.clone();
    next.phase = PendingInputPhase::Removed;
    next.removed_by = Some(RemovalCause::from(cause));
    let next = settled(next);
    let mut effects = vec![PendingInputEffect::Persist];
    if cause == CancelCause::Stop {
        effects.push(PendingInputEffect::RestoreToDraft {
            text: next.text.clone(),
        });
    }
    PendingInputDecision {
        next: Some(next),
        result: None,
        effects,
    }
}

fn rebind(
    current: &PendingInput,
    handle: &PendingInputHandle,
    attempts_unsent: bool,
) -> PendingInputDecision {
    if matches!(
        current.phase,
        PendingInputPhase::Delivered | PendingInputPhase::Removed
    ) {
        return unchanged(Some(current));
    }
    let mut rebound = current.clone();
    rebound.handle = handle.clone();
    rebound.revision = 1;
    if current.phase != PendingInputPhase::Sending {
        return persist(rebound);
    }
    if !attempts_unsent {
        return persist(fail(
            &rebound,
            DeliveryCertainty::Uncertain,
            "the session reopened during handoff",
        ));
    }
    rebound.attempt = None;
    rebound.phase = PendingInputPhase::Queued;
    persist(rebound)
}

fn token_fits_input(input: &PendingInput, token: &DeliveryToken) -> bool {
    token.lifetime_id == input.handle.lifetime_id
        && token.client_message_id == input.client_message_id
}

fn holds_attempt(input: &PendingInput, token: &DeliveryToken) -> bool {
    input.phase == PendingInputPhase::Sending
        && input.attempt.as_ref().is_some_and(|attempt| {
            attempt.attempt_id == token.attempt_id
                && attempt.owner_id == token.owner_id
                && attempt.writer_generation == token.writer_generation
        })
        && token_fits_input(input, token)
}

fn deliver(input: &PendingInput, delivered_by: DeliveryWitness) -> PendingInput {
    let mut next = input.clone();
    next.phase = PendingInputPhase::Delivered;
    next.delivered_by = Some(delivered_by);
    settled(next)
}

fn fail(input: &PendingInput, delivery: DeliveryCertainty, reason: &str) -> PendingInput {
    let failed_attempt = input
        .attempt
        .clone()
        .or_else(|| input.failure.as_ref().and_then(|f| f.attempt.clone()));
    let mut next = input.clone();
    next.attempt = None;
    next.phase = PendingInputPhase::Failed;
    next.failure = Some(PendingInputFailure {
        delivery,
        reason: reason.to_owned(),
        attempt: failed_attempt,
    });
    next
}

/// Late evidence only settles the uncertain failure of the exact attempt it names.
fn uncertain_about(input: &PendingInput, attempt_id: &str) -> bool {
    input.phase == PendingInputPhase::Failed
        && input.failure.as_ref().is_some_and(|failure| {
            failure.delivery == DeliveryCertainty::Uncertain
                && failure
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.attempt_id == attempt_id)
        })
}

fn settled(mut input: PendingInput) -> PendingInput {
    input.attempt = None;
    input.failure = None;
    input
}

fn persist(next: PendingInput) -> PendingInputDecision {
    PendingInputDecision {
        next: Some(next),
        result: None,
        effects: vec![PendingInputEffect::Persist],
    }
}

fn unchanged(current: Option<&PendingInput>) -> PendingInputDecision {
    PendingInputDecision {
        next: current.cloned(),
        result: None,
        effects: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! senpi `test/suite/pending-input.fixtures.ts`.
    use super::*;
    use crate::pending_input_types::{MutationTarget, PendingInputMode};

    pub const CLIENT_ID: &str = "c-1";

    pub fn handle() -> PendingInputHandle {
        PendingInputHandle {
            lifetime_id: "life-1".into(),
            queued_id: "q_1".into(),
        }
    }

    pub fn reopened() -> PendingInputHandle {
        PendingInputHandle {
            lifetime_id: "life-2".into(),
            queued_id: "q_7".into(),
        }
    }

    pub fn submit_event() -> SubmitEvent {
        SubmitEvent {
            client_message_id: CLIENT_ID.into(),
            fingerprint: "fp-original".into(),
            handle: handle(),
            text: "check the logs".into(),
            attachments: Vec::new(),
            requested_mode: PendingInputMode::FollowUp,
        }
    }

    pub fn submit() -> PendingInputEvent {
        PendingInputEvent::Submit(submit_event())
    }

    pub fn token_with(attempt_id: &str, edit: impl FnOnce(&mut DeliveryToken)) -> DeliveryToken {
        let mut token = DeliveryToken {
            owner_id: "owner-1".into(),
            writer_generation: 1,
            lifetime_id: handle().lifetime_id,
            attempt_id: attempt_id.into(),
            client_message_id: CLIENT_ID.into(),
        };
        edit(&mut token);
        token
    }

    pub fn token(attempt_id: &str) -> DeliveryToken {
        token_with(attempt_id, |_| {})
    }

    pub fn replay(events: &[PendingInputEvent]) -> PendingInput {
        let mut current: Option<PendingInput> = None;
        for event in events {
            current = reduce_pending_input(current.as_ref(), event).next;
        }
        current.unwrap_or_else(|| panic!("the events admitted no input"))
    }

    pub fn queued() -> PendingInput {
        replay(&[submit()])
    }

    pub fn sending(attempt_id: &str) -> PendingInput {
        replay(&[
            submit(),
            PendingInputEvent::Reserve {
                token: token(attempt_id),
            },
        ])
    }

    pub fn uncertain() -> PendingInput {
        replay(&[
            submit(),
            PendingInputEvent::Reserve {
                token: token("a-1"),
            },
            PendingInputEvent::Ambiguous {
                token: token("a-1"),
                reason: "handoff lost".into(),
            },
        ])
    }

    pub fn unsent() -> PendingInput {
        replay(&[
            submit(),
            PendingInputEvent::Reserve {
                token: token("a-1"),
            },
            PendingInputEvent::Refused {
                token: token("a-1"),
                reason: "target closed".into(),
            },
        ])
    }

    pub fn edit_with(
        effect_id: &str,
        expected_revision: u64,
        text: &str,
        handle: PendingInputHandle,
    ) -> PendingInputEvent {
        PendingInputEvent::Edit {
            target: MutationTarget {
                effect_id: effect_id.into(),
                handle,
                expected_revision,
            },
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    pub fn edit(effect_id: &str, expected_revision: u64, text: &str) -> PendingInputEvent {
        edit_with(effect_id, expected_revision, text, handle())
    }

    pub fn remove(effect_id: &str, expected_revision: u64) -> PendingInputEvent {
        PendingInputEvent::Remove {
            target: MutationTarget {
                effect_id: effect_id.into(),
                handle: handle(),
                expected_revision,
            },
        }
    }

    pub fn retry(effect_id: &str) -> PendingInputEvent {
        PendingInputEvent::Retry {
            effect_id: effect_id.into(),
            handle: handle(),
        }
    }

    pub fn delivered() -> PendingInput {
        replay(&[
            submit(),
            PendingInputEvent::Reserve {
                token: token("a-1"),
            },
            PendingInputEvent::Consumed {
                token: token("a-1"),
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    //! senpi `test/suite/pending-input.test.ts` and `pending-input-delivery.test.ts`.
    use super::fixtures::*;
    use super::*;
    use crate::pending_input_projection::{
        PendingInputAction, PendingInputState, PendingInputStatusCode, project_pending_input,
    };
    use crate::pending_input_types::SupersededVersion;

    fn outcome(decision: &PendingInputDecision) -> Option<OperationOutcome> {
        decision.result.as_ref().map(|result| result.outcome)
    }

    fn reduce(current: &PendingInput, event: PendingInputEvent) -> PendingInputDecision {
        reduce_pending_input(Some(current), &event)
    }

    fn phase(decision: &PendingInputDecision) -> Option<PendingInputPhase> {
        decision.next.as_ref().map(|next| next.phase)
    }

    // --- admission and changes ---

    #[test]
    fn admits_one_identified_input_into_durable_custody_t01() {
        let decision = reduce_pending_input(None, &submit());
        let next = decision.next.clone().unwrap_or_else(queued);
        assert_eq!(next.client_message_id, CLIENT_ID);
        assert_eq!(
            (next.phase, next.revision, next.has_edits),
            (PendingInputPhase::Queued, 1, false)
        );
        assert_eq!(outcome(&decision), Some(OperationOutcome::Admitted));
        assert_eq!(decision.effects, vec![PendingInputEffect::Persist]);
    }

    #[test]
    fn retains_a_definitively_rejected_input_for_safe_retry_t02() {
        let decision = reduce(
            &queued(),
            PendingInputEvent::RejectBeforeDispatch {
                reason: "target unavailable".into(),
            },
        );
        let next = decision.next.clone().unwrap_or_else(queued);
        assert_eq!(next.phase, PendingInputPhase::Failed);
        assert_eq!(
            next.failure.as_ref().map(|f| f.delivery),
            Some(DeliveryCertainty::NotDelivered)
        );
        assert_eq!(next.text, submit_event().text);
        assert!(
            project_pending_input(&next)
                .actions
                .contains(&PendingInputAction::Retry)
        );
    }

    #[test]
    fn replays_original_submission_without_restoring_old_edited_text_t03() {
        let edited = replay(&[submit(), edit("e1", 1, "check the error logs")]);
        let decision = reduce(&edited, submit());
        let result = decision.result.clone().unwrap_or_else(|| panic!("result"));
        assert_eq!(result.outcome, OperationOutcome::Replayed);
        assert_eq!(result.replayed, Some(true));
        assert_eq!(result.text.as_deref(), Some("check the error logs"));
        assert_eq!(decision.next.as_ref(), Some(&edited));
        assert!(decision.effects.is_empty());
    }

    #[test]
    fn rejects_conflicting_reuse_of_a_submission_id_t04() {
        let current = queued();
        let mut other = submit_event();
        other.fingerprint = "fp-other".into();
        other.text = "something else".into();
        let decision = reduce(&current, PendingInputEvent::Submit(other));
        assert_eq!(
            outcome(&decision),
            Some(OperationOutcome::ClientMessageIdConflict)
        );
        assert_eq!(decision.next.as_ref(), Some(&current));
        assert!(decision.effects.is_empty());
    }

    #[test]
    fn edits_in_place_and_atomically_supersedes_the_old_content_t08() {
        let decision = reduce(&queued(), edit("e1", 1, "check the error logs"));
        let next = decision.next.clone().unwrap_or_else(queued);
        assert_eq!(
            (next.phase, next.revision, next.has_edits),
            (PendingInputPhase::Queued, 2, true)
        );
        assert_eq!(next.text, "check the error logs");
        assert_eq!(
            next.superseded,
            vec![SupersededVersion {
                lifetime_id: "life-1".into(),
                revision: 1,
                text: submit_event().text
            }]
        );
        let result = decision.result.unwrap_or_else(|| panic!("result"));
        assert_eq!(
            (result.outcome, result.revision),
            (OperationOutcome::Edited, Some(2))
        );
        assert_eq!(result.text.as_deref(), Some("check the error logs"));
    }

    #[test]
    fn keeps_identity_order_and_mode_across_repeated_edits_t09() {
        let twice = replay(&[submit(), edit("e1", 1, "second"), edit("e2", 2, "third")]);
        assert_eq!(twice.client_message_id, CLIENT_ID);
        assert_eq!(twice.handle, handle());
        assert_eq!(
            twice.requested_mode,
            crate::pending_input_types::PendingInputMode::FollowUp
        );
        assert_eq!(twice.revision, 3);
        assert_eq!(
            twice
                .superseded
                .iter()
                .map(|v| v.revision)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            project_pending_input(&twice).state,
            PendingInputState::Edited
        );
    }

    #[test]
    fn removes_only_the_revision_the_user_named_t10() {
        let decision = reduce(&queued(), remove("r1", 1));
        assert_eq!(phase(&decision), Some(PendingInputPhase::Removed));
        assert_eq!(
            decision.next.as_ref().and_then(|n| n.removed_by),
            Some(RemovalCause::Remove)
        );
        assert_eq!(outcome(&decision), Some(OperationOutcome::Removed));
    }

    #[test]
    fn refuses_stale_changes_without_replacing_the_expected_revision_t11() {
        let current = replay(&[submit(), edit("e1", 1, "newer")]);
        let decision = reduce(&current, edit("e2", 1, "stale draft"));
        assert_eq!(outcome(&decision), Some(OperationOutcome::Conflict));
        assert_eq!(
            decision.result.as_ref().and_then(|r| r.current_revision),
            Some(2)
        );
        assert_eq!(decision.next.map(|n| n.text).as_deref(), Some("newer"));
    }

    #[test]
    fn refuses_mutation_during_handoff_without_claiming_delivery_t12() {
        let decision = reduce(&sending("a-1"), edit("e1", 1, "too late"));
        assert_eq!(outcome(&decision), Some(OperationOutcome::Sending));
        assert_eq!(phase(&decision), Some(PendingInputPhase::Sending));
    }

    #[test]
    fn uses_gone_precedence_before_stale_revision_conflict_t13() {
        let removed = replay(&[submit(), remove("r1", 1)]);
        assert_eq!(
            outcome(&reduce(&removed, edit("e1", 9, "x"))),
            Some(OperationOutcome::NotFound)
        );
        assert_eq!(
            outcome(&reduce(&delivered(), remove("r2", 9))),
            Some(OperationOutcome::AlreadyDelivered)
        );
    }

    #[test]
    fn retries_the_same_input_with_one_new_attempt_and_no_revision_bump_t14() {
        let decision = reduce(&unsent(), retry("t1"));
        let next = decision.next.clone().unwrap_or_else(queued);
        assert_eq!(
            (next.phase, next.revision, next.attempt_count),
            (PendingInputPhase::Queued, 1, 1)
        );
        assert!(next.failure.is_none());
        assert_eq!(outcome(&decision), Some(OperationOutcome::Requeued));
    }

    #[test]
    fn blocks_retry_until_non_delivery_is_proven_t15() {
        assert_eq!(
            outcome(&reduce(&uncertain(), retry("t1"))),
            Some(OperationOutcome::NotRetryable)
        );
        assert_eq!(
            outcome(&reduce(&queued(), retry("t2"))),
            Some(OperationOutcome::NotRetryable)
        );
    }

    #[test]
    fn prevents_a_superseded_version_from_becoming_runnable_again_t39() {
        let current = replay(&[submit(), edit("e1", 1, "current")]);
        let reserved = reduce(
            &current,
            PendingInputEvent::Reserve {
                token: token("a-1"),
            },
        );
        assert_eq!(reserved.next.map(|n| n.text).as_deref(), Some("current"));
        assert_eq!(
            outcome(&reduce(&current, edit("e2", 1, &submit_event().text))),
            Some(OperationOutcome::Conflict)
        );
    }

    #[test]
    fn allows_discarding_a_proven_unsent_failure_without_replay_t41() {
        let decision = reduce(&unsent(), remove("r1", 1));
        assert_eq!(phase(&decision), Some(PendingInputPhase::Removed));
        assert_eq!(decision.effects, vec![PendingInputEffect::Persist]);
    }

    #[test]
    fn rejects_stale_lifetime_handles_and_foreign_scope_completions_t42() {
        let reopened_input = reduce(
            &queued(),
            PendingInputEvent::Rebind {
                handle: reopened(),
                attempts_unsent: false,
            },
        )
        .next
        .unwrap_or_else(queued);
        assert_eq!(
            outcome(&reduce(&reopened_input, edit("e1", 1, "x"))),
            Some(OperationOutcome::StaleHandle)
        );
        let intruder = token_with("a-1", |t| t.owner_id = "intruder".into());
        assert_eq!(
            phase(&reduce(
                &sending("a-1"),
                PendingInputEvent::Consumed { token: intruder }
            )),
            Some(PendingInputPhase::Sending)
        );
    }

    #[test]
    fn replays_the_complete_original_effect_answer_across_reopen_t43() {
        let edited = replay(&[submit(), edit("e1", 1, "fixed")]);
        let reopened_input = reduce(
            &edited,
            PendingInputEvent::Rebind {
                handle: reopened(),
                attempts_unsent: false,
            },
        )
        .next
        .unwrap_or_else(|| edited.clone());
        let again = reduce(&reopened_input, edit("e1", 1, "fixed"));
        let result = again.result.unwrap_or_else(|| panic!("result"));
        assert_eq!(result.outcome, OperationOutcome::Edited);
        assert_eq!(result.revision, Some(2));
        assert_eq!(result.text.as_deref(), Some("fixed"));
        assert_eq!(result.lifetime_id.as_deref(), Some("life-1"));
        assert_eq!(result.replayed, Some(true));
        assert_eq!(
            outcome(&reduce(&reopened_input, edit("e1", 1, "different"))),
            Some(OperationOutcome::EffectIdConflict)
        );
    }

    // --- delivery and custody ---

    #[test]
    fn freezes_the_current_version_before_handing_it_to_the_consumer_t05() {
        let decision = reduce(
            &queued(),
            PendingInputEvent::Reserve {
                token: token("a-1"),
            },
        );
        let next = decision.next.clone().unwrap_or_else(queued);
        assert_eq!(
            (next.phase, next.attempt.clone(), next.attempt_count),
            (PendingInputPhase::Sending, Some(token("a-1")), 1)
        );
        assert_eq!(
            decision.effects,
            vec![
                PendingInputEffect::Persist,
                PendingInputEffect::Dispatch {
                    token: token("a-1")
                }
            ]
        );
        assert_eq!(
            project_pending_input(&next).actions,
            vec![PendingInputAction::Copy]
        );
    }

    #[test]
    fn delivers_only_the_committed_input_and_retires_its_queue_handle_t06() {
        let decision = reduce(
            &sending("a-1"),
            PendingInputEvent::Consumed {
                token: token("a-1"),
            },
        );
        let next = decision.next.unwrap_or_else(queued);
        assert_eq!(
            (next.phase, next.delivered_by),
            (
                PendingInputPhase::Delivered,
                Some(DeliveryWitness::Consumed)
            )
        );
        assert!(next.attempt.is_none());
        let projection = project_pending_input(&next);
        assert!(!projection.pending_decoration);
        assert!(projection.actions.is_empty());
    }

    #[test]
    fn settles_handled_commands_without_a_fabricated_user_entry_t07() {
        let decision = reduce(
            &queued(),
            PendingInputEvent::HandledLifetime {
                lifetime_id: "life-1".into(),
            },
        );
        let next = decision.next.unwrap_or_else(queued);
        assert_eq!(
            (next.phase, next.delivered_by),
            (PendingInputPhase::Delivered, Some(DeliveryWitness::Handled))
        );
        assert_eq!(
            project_pending_input(&next).status_code,
            PendingInputStatusCode::Applied
        );
    }

    #[test]
    fn fails_safely_only_when_the_attempt_cannot_have_been_consumed_t16() {
        let decision = reduce(
            &sending("a-1"),
            PendingInputEvent::Refused {
                token: token("a-1"),
                reason: "target closed".into(),
            },
        );
        let failure = decision
            .next
            .and_then(|n| n.failure)
            .unwrap_or_else(|| panic!("failure"));
        assert_eq!(
            (failure.delivery, failure.reason.as_str()),
            (DeliveryCertainty::NotDelivered, "target closed")
        );
    }

    #[test]
    fn retains_ambiguous_delivery_without_enabling_resend_t17() {
        let projection = project_pending_input(&uncertain());
        assert_eq!(
            (projection.state, projection.status_code),
            (
                PendingInputState::Failed,
                PendingInputStatusCode::DeliveryUnconfirmed
            )
        );
        assert!(!projection.actions.contains(&PendingInputAction::Retry));
        assert!(!projection.actions.contains(&PendingInputAction::Remove));
    }

    #[test]
    fn settles_uncertainty_from_an_identity_bearing_delivery_witness_t18() {
        assert_eq!(
            phase(&reduce(
                &uncertain(),
                PendingInputEvent::Witnessed {
                    attempt_id: "a-1".into()
                }
            )),
            Some(PendingInputPhase::Delivered)
        );
    }

    #[test]
    fn enables_retry_only_after_complete_fenced_non_delivery_evidence_t19() {
        let proven = reduce(
            &uncertain(),
            PendingInputEvent::NonDeliveryProven {
                attempt_id: "a-1".into(),
            },
        )
        .next
        .unwrap_or_else(uncertain);
        assert_eq!(
            proven.failure.as_ref().map(|f| f.delivery),
            Some(DeliveryCertainty::NotDelivered)
        );
        assert!(
            project_pending_input(&proven)
                .actions
                .contains(&PendingInputAction::Retry)
        );
    }

    #[test]
    fn preserves_pending_input_across_every_non_explicit_teardown_t20() {
        let current = replay(&[submit(), edit("e1", 1, "edited")]);
        let decision = reduce(&current, PendingInputEvent::Teardown);
        assert_eq!(decision.next.as_ref(), Some(&current));
        assert!(decision.effects.is_empty());
    }

    #[test]
    fn releases_a_stale_reservation_without_deleting_the_input_t21() {
        let next = reduce(
            &sending("a-1"),
            PendingInputEvent::OwnerLost {
                token: token("a-1"),
            },
        )
        .next
        .unwrap_or_else(queued);
        assert_eq!(next.phase, PendingInputPhase::Failed);
        assert_eq!(
            next.failure.as_ref().map(|f| f.delivery),
            Some(DeliveryCertainty::Uncertain)
        );
        assert_eq!(next.text, submit_event().text);
        assert!(next.attempt.is_none());
    }

    #[test]
    fn cancels_the_proven_pre_cutoff_batch_and_reports_dropped_ids_t22() {
        let decision = reduce(
            &queued(),
            PendingInputEvent::Cancel {
                cause: CancelCause::Stop,
                consumer_fenced: false,
            },
        );
        assert_eq!(
            decision.next.as_ref().map(|n| (n.phase, n.removed_by)),
            Some((PendingInputPhase::Removed, Some(RemovalCause::Stop)))
        );
        assert!(
            decision
                .effects
                .contains(&PendingInputEffect::RestoreToDraft {
                    text: submit_event().text
                })
        );
    }

    #[test]
    fn keeps_a_consumed_input_out_of_the_stop_restore_receipt_t23() {
        let delivered_input = delivered();
        let decision = reduce(
            &delivered_input,
            PendingInputEvent::Cancel {
                cause: CancelCause::Stop,
                consumer_fenced: false,
            },
        );
        assert_eq!(decision.next.as_ref(), Some(&delivered_input));
        assert!(decision.effects.is_empty());
    }

    #[test]
    fn removes_an_in_flight_input_only_after_its_consumer_is_fenced_t24() {
        let decision = reduce(
            &sending("a-1"),
            PendingInputEvent::Cancel {
                cause: CancelCause::Stop,
                consumer_fenced: true,
            },
        );
        assert_eq!(
            decision.next.as_ref().map(|n| (n.phase, n.removed_by)),
            Some((PendingInputPhase::Removed, Some(RemovalCause::Stop)))
        );
        assert!(
            decision
                .effects
                .contains(&PendingInputEffect::RestoreToDraft {
                    text: submit_event().text
                })
        );
    }

    #[test]
    fn does_not_restore_text_after_an_unconfirmed_drain_t25() {
        let decision = reduce(
            &sending("a-1"),
            PendingInputEvent::Cancel {
                cause: CancelCause::Stop,
                consumer_fenced: false,
            },
        );
        assert_eq!(
            decision
                .next
                .as_ref()
                .and_then(|n| n.failure.as_ref())
                .map(|f| f.delivery),
            Some(DeliveryCertainty::Uncertain)
        );
        assert!(
            !decision
                .effects
                .iter()
                .any(|effect| matches!(effect, PendingInputEffect::RestoreToDraft { .. }))
        );
    }

    #[test]
    fn ignores_a_delivery_event_whose_token_differs_from_the_reservation() {
        let stale_tokens = [
            token_with("a-1", |t| t.writer_generation = 2),
            token_with("a-1", |t| t.lifetime_id = "life-2".into()),
            token_with("a-1", |t| t.owner_id = "owner-2".into()),
            token("a-2"),
        ];
        for stale in stale_tokens {
            for event in [
                PendingInputEvent::Consumed {
                    token: stale.clone(),
                },
                PendingInputEvent::Release {
                    token: stale.clone(),
                },
                PendingInputEvent::Refused {
                    token: stale.clone(),
                    reason: "x".into(),
                },
                PendingInputEvent::Ambiguous {
                    token: stale.clone(),
                    reason: "x".into(),
                },
            ] {
                let next = reduce(&sending("a-1"), event).next.unwrap_or_else(queued);
                assert_eq!(
                    (next.phase, next.attempt),
                    (PendingInputPhase::Sending, Some(token("a-1")))
                );
            }
        }
    }

    #[test]
    fn reserves_only_with_a_token_of_the_inputs_current_lifetime() {
        let decision = reduce(
            &queued(),
            PendingInputEvent::Reserve {
                token: token_with("a-1", |t| t.lifetime_id = "life-0".into()),
            },
        );
        assert_eq!(phase(&decision), Some(PendingInputPhase::Queued));
        assert!(decision.effects.is_empty());
    }

    #[test]
    fn does_not_settle_an_in_flight_input_from_a_handled_receipt_without_its_token() {
        assert_eq!(
            phase(&reduce(
                &sending("a-1"),
                PendingInputEvent::HandledLifetime {
                    lifetime_id: "life-1".into()
                }
            )),
            Some(PendingInputPhase::Sending)
        );
    }

    #[test]
    fn names_the_lifetime_of_each_superseded_revision_since_revisions_restart_on_reopen() {
        let edited = replay(&[submit(), edit("e1", 1, "edited")]);
        let rebound = reduce(
            &edited,
            PendingInputEvent::Rebind {
                handle: reopened(),
                attempts_unsent: false,
            },
        )
        .next
        .unwrap_or_else(queued);
        assert_eq!((rebound.revision, rebound.handle.clone()), (1, reopened()));
        assert_eq!(
            rebound.superseded,
            vec![SupersededVersion {
                lifetime_id: "life-1".into(),
                revision: 1,
                text: submit_event().text
            }]
        );
    }

    #[test]
    fn replays_only_an_uncommitted_native_attempt_with_the_same_id_t28() {
        let released = reduce(
            &sending("a-1"),
            PendingInputEvent::Release {
                token: token("a-1"),
            },
        )
        .next
        .unwrap_or_else(queued);
        assert_eq!(
            (released.phase, released.revision),
            (PendingInputPhase::Queued, 1)
        );
        assert!(released.attempt.is_none());
        let again = reduce(
            &released,
            PendingInputEvent::Reserve {
                token: token("a-2"),
            },
        )
        .next
        .unwrap_or_else(queued);
        assert_eq!(
            (again.phase, again.attempt_count),
            (PendingInputPhase::Sending, 2)
        );
        assert_eq!(
            phase(&reduce(
                &sending("a-2"),
                PendingInputEvent::Release {
                    token: token("a-1")
                }
            )),
            Some(PendingInputPhase::Sending)
        );
    }

    #[test]
    fn settles_a_handled_receipt_without_a_token_only_for_the_inputs_current_lifetime() {
        assert_eq!(
            phase(&reduce(
                &queued(),
                PendingInputEvent::HandledLifetime {
                    lifetime_id: "life-0".into()
                }
            )),
            Some(PendingInputPhase::Queued)
        );
    }

    #[test]
    fn ignores_evidence_about_another_attempt_than_the_one_that_failed() {
        for event in [
            PendingInputEvent::Witnessed {
                attempt_id: "a-other".into(),
            },
            PendingInputEvent::NonDeliveryProven {
                attempt_id: "a-other".into(),
            },
        ] {
            let next = reduce(&uncertain(), event).next.unwrap_or_else(queued);
            let failure = next.failure.unwrap_or_else(|| panic!("failure"));
            assert_eq!(failure.delivery, DeliveryCertainty::Uncertain);
            assert_eq!(
                failure.attempt.map(|a| a.attempt_id).as_deref(),
                Some("a-1")
            );
        }
    }

    #[test]
    fn requeues_an_attempt_the_journal_proves_unsent_on_reopen_and_keeps_an_unproven_one_uncertain_t28()
     {
        let proven = reduce(
            &sending("a-1"),
            PendingInputEvent::Rebind {
                handle: reopened(),
                attempts_unsent: true,
            },
        )
        .next
        .unwrap_or_else(queued);
        let unproven = reduce(
            &sending("a-1"),
            PendingInputEvent::Rebind {
                handle: reopened(),
                attempts_unsent: false,
            },
        )
        .next
        .unwrap_or_else(queued);
        assert_eq!(
            (proven.phase, proven.handle.clone(), proven.revision),
            (PendingInputPhase::Queued, reopened(), 1)
        );
        assert!(proven.attempt.is_none());
        assert_eq!(
            unproven.failure.map(|f| f.delivery),
            Some(DeliveryCertainty::Uncertain)
        );
    }

    #[test]
    fn rebinds_lifetime_handles_without_losing_text_or_edited_history_t29() {
        let edited = replay(&[submit(), edit("e1", 1, "edited")]);
        let next = reduce(
            &edited,
            PendingInputEvent::Rebind {
                handle: reopened(),
                attempts_unsent: false,
            },
        )
        .next
        .unwrap_or_else(queued);
        assert_eq!(
            (
                next.handle.clone(),
                next.revision,
                next.has_edits,
                next.text.as_str()
            ),
            (reopened(), 1, true, "edited")
        );
        assert_eq!(
            project_pending_input(&next).state,
            PendingInputState::Edited
        );
    }

    #[test]
    fn drops_child_input_only_on_an_explicit_cancellation_t31() {
        let decision = reduce(
            &queued(),
            PendingInputEvent::Cancel {
                cause: CancelCause::ChildCancel,
                consumer_fenced: false,
            },
        );
        assert_eq!(
            decision.next.as_ref().map(|n| (n.phase, n.removed_by)),
            Some((PendingInputPhase::Removed, Some(RemovalCause::ChildCancel)))
        );
        assert_eq!(decision.effects, vec![PendingInputEffect::Persist]);
    }

    #[test]
    fn ignores_replay_without_reopening_terminal_input_t32() {
        let delivered_input = delivered();
        for event in [
            PendingInputEvent::Consumed {
                token: token("a-1"),
            },
            PendingInputEvent::Witnessed {
                attempt_id: "a-1".into(),
            },
            PendingInputEvent::Teardown,
        ] {
            let decision = reduce(&delivered_input, event);
            assert_eq!(
                decision,
                PendingInputDecision {
                    next: Some(delivered_input.clone()),
                    result: None,
                    effects: Vec::new()
                }
            );
        }
    }

    #[test]
    fn replays_one_identified_withdrawal_without_another_restore_effect_t35() {
        let stopped = replay(&[
            submit(),
            PendingInputEvent::Cancel {
                cause: CancelCause::Stop,
                consumer_fenced: false,
            },
        ]);
        assert!(
            reduce(
                &stopped,
                PendingInputEvent::Cancel {
                    cause: CancelCause::Stop,
                    consumer_fenced: false
                }
            )
            .effects
            .is_empty()
        );
    }

    #[test]
    fn retains_a_skipped_native_prompt_without_claiming_consumption_t49() {
        let decision = reduce(
            &sending("a-1"),
            PendingInputEvent::Refused {
                token: token("a-1"),
                reason: "skipped before dispatch".into(),
            },
        );
        assert_eq!(
            decision.next.and_then(|n| n.failure).map(|f| f.delivery),
            Some(DeliveryCertainty::NotDelivered)
        );
    }

    #[test]
    fn does_not_infer_non_delivery_from_native_cancellation_t50() {
        let decision = reduce(
            &sending("a-1"),
            PendingInputEvent::Ambiguous {
                token: token("a-1"),
                reason: "native turn cancelled".into(),
            },
        );
        assert_eq!(
            decision.next.and_then(|n| n.failure).map(|f| f.delivery),
            Some(DeliveryCertainty::Uncertain)
        );
    }

    #[test]
    fn keeps_a_witnessed_native_input_delivered_after_turn_cancellation_t51() {
        let delivered_input = delivered();
        let decision = reduce(
            &delivered_input,
            PendingInputEvent::Ambiguous {
                token: token("a-1"),
                reason: "cancelled".into(),
            },
        );
        assert_eq!(decision.next.as_ref(), Some(&delivered_input));
    }

    #[test]
    fn settles_only_the_positively_completed_native_request_identity_t52() {
        assert_eq!(
            phase(&reduce(
                &sending("a-1"),
                PendingInputEvent::Consumed {
                    token: token("a-other")
                }
            )),
            Some(PendingInputPhase::Sending)
        );
        assert_eq!(
            phase(&reduce(
                &sending("a-1"),
                PendingInputEvent::Consumed {
                    token: token("a-1")
                }
            )),
            Some(PendingInputPhase::Delivered)
        );
    }

    #[test]
    fn keeps_the_handle_a_reopen_issued_for_later_changes() {
        let reopened_input = reduce(
            &queued(),
            PendingInputEvent::Rebind {
                handle: reopened(),
                attempts_unsent: false,
            },
        )
        .next
        .unwrap_or_else(queued);
        assert_eq!(
            outcome(&reduce(
                &reopened_input,
                edit_with("e1", 1, "x", reopened())
            )),
            Some(OperationOutcome::Edited)
        );
        assert_eq!(
            outcome(&reduce(&reopened_input, edit_with("e2", 1, "x", handle()))),
            Some(OperationOutcome::StaleHandle)
        );
    }
}
