//! Port of senpi `packages/agent/src/execution/ledger-transition.ts`.

use super::ledger_types::{
    ExecutionCommand, ExecutionEntry, ExecutionEvent, ExecutionOutcome, ExecutionProgress,
    InterruptedReason, LedgerRefusal, LedgerReply, MAX_EXECUTION_LIFETIME_MS,
    RECONCILIATION_GRACE_MS, applied, refused, replayed,
};

fn advance(mut entry: ExecutionEntry, progress: ExecutionProgress) -> LedgerReply {
    entry.progress = progress;
    applied(entry)
}

fn settle(entry: ExecutionEntry, reason: InterruptedReason) -> LedgerReply {
    let hooks_ran = entry.effect_possible;
    let uncertain = entry.effect_possible || entry.body_dispatched;
    advance(
        entry,
        ExecutionProgress::Settled {
            outcome: ExecutionOutcome::from_reason(reason, hooks_ran, uncertain),
        },
    )
}

fn authorized(entry: &ExecutionEntry, command: &ExecutionCommand) -> bool {
    let actor = &command.actor;
    match &command.event {
        ExecutionEvent::Cancel => {
            actor.id == entry.issuer
                || actor.approved_workspace.as_deref() == Some(entry.workspace.as_str())
        }
        ExecutionEvent::Consent
        | ExecutionEvent::Release
        | ExecutionEvent::Abort
        | ExecutionEvent::Ack
        | ExecutionEvent::Renew { .. } => actor.id == entry.issuer,
        ExecutionEvent::Prepare
        | ExecutionEvent::Effect
        | ExecutionEvent::Freeze { .. }
        | ExecutionEvent::Block { .. }
        | ExecutionEvent::Dispatch
        | ExecutionEvent::Result { .. }
        | ExecutionEvent::Expire
        | ExecutionEvent::Restart => actor.id == entry.executor,
    }
}

fn recover(entry: ExecutionEntry, now: i64) -> LedgerReply {
    match &entry.progress {
        ExecutionProgress::Admitted | ExecutionProgress::Preparing => {
            settle(entry, InterruptedReason::HandRestarted)
        }
        ExecutionProgress::Dispatched { call } => {
            if entry.reconcilable {
                let until = entry.lease_until.min(now + RECONCILIATION_GRACE_MS);
                let call = call.clone();
                advance(entry, ExecutionProgress::Reconciling { call, until })
            } else {
                settle(entry, InterruptedReason::HandRestarted)
            }
        }
        ExecutionProgress::Reconciling { until, .. } => {
            if now >= *until {
                settle(entry, InterruptedReason::HandRestarted)
            } else {
                replayed(entry)
            }
        }
        ExecutionProgress::Prepared { .. }
        | ExecutionProgress::PreparedAwaitingConsent { .. }
        | ExecutionProgress::Released { .. }
        | ExecutionProgress::Settled { .. }
        | ExecutionProgress::Acked => replayed(entry),
    }
}

