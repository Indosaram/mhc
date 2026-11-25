//! Port of senpi `packages/agent/src/execution/coordinator-consent.ts`.

use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde_json::json;

use super::coordinator_types::{
    BatchConsent, BatchExecutor, ConsentAnswer, ConsentGate, ConsentRecord, ConsentRequest,
    ConsentStepReply, ExecutionRef, ManifestCall, PrepareReply,
};
use super::ledger_types::ExecutionOutcome;

pub type RecordConsentFn<'a> =
    Box<dyn FnOnce(ConsentRecord) -> BoxFuture<'a, Result<(), String>> + Send + 'a>;

pub struct ConsentStep<'a> {
    pub consent: &'a dyn BatchConsent,
    pub executor: Arc<dyn BatchExecutor>,
    pub exec_ref: ExecutionRef,
    pub call: &'a ManifestCall,
    pub lease_until: i64,
    pub recorded: Option<ConsentRecord>,
    pub record: RecordConsentFn<'a>,
}

fn denied_reply(gate: ConsentGate, answer: ConsentAnswer) -> PrepareReply {
    PrepareReply::Blocked {
        result: json!({
            "reason": "consent_denied",
            "gate": gate,
            "answer": answer,
        }),
    }
}

fn lost_outcome() -> ExecutionOutcome {
    ExecutionOutcome::Uncertain {
        hooks_ran: true,
        uncertain: true,
    }
}

fn expired_outcome() -> ExecutionOutcome {
    ExecutionOutcome::LeaseExpired {
        hooks_ran: true,
        uncertain: true,
    }
}

async fn answer(
    step: ConsentStep<'_>,
    gate: ConsentGate,
    digest: &str,
) -> Result<ConsentAnswer, String> {
    if let Some(rec) = &step.recorded
        && rec.gate == gate
        && rec.digest == digest
    {
        return Ok(rec.answer);
    }
    let given = step
        .consent
        .approve(ConsentRequest {
            issuer: step.exec_ref.issuer,
            execution_id: step.exec_ref.execution_id,
            generation: step.exec_ref.generation,
            gate,
            digest: digest.to_owned(),
            name: step.call.name.clone(),
            lease_until: step.lease_until,
        })
        .await?;
    (step.record)(ConsentRecord {
        gate,
        digest: digest.to_owned(),
        answer: given,
    })
    .await?;
    Ok(given)
}

pub enum RawConsentResult {
    Allowed { authorized_digest: Option<String> },
    Blocked(PrepareReply),
}

pub async fn gate_raw_call(step: ConsentStep<'_>) -> Result<RawConsentResult, String> {
    if !step.consent.needs_consent(&step.call.name, &step.call.args) {
        return Ok(RawConsentResult::Allowed {
            authorized_digest: None,
        });
    }
    let digest = step.call.request_digest.clone();
    let given = answer(step, ConsentGate::Raw, &digest).await?;
    if given == ConsentAnswer::Approved {
        Ok(RawConsentResult::Allowed {
            authorized_digest: Some(digest),
        })
    } else {
        Ok(RawConsentResult::Blocked(denied_reply(
            ConsentGate::Raw,
            given,
        )))
    }
}

pub async fn resolve_frozen_consent(
    step: ConsentStep<'_>,
    frozen_digest: &str,
) -> Result<PrepareReply, String> {
    let executor = Arc::clone(&step.executor);
    let exec_ref = step.exec_ref.clone();
    let exec_id = exec_ref.execution_id.clone();
    let given = answer(step, ConsentGate::Frozen, frozen_digest).await?;
    if given == ConsentAnswer::Denied {
        let _ = executor.abort(exec_ref).await?;
        return Ok(denied_reply(ConsentGate::Frozen, given));
    }
    if given == ConsentAnswer::Expired {
        return Ok(PrepareReply::Settled {
            outcome: expired_outcome(),
        });
    }
    let Some(fut) = executor.consent(exec_ref, frozen_digest) else {
        return Err(format!(
            "executor answered prepared_awaiting_consent for {exec_id} but cannot take consent"
        ));
    };
    let reply = fut.await?;
    match reply {
        ConsentStepReply::Prepared => Ok(PrepareReply::Prepared {
            frozen_digest: frozen_digest.to_owned(),
        }),
        ConsentStepReply::UnknownExecution => Ok(PrepareReply::Settled {
            outcome: lost_outcome(),
        }),
        ConsentStepReply::Settled { outcome } => Ok(PrepareReply::Settled { outcome }),
    }
}
