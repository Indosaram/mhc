//! Port of senpi `packages/agent/src/execution/testing/consent-cases.ts`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use tokio::sync::{Notify, oneshot};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::StoreValue;
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::coordinator_types::{
    BatchConsent, BatchDecision, BatchResult, CallOutcome, ConsentAnswer, ConsentRequest,
    ConsentStepReply, ExecutionRef, NotRunCause, OutcomeInterruptionReason,
};
use crate::execution::ledger_types::ExecutionOutcome;

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::{BatchHarness, batch_harness, batch_plan, call_spec};

type ConsentCase = CaseImplementation<BatchCaseAdapters>;

/// A lease that has ended: the harness admits every call with a 60 s lease.
const PAST_LEASE_MS: i64 = 120_000;

pub struct ScriptedConsent {
    pub port: Arc<dyn BatchConsent>,
    requests: Arc<Mutex<Vec<ConsentRequest>>>,
    waiting: Arc<Mutex<VecDeque<oneshot::Sender<ConsentAnswer>>>>,
    asked_notify: Arc<Notify>,
    asked_count: Arc<Mutex<usize>>,
}

impl ScriptedConsent {
    pub async fn asked(&self) {
        loop {
            let notified = self.asked_notify.notified();
            if *self.asked_count.lock().unwrap_or_else(|p| p.into_inner()) > 0 {
                return;
            }
            notified.await;
        }
    }

    pub fn answer(&self, given: ConsentAnswer) -> Result<(), String> {
        let tx = self
            .waiting
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front()
            .ok_or_else(|| "no consent request is waiting".to_string())?;
        let _ = tx.send(given);
        Ok(())
    }

    pub fn requests(&self) -> Vec<ConsentRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

struct ScriptedConsentPort {
    needs_consent: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    requests: Arc<Mutex<Vec<ConsentRequest>>>,
    waiting: Arc<Mutex<VecDeque<oneshot::Sender<ConsentAnswer>>>>,
    asked_notify: Arc<Notify>,
    asked_count: Arc<Mutex<usize>>,
}

impl BatchConsent for ScriptedConsentPort {
    fn needs_consent(&self, name: &str, _args: &StoreValue) -> bool {
        (self.needs_consent)(name)
    }

    fn approve(&self, request: ConsentRequest) -> BoxFuture<'_, Result<ConsentAnswer, String>> {
        self.requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(request);
        let (tx, rx) = oneshot::channel();
        self.waiting
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push_back(tx);
        {
            let mut count = self.asked_count.lock().unwrap_or_else(|p| p.into_inner());
            *count += 1;
        }
        self.asked_notify.notify_waiters();
        Box::pin(async move { rx.await.map_err(|e| e.to_string()) })
    }
}

pub fn scripted_consent(needs: impl Fn(&str) -> bool + Send + Sync + 'static) -> ScriptedConsent {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let waiting = Arc::new(Mutex::new(VecDeque::new()));
    let asked_notify = Arc::new(Notify::new());
    let asked_count = Arc::new(Mutex::new(0usize));
    let port = Arc::new(ScriptedConsentPort {
        needs_consent: Arc::new(needs),
        requests: Arc::clone(&requests),
        waiting: Arc::clone(&waiting),
        asked_notify: Arc::clone(&asked_notify),
        asked_count: Arc::clone(&asked_count),
    });
    ScriptedConsent {
        port,
        requests,
        waiting,
        asked_notify,
        asked_count,
    }
}

