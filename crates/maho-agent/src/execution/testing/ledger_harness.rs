//! Port of senpi `packages/agent/src/execution/testing/ledger-harness.ts`.

use std::sync::Arc;

use serde_json::json;

use crate::durable::store::{DurableStore, DurableStoreOptions};
use crate::durable::store_admission::create_admission_line;
use crate::durable::testing::store_cases::claimed_store;
use crate::durable::testing::store_harness::{StoreConformanceAdapter, expect_equal};
use crate::execution::ledger::{
    ExecutionLedger, StoreExecutionLedgerOptions, create_execution_ledger,
};
use crate::execution::ledger_types::{
    Actor, ExecutionAdmission, ExecutionCommand, ExecutionEntry, ExecutionEvent, ExecutionKey,
    FrozenCall, LedgerReply,
};

pub fn test_admission() -> ExecutionAdmission {
    ExecutionAdmission {
        issuer: "brain".to_string(),
        command_id: "call".to_string(),
        executor: "hand".to_string(),
        workspace: "workspace".to_string(),
        scope: "scope".to_string(),
        generation: 1,
        request_digest: "raw".to_string(),
        admitted_at: 100,
        lease_until: 60_100,
        reconcilable: false,
    }
}

pub fn frozen_call() -> FrozenCall {
    FrozenCall {
        name: "read".to_string(),
        args: json!({ "path": "a" }),
        digest: "frozen".to_string(),
    }
}

pub fn prepare_events() -> Vec<ExecutionEvent> {
    vec![
        ExecutionEvent::Prepare,
        ExecutionEvent::Effect,
        ExecutionEvent::Freeze {
            call: frozen_call(),
            awaiting_consent: false,
        },
    ]
}

pub fn dispatch_events() -> Vec<ExecutionEvent> {
    let mut events = prepare_events();
    events.push(ExecutionEvent::Release);
    events.push(ExecutionEvent::Dispatch);
    events
}

pub fn command(event: ExecutionEvent, now: Option<i64>) -> ExecutionCommand {
    let is_brain = matches!(
        event,
        ExecutionEvent::Consent
            | ExecutionEvent::Release
            | ExecutionEvent::Abort
            | ExecutionEvent::Ack
            | ExecutionEvent::Renew { .. }
            | ExecutionEvent::Cancel
    );
    let adm = test_admission();
    ExecutionCommand {
        key: ExecutionKey {
            issuer: adm.issuer,
            command_id: adm.command_id,
        },
        generation: 1,
        actor: Actor {
            id: if is_brain { "brain" } else { "hand" }.to_string(),
            approved_workspace: None,
        },
        now: now.unwrap_or(101),
        event,
    }
}

pub fn accepted(reply: LedgerReply) -> Result<ExecutionEntry, String> {
    match reply {
        LedgerReply::Applied { entry } | LedgerReply::Replayed { entry } => Ok(entry),
        other => Err(format!(
            "expected an execution row, got {}",
            serde_json::to_string(&other).unwrap_or_default()
        )),
    }
}

pub fn ledger_on(store: Arc<dyn DurableStore>, epoch: u64) -> Arc<dyn ExecutionLedger> {
    create_execution_ledger(StoreExecutionLedgerOptions {
        store,
        epoch,
        admission: create_admission_line(),
    })
}

pub struct LedgerFixture {
    pub medium: String,
    pub store: Arc<dyn DurableStore>,
    pub ledger: Arc<dyn ExecutionLedger>,
    pub adapter: Arc<dyn StoreConformanceAdapter>,
}

impl LedgerFixture {
    pub async fn reopen(
        &self,
        options: Option<DurableStoreOptions>,
    ) -> Result<Arc<dyn ExecutionLedger>, String> {
        let store = self
            .adapter
            .open(&self.medium, options.unwrap_or_default())
            .await
            .map_err(|e| e.to_string())?;
        Ok(ledger_on(store, 1))
    }
}

pub async fn fixture(
    adapter: Arc<dyn StoreConformanceAdapter>,
    input: Option<ExecutionAdmission>,
) -> Result<LedgerFixture, String> {
    let (medium, store) = claimed_store(adapter.as_ref(), 1).await?;
    let ledger = ledger_on(store.clone(), 1);
    let adm = input.unwrap_or_else(test_admission);
    let reply = ledger.admit(adm, None).await.map_err(|e| e.to_string())?;
    let status = match reply {
        LedgerReply::Applied { .. } => "applied",
        LedgerReply::Replayed { .. } => "replayed",
        LedgerReply::Refused { .. } => "refused",
        LedgerReply::ResultExpired => "result_expired",
    };
    expect_equal(&status, &"applied", "admitted")?;
    Ok(LedgerFixture {
        medium,
        store,
        ledger,
        adapter,
    })
}

pub async fn run_events(
    ledger: &dyn ExecutionLedger,
    events: &[ExecutionEvent],
) -> Result<(), String> {
    for event in events {
        let kind = format!("{event:?}");
        let reply = ledger
            .apply(command(event.clone(), None), None)
            .await
            .map_err(|e| e.to_string())?;
        let status = match reply {
            LedgerReply::Applied { .. } => "applied",
            LedgerReply::Replayed { .. } => "replayed",
            LedgerReply::Refused { .. } => "refused",
            LedgerReply::ResultExpired => "result_expired",
        };
        expect_equal(&status, &"applied", &kind)?;
    }
    Ok(())
}

pub async fn row(ledger: &dyn ExecutionLedger) -> Result<ExecutionEntry, String> {
    let adm = test_admission();
    let key = adm.key();
    ledger
        .read(key)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the admitted execution disappeared".to_string())
}
