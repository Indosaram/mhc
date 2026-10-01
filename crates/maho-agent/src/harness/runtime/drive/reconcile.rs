//! Port of senpi `packages/agent/src/harness/runtime/drive/reconcile.ts`.

use std::sync::Arc;
use crate::harness::events::{HarnessEvent, HarnessEventPayload, RunEndPayload, NavigationEndPayload};
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::*;
use crate::harness::runtime::types::*;
use super::terminal::{operation_cleanup_writes, operation_result_record};

pub async fn reconcile_operation(lane: &dyn RuntimeDriveLane, drive: &Drive) -> Result<ProcedureResult, SessionError> {
    let operation = lane.state().operation.ok_or_else(|| session_invariant_error(format!("Drive {} has no matching operation to reconcile", drive.operation_id)))?;
    if operation.meta.operation_id != drive.operation_id { return Err(session_invariant_error(format!("Drive {} has no matching operation to reconcile", drive.operation_id))); }
    if !matches!(operation.state.operation_scope_of().control, Control::CancelRequested { .. }) { return Err(session_invariant_error(format!("Operation {} is not cancelled", drive.operation_id))); }
    let cancellation = crate::harness::execution::effect_gate::Cancellation::new(); cancellation.resolve();
    drive.begin_abort(cancellation); drive.signal_abort();
    let deferred = match &operation.state { OperationState::DeferredSuspended(s) => Some(&s.deferred), OperationState::DeferredEffectPending(s) => Some(&s.deferred), _ => None };
    if let Some(deferred) = deferred {
        let reader = lane.session().begin_mutation(&drive.context).await?;
        let handle = super::deferred::read_deferred_source_handle(reader.as_ref(), deferred, &drive.context).await;
        reader.end(&drive.context).await;
        let handle = handle?;
        if let Some(model) = lane.models().get_model(&deferred.configuration.model.provider, &deferred.configuration.model.model_id) {
            let options = maho_ai::types::ProviderRequestOptions { signal: Some(drive.close_signal.clone()), timeout_ms: deferred.stream_options.timeout_ms, max_retries: deferred.stream_options.max_retries, max_retry_delay_ms: deferred.stream_options.max_retry_delay_ms, headers: deferred.stream_options.headers.as_ref().map(|headers| headers.iter().map(|(key,value)| (key.clone(), value.as_str().map(str::to_owned))).collect()), ..Default::default() };
            match lane.cancel_deferred(&model, &handle, options).await { Ok(()) | Err(_) => {} }
        }
    }
    match &operation.state {
        OperationState::AssistantEffectPending(_) | OperationState::DeferredEffectPending(_) => return super::recovery::recover_cancelled_assistant_effect(lane, drive, &operation.state).await,
        OperationState::Tools(_) => return lane.run_tools(drive, operation.state).await,
        _ => {}
    }
    let result = settle_operation(lane, &operation.state, &drive.context, false, |state, op, reader| async move {
        let record = operation_result_record(&op.meta, TerminalStatus::Aborted, state.tip_id.clone(), None)?;
        let writes = operation_cleanup_writes(reader.as_ref(), &drive.operation_id, &op.state, &drive.context).await?;
        let payload = match op.meta.intent.kind() {
            OperationIntentKind::Run => HarnessEventPayload::RunEnd(RunEndPayload { run_id: drive.operation_id.clone(), from_tip_id: op.meta.source_tip_id, tip_id: state.tip_id, ended_at: record.ended_at, status: "aborted".into(), error: None }),
            OperationIntentKind::Compaction => HarnessEventPayload::CompactionEnd { run_id: drive.operation_id.clone(), reason: "manual".into(), ended_at: record.ended_at, status: "aborted".into(), entry_id: None },
            OperationIntentKind::Navigation => HarnessEventPayload::NavigationEnd(NavigationEndPayload { run_id: drive.operation_id.clone(), from_tip_id: op.meta.source_tip_id, tip_id: state.tip_id, ended_at: record.ended_at, status: "aborted".into(), error: None }),
        };
        let mut events = Vec::new();
        if matches!(op.meta.intent, OperationIntent::Run { .. }) {
            let task = match &op.state { OperationState::SummaryDeciding(s) => Some(&s.task), OperationState::SummaryReady(s) => Some(&s.ready.scope.task), OperationState::SummaryEffectPending(s) => Some(&s.pending.scope.task), OperationState::SummaryRetryWait(s) => Some(&s.retry.scope.task), _ => None };
            if let Some(task) = task {
                if !matches!(task.boundary, ResultBoundary::ResumeCheckpoint { .. }) || task.reason.is_none() { return Err(session_invariant_error("Cancelled run summary has an invalid result boundary")); }
                let reason = match task.reason { Some(SummaryTaskReason::Threshold) => "threshold", Some(SummaryTaskReason::Overflow) => "overflow", Some(SummaryTaskReason::Manual) => "manual", None => return Err(session_invariant_error("Cancelled run summary has an invalid result boundary")) };
                events.push(HarnessEvent::new(HarnessEventPayload::CompactionEnd { run_id: drive.operation_id.clone(), reason: reason.into(), ended_at: record.ended_at, status: "aborted".into(), entry_id: None }, Some(lane.name().into())));
            }
        }
        events.push(HarnessEvent::new(payload, Some(lane.name().into()))); let outcome = record.clone();
        Ok(OperationCommand::Finish { decision: Box::new(FinishDecision { writes, record, lane: None, materialize: Arc::new(move |_| ProcedureResult::Settled { outcome: outcome.clone() }), events: Some(Arc::new(move |_| events.clone())) }) })
    }).await?;
    Ok(match result { ContinueOperationResult::Result { value } => value, ContinueOperationResult::CancelRequested => ProcedureResult::Continue })
}
