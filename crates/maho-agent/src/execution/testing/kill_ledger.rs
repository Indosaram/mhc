//! Port of senpi `packages/agent/src/execution/testing/kill-ledger.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::{DurableStoreError, StoreOperation, StoreWritePoint};
use crate::durable::testing::store_cases::claimed_store;
use crate::durable::testing::store_harness::{
    StoreCaseAdapters, cut_once_at, expect_cut, expect_equal,
};
use crate::execution::ledger_types::{ExecutionEvent, ExecutionProgress, LedgerReply};

use super::ledger_harness::{
    command, dispatch_events, frozen_call, ledger_on, prepare_events, run_events, test_admission,
};

#[derive(Clone)]
struct Transition {
    name: &'static str,
    before: Vec<ExecutionEvent>,
    event: Option<ExecutionEvent>,
    now: Option<i64>,
    reconcilable: bool,
    expected: &'static str,
}

fn transitions() -> Vec<Transition> {
    let prep = prepare_events();
    let disp = dispatch_events();
    let mut done_ack = disp.clone();
    done_ack.push(ExecutionEvent::Result {
        value: json!("done"),
    });
    let mut await_consent_before = prep[..2].to_vec();
    await_consent_before.push(ExecutionEvent::Freeze {
        call: frozen_call(),
        awaiting_consent: true,
    });

    vec![
        Transition {
            name: "admit",
            before: vec![],
            event: None,
            now: None,
            reconcilable: false,
            expected: "admitted",
        },
        Transition {
            name: "prepare",
            before: vec![],
            event: Some(ExecutionEvent::Prepare),
            now: None,
            reconcilable: false,
            expected: "preparing",
        },
        Transition {
            name: "effect",
            before: prep[..1].to_vec(),
            event: Some(ExecutionEvent::Effect),
            now: None,
            reconcilable: false,
            expected: "preparing",
        },
        Transition {
            name: "freeze",
            before: prep[..2].to_vec(),
            event: Some(ExecutionEvent::Freeze {
                call: frozen_call(),
                awaiting_consent: false,
            }),
            now: None,
            reconcilable: false,
            expected: "prepared",
        },
        Transition {
            name: "await-consent",
            before: prep[..2].to_vec(),
            event: Some(ExecutionEvent::Freeze {
                call: frozen_call(),
                awaiting_consent: true,
            }),
            now: None,
            reconcilable: false,
            expected: "prepared_awaiting_consent",
        },
        Transition {
            name: "consent",
            before: await_consent_before,
            event: Some(ExecutionEvent::Consent),
            now: None,
            reconcilable: false,
            expected: "prepared",
        },
        Transition {
            name: "release",
            before: prep.clone(),
            event: Some(ExecutionEvent::Release),
            now: None,
            reconcilable: false,
            expected: "released",
        },
        Transition {
            name: "dispatch",
            before: disp[..4].to_vec(),
            event: Some(ExecutionEvent::Dispatch),
            now: None,
            reconcilable: false,
            expected: "dispatched",
        },
        Transition {
            name: "result",
            before: disp.clone(),
            event: Some(ExecutionEvent::Result {
                value: json!("done"),
            }),
            now: None,
            reconcilable: false,
            expected: "settled",
        },
        Transition {
            name: "cancel",
            before: disp.clone(),
            event: Some(ExecutionEvent::Cancel),
            now: None,
            reconcilable: false,
            expected: "settled",
        },
        Transition {
            name: "abort",
            before: prep.clone(),
            event: Some(ExecutionEvent::Abort),
            now: None,
            reconcilable: false,
            expected: "settled",
        },
        Transition {
            name: "renew",
            before: vec![],
            event: Some(ExecutionEvent::Renew {
                seq: 1,
                lease_until: 70_100,
            }),
            now: None,
            reconcilable: false,
            expected: "admitted",
        },
        Transition {
            name: "expire",
            before: prep.clone(),
            event: Some(ExecutionEvent::Expire),
            now: Some(60_100),
            reconcilable: false,
            expected: "settled",
        },
        Transition {
            name: "restart",
            before: prep[..2].to_vec(),
            event: Some(ExecutionEvent::Restart),
            now: None,
            reconcilable: false,
            expected: "settled",
        },
        Transition {
            name: "reconcile",
            before: disp,
            event: Some(ExecutionEvent::Restart),
            now: None,
            reconcilable: true,
            expected: "reconciling",
        },
        Transition {
            name: "ack",
            before: done_ack,
            event: Some(ExecutionEvent::Ack),
            now: None,
            reconcilable: false,
            expected: "acked",
        },
    ]
}

const POINTS: &[StoreWritePoint] = &[
    StoreWritePoint::Before {
        op: StoreOperation::Commit,
    },
    StoreWritePoint::Write {
        op: StoreOperation::Commit,
        index: 0,
    },
    StoreWritePoint::Write {
        op: StoreOperation::Commit,
        index: 1,
    },
    StoreWritePoint::Durable {
        op: StoreOperation::Commit,
    },
];

#[derive(Debug, Clone)]
pub struct KillLedgerDeclaration {
    pub id: &'static str,
    pub operation: &'static str,
    pub point: StoreWritePoint,
    pub placement: &'static str,
}

