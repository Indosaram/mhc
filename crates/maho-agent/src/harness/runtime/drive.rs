//! Port of senpi `packages/agent/src/harness/runtime/drive.ts`.

#[path = "drive/mod.rs"]
mod primitives;
pub use primitives::*;

use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::{Control, Operation, OperationState};
use super::types::{Drive, ProcedureResult, RuntimeDriveLane};
use crate::harness::agent_harness::DriveOutcome;

fn current_operation(lane: &dyn RuntimeDriveLane, drive: &Drive) -> Result<Operation, SessionError> {
    lane.state().operation.filter(|op| op.meta.operation_id == drive.operation_id).ok_or_else(|| session_invariant_error(format!("Drive {} has no matching current operation", drive.operation_id)))
}

pub async fn drive_operation(lane: &dyn RuntimeDriveLane, drive: &Drive) -> Result<DriveOutcome, SessionError> {
    let operation = current_operation(lane, drive)?;
    if operation.state.operation_scope_of().control == Control::Running {
        let mut invocation = crate::harness::hooks::HookInvocation::new(lane.name(), &drive.operation_id);
        invocation.operation = Some(operation.meta.intent.kind());
        match lane.hooks().run_with_gate(crate::harness::hooks::HookName::BeforeDrive, invocation, &drive.gate, &drive.context).await {
            Ok(_) => {},
            Err(crate::harness::hooks::HookRunError::Gate(crate::harness::execution::effect_gate::GateRefusal::Abort(abort))) => abort.cancellation.wait().await,
            Err(error) => return Err(session_invariant_error(error.to_string())),
        }
    }
    loop {
        let state = current_operation(lane, drive)?.state;
        let result = if matches!(state.operation_scope_of().control, Control::CancelRequested { .. }) { reconcile::reconcile_operation(lane, drive).await }
        else { match &state {
            OperationState::Starting(_) => checkpoint::start_run(lane, drive, &state).await,
            OperationState::Checkpoint(_) => checkpoint::run_checkpoint(lane, drive, &state).await,
            OperationState::AssistantReady(_) | OperationState::AssistantRetryWait(_) => generation::run_generation(lane, drive, &state).await,
            OperationState::AssistantEffectPending(_) => recovery::recover_assistant_generation(lane, drive, &state).await,
            OperationState::Tools(_) => lane.run_tools(drive, state.clone()).await,
            OperationState::DeferredSuspended(_) | OperationState::DeferredEffectPending(_) => deferred::run_deferred(lane, drive, &state).await,
            OperationState::SummaryDeciding(_) | OperationState::SummaryReady(_) | OperationState::SummaryEffectPending(_) | OperationState::SummaryRetryWait(_) | OperationState::NavigationReadyToCommit(_) => lane.run_structural(drive, state.clone()).await,
        }};
        let result = match result {
            Ok(result) => result,
            Err(error) => match drive.gate.admit(|| ()) {
                Err(crate::harness::execution::effect_gate::GateRefusal::Abort(abort)) => { abort.cancellation.wait().await; ProcedureResult::Continue },
                _ => return Err(error),
            },
        };
        match result {
            ProcedureResult::Settled { outcome } => return Ok(DriveOutcome::Settled { outcome }),
            ProcedureResult::Waiting { outcome } => return Ok(outcome),
            ProcedureResult::Continue => {
                let next = current_operation(lane, drive)?.state;
                if next == state && !matches!(next.operation_scope_of().control, Control::CancelRequested { .. }) {
                    let at = serde_json::to_value(state.at()).map_err(|e|session_invariant_error(e.to_string()))?;
                    return Err(session_invariant_error(format!("Drive procedure made no progress from {}", at.as_str().unwrap_or_default())));
                }
            }
        }
    }
}
