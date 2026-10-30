//! Port of senpi `packages/coding-agent/src/core/pending-input-mutations.ts`.

use crate::pending_input_types::{
    DeliveryCertainty, MutationTarget, OperationOutcome, PendingInput, PendingInputDecision,
    PendingInputEffect, PendingInputEvent, PendingInputHandle, PendingInputOperationResult,
    PendingInputPhase, RecordedOperation, RemovalCause, SupersededVersion,
};

/// The three user mutations: edit, remove, retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationEvent {
    Edit {
        target: MutationTarget,
        text: String,
        attachments: Vec<String>,
    },
    Remove {
        target: MutationTarget,
    },
    Retry {
        effect_id: String,
        handle: PendingInputHandle,
    },
}

impl MutationEvent {
    pub fn from_event(event: &PendingInputEvent) -> Option<Self> {
        match event {
            PendingInputEvent::Edit {
                target,
                text,
                attachments,
            } => Some(Self::Edit {
                target: target.clone(),
                text: text.clone(),
                attachments: attachments.clone(),
            }),
            PendingInputEvent::Remove { target } => Some(Self::Remove {
                target: target.clone(),
            }),
            PendingInputEvent::Retry { effect_id, handle } => Some(Self::Retry {
                effect_id: effect_id.clone(),
                handle: handle.clone(),
            }),
            _ => None,
        }
    }

    fn effect_id(&self) -> &str {
        match self {
            Self::Edit { target, .. } | Self::Remove { target } => &target.effect_id,
            Self::Retry { effect_id, .. } => effect_id,
        }
    }

    fn handle(&self) -> &PendingInputHandle {
        match self {
            Self::Edit { target, .. } | Self::Remove { target } => &target.handle,
            Self::Retry { handle, .. } => handle,
        }
    }
}

pub fn reduce_mutation(
    current: Option<&PendingInput>,
    event: &MutationEvent,
) -> PendingInputDecision {
    let Some(current) = current else {
        return refuse(
            None,
            PendingInputOperationResult {
                outcome: OperationOutcome::NotFound,
                client_message_id: String::new(),
                revision: None,
                current_revision: None,
                text: None,
                lifetime_id: None,
                replayed: None,
            },
        );
    };
    let fingerprint = mutation_fingerprint(event);
    if let Some(recorded) = current.operations.get(event.effect_id()) {
        if recorded.fingerprint != fingerprint {
            return refuse(
                Some(current),
                answer(current, OperationOutcome::EffectIdConflict),
            );
        }
        return refuse(
            Some(current),
            PendingInputOperationResult {
                replayed: Some(true),
                ..recorded.result.clone()
            },
        );
    }
    let record = |next: &mut PendingInput, result: &PendingInputOperationResult| {
        next.operations = current.operations.clone();
        next.operations.insert(
            event.effect_id().to_owned(),
            RecordedOperation {
                fingerprint: fingerprint.clone(),
                result: result.clone(),
            },
        );
    };
    if let Some(refusal) = mutation_refusal(current, event) {
        let mut next = current.clone();
        record(&mut next, &refusal);
        return PendingInputDecision {
            next: Some(next),
            result: Some(refusal),
            effects: vec![PendingInputEffect::Persist],
        };
    }
    let (mut next, result) = apply_mutation(current, event);
    record(&mut next, &result);
    PendingInputDecision {
        next: Some(next),
        result: Some(result),
        effects: vec![PendingInputEffect::Persist],
    }
}

