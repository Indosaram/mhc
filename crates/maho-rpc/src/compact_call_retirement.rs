//! senpi `modes/rpc/compact-call-retirement.ts`: iterative post-order retirement of a
//! settled call and its completed descendants into `CallProjection`.

use crate::compact_call_model::{
    CallJoin, CallOutcome, CallOutcomeSource, CallPhase, CallProjection, NestedCalls,
    OutstandingCall, TERMINAL_PHASE, terminal,
};
use crate::compact_call_table::CompactCallTable;
use crate::compact_read_lease::CompactError;

fn projection_of(call: OutstandingCall, nested_calls: NestedCalls) -> CallProjection {
    CallProjection {
        runtime_item_key: call.runtime_item_key,
        turn_id: call.turn_id,
        tool_call_id: call.tool_call_id,
        owning_message_id: call.owning_message_id,
        content_index: call.content_index,
        parent_key: call.parent_key,
        requested_name: call.requested_name,
        resolved_name: call.resolved_name,
        seed: call.seed,
        input: call.input,
        correlation: call.correlation,
        start_sequence: call.start_sequence,
        started_at_ms: call.started_at_ms,
        outcome: call.outcome,
        call_unknown: call.call_unknown,
        nested_calls,
    }
}

/// The carrier settled: the call and its completed descendants leave the table with one
/// projection carrying every completed child (RULING-60 Q3).
pub fn settle_call(
    table: &mut CompactCallTable,
    call: &OutstandingCall,
    is_error: bool,
) -> Result<CallProjection, CompactError> {
    let settled = if terminal(call) {
        call.clone()
    } else {
        OutstandingCall {
            phase: TERMINAL_PHASE,
            outcome: Some(CallOutcome::Result {
                is_error,
                source: CallOutcomeSource::ResultMessage,
            }),
            ..call.clone()
        }
    };
    retire(table, settled)
}

struct RetireFrame {
    call: OutstandingCall,
    completed: Vec<OutstandingCall>,
    every_child_completed: bool,
    calls: Vec<CallProjection>,
}

fn frame(table: &CompactCallTable, call: OutstandingCall) -> RetireFrame {
    let children = table.children_of(&call.runtime_item_key);
    let total = children.len();
    let completed: Vec<OutstandingCall> = children.into_iter().filter(terminal).collect();
    let every_child_completed = completed.len() == total;
    RetireFrame {
        call,
        completed,
        every_child_completed,
        calls: Vec::new(),
    }
}

/// Post-order over the completed descendants with an explicit stack (RULING-59 (1), RULING-67
/// (2)): one frame per open level, so the stack is bounded by the live call table, never by the
/// call stack, and a chain of any depth settles.
fn retire(
    table: &mut CompactCallTable,
    call: OutstandingCall,
) -> Result<CallProjection, CompactError> {
    let mut stack = vec![frame(table, call)];
    loop {
        let next_child = {
            let top = stack
                .last()
                .ok_or_else(|| CompactError::Io("call retirement lost its root".into()))?;
            top.completed.get(top.calls.len()).cloned()
        };
        if let Some(child) = next_child {
            stack.push(frame(table, child));
            continue;
        }
        let top = stack
            .pop()
            .ok_or_else(|| CompactError::Io("call retirement lost its root".into()))?;
        table.remove(&top.call.runtime_item_key)?;
        let complete = top.every_child_completed
            && top
                .calls
                .iter()
                .all(|projection| projection.nested_calls.complete);
        let projection = projection_of(
            top.call,
            NestedCalls {
                calls: top.calls,
                complete,
                omitted: 0,
            },
        );
        if let Some(parent) = stack.last_mut() {
            parent.calls.push(projection);
        } else {
            return Ok(projection);
        }
    }
}

/// A child whose parent left the table, or can no longer get a carrier, has no projection to ride
/// on.
pub fn orphaned(table: &CompactCallTable, call: &OutstandingCall) -> bool {
    let Some(parent_key) = &call.parent_key else {
        return false;
    };
    table
        .get(parent_key)
        .is_none_or(|parent| parent.join == CallJoin::Carrierless)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSweepPatch {
    pub phase: Option<CallPhase>,
    pub outcome: Option<CallOutcome>,
    pub join: Option<CallJoin>,
}

impl CallSweepPatch {
    pub fn apply(self, call: &mut OutstandingCall) {
        if let Some(phase) = self.phase {
            call.phase = phase;
        }
        if let Some(outcome) = self.outcome {
            call.outcome = Some(outcome);
        }
        if let Some(join) = self.join {
            call.join = join;
        }
    }
}

pub fn not_executed_patch() -> CallSweepPatch {
    CallSweepPatch {
        phase: Some(TERMINAL_PHASE),
        outcome: Some(CallOutcome::NotExecuted),
        join: Some(CallJoin::Carrierless),
    }
}

/// RULING-13 Q3/Q4: the sweep patch for one call, nested or not, once no carrier can arrive;
/// `None` leaves it alone.
pub fn settled_patch(call: &OutstandingCall) -> Option<CallSweepPatch> {
    if call.start_sequence.is_none() && !terminal(call) {
        return Some(not_executed_patch());
    }
    if call.phase == CallPhase::Executing {
        return Some(CallSweepPatch {
            phase: Some(TERMINAL_PHASE),
            outcome: Some(CallOutcome::Abandoned),
            join: Some(CallJoin::Carrierless),
        });
    }
    if call.join == CallJoin::Pending {
        return Some(CallSweepPatch {
            phase: None,
            outcome: None,
            join: Some(CallJoin::Carrierless),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_call_model::fresh_call;
    use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions};

    #[tokio::test]
    async fn retires_deep_completed_chain_and_marks_incomplete_when_child_still_executing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut table = CompactCallTable::new(store.clone());

        let mut root = fresh_call("root".into(), "t1".into(), TERMINAL_PHASE);
        root.outcome = Some(CallOutcome::Result {
            is_error: false,
            source: CallOutcomeSource::ExecutionEnd,
        });
        table.put(root.clone(), 1, 0).expect("put root");

        let mut done_child = fresh_call("c1".into(), "t1".into(), TERMINAL_PHASE);
        done_child.parent_key = Some("root".into());
        table.put(done_child, 2, 0).expect("put c1");

        let mut live_child = fresh_call("c2".into(), "t1".into(), CallPhase::Executing);
        live_child.parent_key = Some("root".into());
        table.put(live_child, 3, 0).expect("put c2");

        let proj = settle_call(&mut table, &root, false).expect("settle");
        assert_eq!(proj.nested_calls.calls.len(), 1);
        assert_eq!(proj.nested_calls.calls[0].runtime_item_key, "c1");
        assert!(!proj.nested_calls.complete);
        assert_eq!(proj.nested_calls.omitted, 0);
        assert!(table.get("root").is_none());
        assert!(table.get("c1").is_none());
        let c2 = table.get("c2").expect("c2 stays");
        assert!(orphaned(&table, c2));

        table.remove("c2").expect("rm c2");
        store.retire().await.expect("retire");
    }
}