async fn run_kill_case(
    adapters: StoreCaseAdapters,
    transition: Transition,
    point: StoreWritePoint,
) -> ConformanceResult {
    let (medium, store) = claimed_store(adapters.store.as_ref(), 1).await?;
    let ledger = ledger_on(store.clone(), 1);
    let mut input = test_admission();
    input.reconcilable = transition.reconcilable;
    if transition.event.is_some() {
        ledger
            .admit(input.clone(), None)
            .await
            .map_err(|e| e.to_string())?;
    }
    run_events(ledger.as_ref(), &transition.before).await?;
    let baseline = store.export_snapshot().await.map_err(|e| e.to_string())?;
    let cut_store = adapters
        .store
        .open(&medium, cut_once_at(point))
        .await
        .map_err(|e| e.to_string())?;
    let cut = ledger_on(cut_store, 1);
    let what = format!("{}/{point:?}", transition.name);
    match &transition.event {
        None => expect_cut(
            cut.admit(input.clone(), None)
                .await
                .map_err(DurableStoreError::from),
            &what,
        )?,
        Some(ev) => expect_cut(
            cut.apply(command(ev.clone(), transition.now), None)
                .await
                .map_err(DurableStoreError::from),
            &what,
        )?,
    };

    let reopened_store = adapters
        .store
        .open(&medium, Default::default())
        .await
        .map_err(|e| e.to_string())?;
    let reopened = ledger_on(reopened_store.clone(), 1);
    let snapshot = reopened_store
        .export_snapshot()
        .await
        .map_err(|e| e.to_string())?;
    let key = input.key();
    if !matches!(point, StoreWritePoint::Durable { .. }) {
        expect_equal(
            &snapshot,
            &baseline,
            "a cut before durable changes neither journal nor row",
        )?;
    } else {
        expect_equal(
            &snapshot.head,
            &(baseline.head + 1),
            "exactly one durable transition",
        )?;
        let entry = reopened
            .read(key.clone())
            .await
            .map_err(|e| e.to_string())?;
        let state = entry.as_ref().map(|e| e.progress.state());
        expect_equal(
            &state,
            &Some(transition.expected),
            "the complete endpoint transition survived",
        )?;
        if transition.name == "effect" {
            expect_equal(
                &entry.as_ref().map(|e| e.effect_possible),
                &Some(true),
                "effect permission durable before callback",
            )?;
        }
        if transition.name == "dispatch" {
            expect_equal(
                &entry.as_ref().map(|e| e.body_dispatched),
                &Some(true),
                "dispatch permission is durable before body",
            )?;
            let replay = reopened
                .apply(command(ExecutionEvent::Dispatch, None), None)
                .await
                .map_err(|e| e.to_string())?;
            let status = match replay {
                LedgerReply::Replayed { .. } => "replayed",
                _ => "other",
            };
            expect_equal(&status, &"replayed", "no second dispatch after crash")?;
        }
        if transition.name == "renew" {
            expect_equal(
                &entry.as_ref().map(|e| e.renew_seq),
                &Some(1u64),
                "renewal sequence survives",
            )?;
        }
    }
    reopened
        .recover(transition.now.unwrap_or(102))
        .await
        .map_err(|e| e.to_string())?;
    let recovered = reopened.read(key).await.map_err(|e| e.to_string())?;
    if let Some(entry) = &recovered {
        if matches!(
            entry.progress,
            ExecutionProgress::Preparing | ExecutionProgress::Dispatched { .. }
        ) {
            return Err("restart left a non-resumable execution live".to_string());
        }
        let replay = reopened
            .admit(input, None)
            .await
            .map_err(|e| e.to_string())?;
        let replay_status = match replay {
            LedgerReply::Applied { .. } => "applied",
            LedgerReply::Replayed { .. } => "replayed",
            LedgerReply::Refused { .. } => "refused",
            LedgerReply::ResultExpired => "result_expired",
        };
        let expected_status = if matches!(entry.progress, ExecutionProgress::Acked) {
            "result_expired"
        } else {
            "replayed"
        };
        expect_equal(
            &replay_status,
            &expected_status,
            "identity survives restart",
        )?;
    }
    Ok(())
}

pub fn kill_ledger_declarations() -> Vec<KillLedgerDeclaration> {
    let mut out = Vec::new();
    for (index, transition) in transitions().into_iter().enumerate() {
        for (cut_index, point) in POINTS.iter().copied().enumerate() {
            let id_str = format!("KILL-LEDGER-{:02}-{}", index + 1, cut_index + 1);
            let id: &'static str = Box::leak(id_str.into_boxed_str());
            out.push(KillLedgerDeclaration {
                id,
                operation: transition.name,
                point,
                placement: "everywhere",
            });
        }
    }
    out
}

pub fn kill_ledger_cases() -> BTreeMap<&'static str, CaseImplementation<StoreCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<StoreCaseAdapters>> = BTreeMap::new();
    for (index, transition) in transitions().into_iter().enumerate() {
        for (cut_index, point) in POINTS.iter().copied().enumerate() {
            let id_str = format!("KILL-LEDGER-{:02}-{}", index + 1, cut_index + 1);
            let id: &'static str = Box::leak(id_str.into_boxed_str());
            let tr = transition.clone();
            map.insert(
                id,
                Arc::new(move |adapters| {
                    let tr_c = tr.clone();
                    Box::pin(run_kill_case(adapters, tr_c, point))
                }),
            );
        }
    }
    map
}

pub const KILL_LEDGER_DECLARATIONS: fn() -> Vec<KillLedgerDeclaration> = kill_ledger_declarations;
pub const KILL_LEDGER_CASES: fn() -> BTreeMap<&'static str, CaseImplementation<StoreCaseAdapters>> =
    kill_ledger_cases;