fn mutation_refusal(
    current: &PendingInput,
    event: &MutationEvent,
) -> Option<PendingInputOperationResult> {
    match current.phase {
        PendingInputPhase::Delivered => {
            return Some(answer(current, OperationOutcome::AlreadyDelivered));
        }
        PendingInputPhase::Removed => return Some(answer(current, OperationOutcome::NotFound)),
        _ => {}
    }
    if !same_handle(&current.handle, event.handle()) {
        return Some(answer(current, OperationOutcome::StaleHandle));
    }
    if current.phase == PendingInputPhase::Sending {
        let outcome = if matches!(event, MutationEvent::Retry { .. }) {
            OperationOutcome::NotRetryable
        } else {
            OperationOutcome::Sending
        };
        return Some(answer(current, outcome));
    }
    let delivery = current.failure.as_ref().map(|failure| failure.delivery);
    let (expected_revision, is_edit) = match event {
        MutationEvent::Retry { .. } => {
            let retryable = current.phase == PendingInputPhase::Failed
                && delivery == Some(DeliveryCertainty::NotDelivered);
            return if retryable {
                None
            } else {
                Some(answer(current, OperationOutcome::NotRetryable))
            };
        }
        MutationEvent::Edit { target, .. } => (target.expected_revision, true),
        MutationEvent::Remove { target } => (target.expected_revision, false),
    };
    if current.phase == PendingInputPhase::Failed {
        if delivery == Some(DeliveryCertainty::Uncertain) {
            return Some(answer(current, OperationOutcome::Unresolved));
        }
        if is_edit {
            return Some(answer(current, OperationOutcome::NotEditable));
        }
    }
    if expected_revision != current.revision {
        return Some(PendingInputOperationResult {
            current_revision: Some(current.revision),
            ..answer(current, OperationOutcome::Conflict)
        });
    }
    None
}

fn apply_mutation(
    current: &PendingInput,
    event: &MutationEvent,
) -> (PendingInput, PendingInputOperationResult) {
    match event {
        MutationEvent::Edit {
            text, attachments, ..
        } => {
            let revision = current.revision + 1;
            let mut next = current.clone();
            next.revision = revision;
            next.has_edits = true;
            next.text = text.clone();
            next.attachments = attachments.clone();
            next.superseded.push(SupersededVersion {
                lifetime_id: current.handle.lifetime_id.clone(),
                revision: current.revision,
                text: current.text.clone(),
            });
            let result = PendingInputOperationResult {
                revision: Some(revision),
                text: Some(text.clone()),
                ..answer(&next, OperationOutcome::Edited)
            };
            (next, result)
        }
        MutationEvent::Remove { .. } => {
            let mut next = current.clone();
            next.phase = PendingInputPhase::Removed;
            next.removed_by = Some(RemovalCause::Remove);
            next.failure = None;
            let result = answer(&next, OperationOutcome::Removed);
            (next, result)
        }
        MutationEvent::Retry { .. } => {
            let mut next = current.clone();
            next.phase = PendingInputPhase::Queued;
            next.failure = None;
            let result = PendingInputOperationResult {
                revision: Some(next.revision),
                ..answer(&next, OperationOutcome::Requeued)
            };
            (next, result)
        }
    }
}

fn mutation_fingerprint(event: &MutationEvent) -> String {
    let target = format!(
        "{}/{}",
        event.handle().lifetime_id,
        event.handle().queued_id
    );
    let value = match event {
        MutationEvent::Edit {
            target: t,
            text,
            attachments,
        } => {
            serde_json::json!(["edit", target, t.expected_revision, text, attachments])
        }
        MutationEvent::Remove { target: t } => {
            serde_json::json!(["remove", target, t.expected_revision])
        }
        MutationEvent::Retry { .. } => serde_json::json!(["retry", target]),
    };
    value.to_string()
}

fn same_handle(a: &PendingInputHandle, b: &PendingInputHandle) -> bool {
    a.lifetime_id == b.lifetime_id && a.queued_id == b.queued_id
}

pub fn answer(input: &PendingInput, outcome: OperationOutcome) -> PendingInputOperationResult {
    PendingInputOperationResult {
        outcome,
        client_message_id: input.client_message_id.clone(),
        revision: Some(input.revision),
        current_revision: None,
        text: None,
        lifetime_id: Some(input.handle.lifetime_id.clone()),
        replayed: None,
    }
}

fn refuse(
    current: Option<&PendingInput>,
    result: PendingInputOperationResult,
) -> PendingInputDecision {
    PendingInputDecision {
        next: current.cloned(),
        result: Some(result),
        effects: Vec::new(),
    }
}
