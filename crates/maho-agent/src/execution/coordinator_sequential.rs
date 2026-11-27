//! Port of senpi `packages/agent/src/execution/coordinator-sequential.ts`.

use std::sync::Arc;

use maho_ai::types::BoxFuture;

use super::coordinator::BatchCoordinator;
use super::coordinator_types::{BatchDecision, BatchPlan, BatchResult, CallOutcome, NotRunCause};

pub trait SequentialCoordinator: Send + Sync {
    fn run(&self, plan: BatchPlan) -> BoxFuture<'_, Result<BatchResult, String>>;
}

fn sequential_batch_id(batch_id: &str, index: usize) -> String {
    format!("{batch_id}#{index}")
}

fn inherited_cause(aborting: Option<&CallOutcome>) -> NotRunCause {
    match aborting {
        None => NotRunCause::BatchAbort,
        Some(CallOutcome::NotRun { cause }) => *cause,
        Some(_) => NotRunCause::InterruptedPreparation,
    }
}

struct DefaultSequentialCoordinator {
    coordinator: Arc<dyn BatchCoordinator>,
}

impl SequentialCoordinator for DefaultSequentialCoordinator {
    fn run(&self, plan: BatchPlan) -> BoxFuture<'_, Result<BatchResult, String>> {
        Box::pin(async move {
            let mut outcomes: Vec<CallOutcome> = Vec::with_capacity(plan.calls.len());
            for (index, call) in plan.calls.iter().enumerate() {
                let single_plan = BatchPlan {
                    batch_id: sequential_batch_id(&plan.batch_id, index),
                    scope: plan.scope.clone(),
                    workspace: plan.workspace.clone(),
                    generation: plan.generation,
                    admitted_at: plan.admitted_at,
                    lease_until: plan.lease_until,
                    calls: vec![call.clone()],
                };
                let single = self.coordinator.run(single_plan).await?;
                let decision = single.decision;
                outcomes.extend(single.outcomes);
                if decision == BatchDecision::Abort {
                    let cause = inherited_cause(outcomes.last());
                    let remaining = plan.calls.len() - index - 1;
                    for _ in 0..remaining {
                        outcomes.push(CallOutcome::NotRun { cause });
                    }
                    return Ok(BatchResult {
                        decision: BatchDecision::Abort,
                        outcomes,
                    });
                }
            }
            Ok(BatchResult {
                decision: BatchDecision::Release,
                outcomes,
            })
        })
    }
}

pub fn create_sequential_coordinator(
    coordinator: Arc<dyn BatchCoordinator>,
) -> Arc<dyn SequentialCoordinator> {
    Arc::new(DefaultSequentialCoordinator { coordinator })
}