/// A body may run only for an applied reply in state `dispatched`, never for an expiry settlement or replay.
pub fn transition_execution(entry: ExecutionEntry, command: &ExecutionCommand) -> LedgerReply {
    let now = command.now;
    if command.generation != entry.generation {
        return refused(LedgerRefusal::StaleGeneration);
    }
    if !authorized(&entry, command) {
        return refused(LedgerRefusal::Unauthorized);
    }
    if now < entry.admitted_at {
        return refused(LedgerRefusal::InvalidTime);
    }
    match &entry.progress {
        ExecutionProgress::Acked => {
            if matches!(command.event, ExecutionEvent::Ack) {
                return LedgerReply::Replayed { entry };
            }
            return LedgerReply::ResultExpired;
        }
        ExecutionProgress::Settled { .. } => match &command.event {
            ExecutionEvent::Ack => return advance(entry, ExecutionProgress::Acked),
            ExecutionEvent::Release | ExecutionEvent::Abort | ExecutionEvent::Consent => {
                return refused(LedgerRefusal::Terminal);
            }
            _ => return replayed(entry),
        },
        _ => {}
    }

    if now >= entry.lease_until {
        return settle(entry, InterruptedReason::LeaseExpired);
    }
    if let ExecutionProgress::Reconciling { until, .. } = &entry.progress
        && now >= *until
    {
        return settle(entry, InterruptedReason::HandRestarted);
    }

    match &command.event {
        ExecutionEvent::Prepare => {
            if matches!(entry.progress, ExecutionProgress::Admitted) {
                advance(entry, ExecutionProgress::Preparing)
            } else {
                replayed(entry)
            }
        }
        ExecutionEvent::Effect => {
            if entry.effect_possible {
                return replayed(entry);
            }
            if matches!(entry.progress, ExecutionProgress::Preparing) {
                let mut next = entry;
                next.effect_possible = true;
                applied(next)
            } else {
                refused(LedgerRefusal::InvalidTransition)
            }
        }
        ExecutionEvent::Freeze {
            call,
            awaiting_consent,
        } => {
            if matches!(entry.progress, ExecutionProgress::Preparing) && entry.effect_possible {
                let next_progress = if *awaiting_consent {
                    ExecutionProgress::PreparedAwaitingConsent { call: call.clone() }
                } else {
                    ExecutionProgress::Prepared { call: call.clone() }
                };
                return advance(entry, next_progress);
            }
            if entry
                .progress
                .call()
                .is_some_and(|existing| existing.digest == call.digest)
            {
                replayed(entry)
            } else {
                refused(LedgerRefusal::InvalidTransition)
            }
        }
        ExecutionEvent::Block { value } => {
            if matches!(entry.progress, ExecutionProgress::Preparing) {
                let hooks_ran = entry.effect_possible;
                advance(
                    entry,
                    ExecutionProgress::Settled {
                        outcome: ExecutionOutcome::Blocked {
                            value: value.clone(),
                            hooks_ran,
                            uncertain: false,
                        },
                    },
                )
            } else {
                refused(LedgerRefusal::InvalidTransition)
            }
        }
        ExecutionEvent::Consent => {
            if let ExecutionProgress::PreparedAwaitingConsent { call } = &entry.progress {
                let call = call.clone();
                advance(entry, ExecutionProgress::Prepared { call })
            } else {
                refused(LedgerRefusal::InvalidTransition)
            }
        }
        ExecutionEvent::Release => {
            if let ExecutionProgress::Prepared { call } = &entry.progress {
                let call = call.clone();
                return advance(entry, ExecutionProgress::Released { call });
            }
            if matches!(entry.progress, ExecutionProgress::Released { .. }) || entry.body_dispatched
            {
                replayed(entry)
            } else {
                refused(LedgerRefusal::InvalidTransition)
            }
        }
        ExecutionEvent::Dispatch => {
            if entry.body_dispatched {
                return replayed(entry);
            }
            if let ExecutionProgress::Released { call } = &entry.progress {
                let call = call.clone();
                let mut next = entry;
                next.body_dispatched = true;
                next.progress = ExecutionProgress::Dispatched { call };
                applied(next)
            } else {
                refused(LedgerRefusal::InvalidTransition)
            }
        }
        ExecutionEvent::Result { value } => {
            if !matches!(
                entry.progress,
                ExecutionProgress::Dispatched { .. } | ExecutionProgress::Reconciling { .. }
            ) {
                return refused(LedgerRefusal::InvalidTransition);
            }
            let hooks_ran = entry.effect_possible;
            advance(
                entry,
                ExecutionProgress::Settled {
                    outcome: ExecutionOutcome::Result {
                        value: value.clone(),
                        hooks_ran,
                        uncertain: false,
                    },
                },
            )
        }
        ExecutionEvent::Cancel => settle(entry, InterruptedReason::Cancelled),
        ExecutionEvent::Abort => {
            if entry.body_dispatched {
                refused(LedgerRefusal::InvalidTransition)
            } else {
                settle(entry, InterruptedReason::Cancelled)
            }
        }
        ExecutionEvent::Ack => refused(LedgerRefusal::InvalidTransition),
        ExecutionEvent::Renew { seq, lease_until } => {
            if *seq <= entry.renew_seq {
                return replayed(entry);
            }
            if *lease_until <= entry.lease_until
                || *lease_until > entry.admitted_at + MAX_EXECUTION_LIFETIME_MS
            {
                return refused(LedgerRefusal::InvalidRenewal);
            }
            let mut next = entry;
            next.renew_seq = *seq;
            next.lease_until = *lease_until;
            applied(next)
        }
        ExecutionEvent::Expire => replayed(entry),
        ExecutionEvent::Restart => recover(entry, now),
    }
}