async fn expired_while_awaiting(
    h: BatchHarness,
    answer: ConsentAnswer,
    late_approval: bool,
) -> ConformanceResult {
    let not_run = CallOutcome::NotRun {
        cause: NotRunCause::InterruptedPreparation,
    };
    let plan = batch_plan(
        if late_approval {
            "consent-02"
        } else {
            "consent-01"
        },
        &[
            call_spec("a", "e1"),
            ("g", "e1", "gated", false),
            call_spec("c", "e2"),
        ],
    );
    let approver = scripted_consent(|_| false);
    let brain = h
        .brain(None, Some("brain"), Some(Arc::clone(&approver.port)))
        .await?;
    let plan_c = plan.clone();
    let run = tokio::spawn(async move { brain.run(plan_c).await });
    approver.asked().await;
    h.advance(PAST_LEASE_MS);
    approver.answer(answer)?;
    let result = run.await.map_err(|e| e.to_string())??;
    let expired = CallOutcome::Interrupted {
        reason: OutcomeInterruptionReason::LeaseExpired,
        uncertain: true,
    };
    expect_equal(
        &result,
        &BatchResult {
            decision: BatchDecision::Abort,
            outcomes: vec![not_run.clone(), expired, not_run],
        },
        "lease_expired, the rest ABORTed",
    )?;
    let reqs = approver.requests();
    let req_pairs: Vec<(&str, &str)> = reqs
        .iter()
        .map(|r| {
            (
                r.execution_id.as_str(),
                match r.gate {
                    crate::execution::coordinator_types::ConsentGate::Raw => "raw",
                    crate::execution::coordinator_types::ConsentGate::Frozen => "frozen",
                },
            )
        })
        .collect();
    expect_equal(
        &req_pairs,
        &vec![("g", "frozen")],
        "one approval request, bound to the frozen call",
    )?;
    expect_equal(
        &(h.count("body:a") + h.count("body:g") + h.count("body:c")),
        &0usize,
        "no body ran",
    )?;
    expect_equal(
        &h.count("shim:g"),
        &1usize,
        "no automatic successor: g was never prepared again",
    )?;
    let frozen_digest = reqs.first().map(|r| r.digest.as_str()).unwrap_or("");
    let exec = h.executor_proxy("e1");
    let late = match exec.consent(
        ExecutionRef {
            issuer: "brain".to_string(),
            execution_id: "g".to_string(),
            generation: 1,
        },
        frozen_digest,
    ) {
        Some(fut) => Some(fut.await?),
        None => None,
    };
    let late_reason = late.as_ref().map(|reply| match reply {
        ConsentStepReply::Settled { outcome } => outcome.reason_str(),
        ConsentStepReply::Prepared => "prepared",
        ConsentStepReply::UnknownExecution => "unknown_execution",
    });
    expect_equal(
        &late_reason,
        &Some("lease_expired"),
        "a later approval is rejected with the endpoint's own lease_expired outcome",
    )?;
    expect_equal(
        &h.ledger_state("e1", "g").await?,
        &Some("settled".to_string()),
        "the endpoint is terminal",
    )?;
    let recovered = h
        .brain(None, Some("brain"), Some(Arc::clone(&approver.port)))
        .await?
        .run(plan)
        .await?;
    expect_equal(&recovered, &result, "recovery decides the same")?;
    expect_equal(
        &approver.requests().len(),
        &1usize,
        "recovery asks nobody again",
    )?;
    let _ = std::marker::PhantomData::<ExecutionOutcome>;
    Ok(())
}

async fn expiry_while_awaiting(adapters: BatchCaseAdapters) -> ConformanceResult {
    expired_while_awaiting(
        batch_harness(adapters).await?,
        ConsentAnswer::Expired,
        false,
    )
    .await
}

async fn late_approval(adapters: BatchCaseAdapters) -> ConformanceResult {
    expired_while_awaiting(
        batch_harness(adapters).await?,
        ConsentAnswer::Approved,
        true,
    )
    .await
}

pub fn consent_cases() -> BTreeMap<&'static str, ConsentCase> {
    let mut map: BTreeMap<&'static str, ConsentCase> = BTreeMap::new();
    map.insert(
        "CONSENT-01",
        Arc::new(|a| Box::pin(expiry_while_awaiting(a))),
    );
    map.insert("CONSENT-02", Arc::new(|a| Box::pin(late_approval(a))));
    map
}

pub const CONSENT_CASES: fn() -> BTreeMap<&'static str, ConsentCase> = consent_cases;
