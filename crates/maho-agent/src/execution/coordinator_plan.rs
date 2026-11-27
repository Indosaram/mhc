//! Port of senpi `packages/agent/src/execution/coordinator-plan.ts`.

use std::collections::HashMap;

use maho_ai::utils::tool_pair_repair::TOOL_RESULT_PLACEHOLDER;
use serde_json::json;

use super::canonical_digest::canonical_digest;
use super::coordinator_types::{
    BatchManifest, BatchPlan, CallOutcome, ManifestCall, NotRunCause, OutcomeInterruptionReason,
    PrepareReply, SettleReply,
};
use super::ledger_types::ExecutionOutcome;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallDependency {
    pub depends_on: Vec<usize>,
    pub wave: usize,
}

/// Release dependencies exactly as the agent loop has always ordered a batch.
pub fn batch_dependencies(sequential_flags: &[bool]) -> Vec<CallDependency> {
    let mut result: Vec<CallDependency> = Vec::with_capacity(sequential_flags.len());
    let mut previous_sequential: Option<usize> = None;
    let mut parallel_since: Vec<usize> = Vec::new();
    for (index, &is_seq) in sequential_flags.iter().enumerate() {
        let mut depends_on = match previous_sequential {
            Some(prev) => vec![prev],
            None => Vec::new(),
        };
        if is_seq {
            depends_on.extend(parallel_since.iter().copied());
        }
        let max_wave = depends_on
            .iter()
            .filter_map(|&dep| result.get(dep).map(|d| d.wave as isize))
            .max()
            .unwrap_or(-1);
        let wave = (max_wave + 1) as usize;
        result.push(CallDependency { depends_on, wave });
        if is_seq {
            previous_sequential = Some(index);
            parallel_since.clear();
        } else {
            parallel_since.push(index);
        }
    }
    result
}

pub fn build_batch_manifest(issuer: &str, plan: &BatchPlan) -> BatchManifest {
    let seq_flags: Vec<bool> = plan.calls.iter().map(|c| c.sequential).collect();
    let dependencies = batch_dependencies(&seq_flags);
    let mut calls = Vec::with_capacity(plan.calls.len());
    for (source_index, call) in plan.calls.iter().enumerate() {
        let placement = dependencies.get(source_index);
        let depends_on = placement.map_or_else(Vec::new, |p| p.depends_on.clone());
        let wave = placement.map_or(0, |p| p.wave);
        let req_val = json!({ "name": call.name, "args": call.args });
        let request_digest = canonical_digest(&req_val);
        calls.push(ManifestCall {
            execution_id: call.execution_id.clone(),
            executor: call.executor.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
            sequential: call.sequential,
            source_index,
            depends_on,
            wave,
            request_digest,
        });
    }
    let manifest_input = json!({
        "batchId": plan.batch_id,
        "scope": plan.scope,
        "workspace": plan.workspace,
        "generation": plan.generation,
        "admittedAt": plan.admitted_at,
        "leaseUntil": plan.lease_until,
        "issuer": issuer,
        "calls": calls,
    });
    let manifest_digest = canonical_digest(&manifest_input);
    BatchManifest {
        batch_id: plan.batch_id.clone(),
        scope: plan.scope.clone(),
        workspace: plan.workspace.clone(),
        generation: plan.generation,
        admitted_at: plan.admitted_at,
        lease_until: plan.lease_until,
        issuer: issuer.to_owned(),
        manifest_digest,
        calls,
    }
}

pub fn abort_cause(reply: &PrepareReply) -> Option<NotRunCause> {
    match reply {
        PrepareReply::BatchAbort => Some(NotRunCause::BatchAbort),
        PrepareReply::Settled { outcome }
            if !matches!(outcome, ExecutionOutcome::Result { .. }) =>
        {
            Some(NotRunCause::InterruptedPreparation)
        }
        _ => None,
    }
}

pub fn finished(calls: &[ManifestCall], outcomes: &[Option<CallOutcome>], index: usize) -> bool {
    fn visit(
        calls: &[ManifestCall],
        outcomes: &[Option<CallOutcome>],
        at: usize,
        done: &mut HashMap<usize, bool>,
    ) -> bool {
        if let Some(&known) = done.get(&at) {
            return known;
        }
        let has_outcome = outcomes.get(at).is_some_and(Option::is_some);
        let deps_done = calls
            .get(at)
            .map(|c| c.depends_on.clone())
            .unwrap_or_default()
            .into_iter()
            .all(|dep| visit(calls, outcomes, dep, done));
        let res = has_outcome && deps_done;
        done.insert(at, res);
        res
    }
    let mut done = HashMap::new();
    visit(calls, outcomes, index, &mut done)
}

pub fn settled_outcome(reply: SettleReply) -> CallOutcome {
    match reply {
        SettleReply::UnknownExecution => CallOutcome::Interrupted {
            reason: OutcomeInterruptionReason::UnknownExecution,
            uncertain: true,
        },
        SettleReply::Settled { outcome } => match outcome {
            ExecutionOutcome::Result { value, .. } => CallOutcome::Result { value },
            ExecutionOutcome::Blocked { value, .. } => CallOutcome::Blocked { result: value },
            ExecutionOutcome::Cancelled { uncertain, .. } => CallOutcome::Interrupted {
                reason: OutcomeInterruptionReason::Cancelled,
                uncertain,
            },
            ExecutionOutcome::LeaseExpired { uncertain, .. } => CallOutcome::Interrupted {
                reason: OutcomeInterruptionReason::LeaseExpired,
                uncertain,
            },
            ExecutionOutcome::HandRestarted { uncertain, .. } => CallOutcome::Interrupted {
                reason: OutcomeInterruptionReason::HandRestarted,
                uncertain,
            },
            ExecutionOutcome::Uncertain { uncertain, .. } => CallOutcome::Interrupted {
                reason: OutcomeInterruptionReason::Uncertain,
                uncertain,
            },
        },
    }
}

pub fn interrupted_replay_text(outcome: &CallOutcome) -> Option<&'static str> {
    match outcome {
        CallOutcome::Interrupted { .. } => Some(TOOL_RESULT_PLACEHOLDER),
        CallOutcome::NotRun {
            cause: NotRunCause::InterruptedPreparation,
        } => Some(TOOL_RESULT_PLACEHOLDER),
        _ => None,
    }
}
