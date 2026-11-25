//! Port of senpi `packages/agent/src/execution/testing/ledger-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::RecordScan;
use crate::durable::testing::store_harness::{StoreCaseAdapters, expect_equal};
use crate::execution::ledger_types::{
    ExecutionEvent, ExecutionOutcome, ExecutionProgress, LedgerReply, RefusalReason,
};

use super::ledger_harness::{
    command, dispatch_events, fixture, frozen_call, prepare_events, row, run_events, test_admission,
};
use super::ledger_recovery_cases::ledger_recovery_cases;

pub type LedgerCase = CaseImplementation<StoreCaseAdapters>;

fn reply_status(reply: &LedgerReply) -> &'static str {
    match reply {
        LedgerReply::Applied { .. } => "applied",
        LedgerReply::Replayed { .. } => "replayed",
        LedgerReply::Refused { .. } => "refused",
        LedgerReply::ResultExpired => "result_expired",
    }
}

async fn identity(adapters: StoreCaseAdapters) -> ConformanceResult {
    let fix = fixture(adapters.store, None).await?;
    let base = test_admission();
    expect_equal(
        &reply_status(
            &fix.ledger
                .admit(base.clone(), None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"replayed",
        "same identity",
    )?;
    let mut changes = vec![base.clone(), base.clone(), base.clone(), base.clone()];
    changes[0].scope = "other".to_string();
    changes[1].generation = 2;
    changes[2].request_digest = "changed".to_string();
    changes[3].executor = "other".to_string();
    for change in changes {
        expect_equal(
            &fix.ledger
                .admit(change, None)
                .await
                .map_err(|e| e.to_string())?,
            &LedgerReply::Refused {
                reason: RefusalReason::IdentityConflict,
            },
            "binding",
        )?;
    }
    let mut another_issuer = base;
    another_issuer.issuer = "another".to_string();
    expect_equal(
        &reply_status(
            &fix.ledger
                .admit(another_issuer, None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"applied",
        "issuer namespaces identity",
    )?;
    let mut stale_cmd = command(ExecutionEvent::Prepare, None);
    stale_cmd.generation = 2;
    expect_equal(
        &fix.ledger
            .apply(stale_cmd, None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::StaleGeneration,
        },
        "generation fence",
    )?;
    Ok(())
}

async fn preparation(adapters: StoreCaseAdapters) -> ConformanceResult {
    let fix = fixture(adapters.store.clone(), None).await?;
    expect_equal(
        &fix.ledger
            .apply(command(ExecutionEvent::Effect, None), None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::InvalidTransition,
        },
        "no effects before prepare",
    )?;
    fix.ledger
        .apply(command(ExecutionEvent::Prepare, None), None)
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &fix.ledger
            .apply(
                command(
                    ExecutionEvent::Freeze {
                        call: frozen_call(),
                        awaiting_consent: false,
                    },
                    None,
                ),
                None,
            )
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::InvalidTransition,
        },
        "freeze requires effect marker",
    )?;
    fix.ledger
        .apply(command(ExecutionEvent::Effect, None), None)
        .await
        .map_err(|e| e.to_string())?;
    let reopened = fix.reopen(None).await?;
    expect_equal(
        &row(reopened.as_ref()).await?.effect_possible,
        &true,
        "effect marker survives reopen before a callback may run",
    )?;
    expect_equal(
        &reply_status(
            &fix.ledger
                .apply(command(ExecutionEvent::Effect, None), None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"replayed",
        "effects never granted twice",
    )?;
    fix.ledger
        .apply(
            command(
                ExecutionEvent::Freeze {
                    call: frozen_call(),
                    awaiting_consent: false,
                },
                None,
            ),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &row(fix.reopen(None).await?.as_ref()).await?.progress,
        &ExecutionProgress::Prepared {
            call: frozen_call(),
        },
        "frozen bytes survive reopen",
    )?;
    expect_equal(
        &fix.ledger
            .apply(
                command(
                    ExecutionEvent::Block {
                        value: json!("late"),
                    },
                    None,
                ),
                None,
            )
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::InvalidTransition,
        },
        "a frozen call cannot be blocked",
    )?;

    let blocked = fixture(adapters.store, None).await?;
    let mut events = prepare_events()[..2].to_vec();
    events.push(ExecutionEvent::Block {
        value: json!({ "reply": "blocked" }),
    });
    run_events(blocked.ledger.as_ref(), &events).await?;
    expect_equal(
        &row(blocked.reopen(None).await?.as_ref()).await?.progress,
        &ExecutionProgress::Settled {
            outcome: ExecutionOutcome::Blocked {
                value: json!({ "reply": "blocked" }),
                hooks_ran: true,
                uncertain: false,
            },
        },
        "a blocked preparation is a durable terminal outcome",
    )?;
    blocked
        .reopen(None)
        .await?
        .recover(102)
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &row(blocked.reopen(None).await?.as_ref())
            .await?
            .progress
            .state(),
        &"settled",
        "restart recovery keeps a blocked call",
    )?;
    Ok(())
}

async fn consent(adapters: StoreCaseAdapters) -> ConformanceResult {
    let fix = fixture(adapters.store, None).await?;
    let mut events = prepare_events()[..2].to_vec();
    events.push(ExecutionEvent::Freeze {
        call: frozen_call(),
        awaiting_consent: true,
    });
    run_events(fix.ledger.as_ref(), &events).await?;
    expect_equal(
        &fix.ledger
            .apply(command(ExecutionEvent::Release, None), None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::InvalidTransition,
        },
        "consent wait cannot release",
    )?;
    let mut wrong_actor = command(ExecutionEvent::Consent, None);
    wrong_actor.actor.id = "other".to_string();
    expect_equal(
        &fix.ledger
            .apply(wrong_actor, None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::Unauthorized,
        },
        "consent issuer",
    )?;
    run_events(
        fix.ledger.as_ref(),
        &[
            ExecutionEvent::Consent,
            ExecutionEvent::Release,
            ExecutionEvent::Dispatch,
        ],
    )
    .await?;
    expect_equal(
        &row(fix.ledger.as_ref()).await?.progress,
        &ExecutionProgress::Dispatched {
            call: frozen_call(),
        },
        "consent releases the frozen call without preparation",
    )?;
    Ok(())
}

async fn dispatch(adapters: StoreCaseAdapters) -> ConformanceResult {
    let fix = fixture(adapters.store, None).await?;
    expect_equal(
        &fix.ledger
            .apply(command(ExecutionEvent::Dispatch, None), None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::InvalidTransition,
        },
        "no dispatch without release",
    )?;
    run_events(fix.ledger.as_ref(), &dispatch_events()).await?;
    expect_equal(
        &reply_status(
            &fix.reopen(None)
                .await?
                .apply(command(ExecutionEvent::Dispatch, None), None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"replayed",
        "durable dispatch is never granted twice",
    )?;
    expect_equal(
        &reply_status(
            &fix.ledger
                .admit(test_admission(), None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"replayed",
        "invoke replay",
    )?;
    Ok(())
}

async fn cancel_first(adapters: StoreCaseAdapters) -> ConformanceResult {
    let prep = prepare_events();
    let disp = dispatch_events();
    let slices: Vec<&[ExecutionEvent]> = vec![&[], &prep[..2], &disp];
    for before in slices {
        let fix = fixture(adapters.store.clone(), None).await?;
        run_events(fix.ledger.as_ref(), before).await?;
        fix.ledger
            .apply(command(ExecutionEvent::Cancel, None), None)
            .await
            .map_err(|e| e.to_string())?;
        let terminal = row(fix.ledger.as_ref()).await?;
        let expected_hooks = !before.is_empty();
        expect_equal(
            &terminal.progress,
            &ExecutionProgress::Settled {
                outcome: ExecutionOutcome::Cancelled {
                    hooks_ran: expected_hooks,
                    uncertain: expected_hooks,
                },
            },
            "cancel projection follows the effect boundary",
        )?;
        fix.ledger
            .apply(
                command(
                    ExecutionEvent::Result {
                        value: json!("late"),
                    },
                    None,
                ),
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        expect_equal(
            &row(fix.reopen(None).await?.as_ref()).await?,
            &terminal,
            "late result cannot replace cancellation",
        )?;
        let audits = fix
            .store
            .scan_records(RecordScan {
                kind: Some("execution.audit".to_string()),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        expect_equal(&audits.len(), &1usize, "late result is audited")?;
    }
    Ok(())
}

async fn result_first(adapters: StoreCaseAdapters) -> ConformanceResult {
    let fix = fixture(adapters.store, None).await?;
    run_events(fix.ledger.as_ref(), &dispatch_events()).await?;
    fix.ledger
        .apply(
            command(
                ExecutionEvent::Result {
                    value: json!({ "content": "done" }),
                },
                None,
            ),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    let terminal = row(fix.ledger.as_ref()).await?;
    fix.ledger
        .apply(command(ExecutionEvent::Cancel, None), None)
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &row(fix.reopen(None).await?.as_ref()).await?,
        &terminal,
        "result-first is immutable",
    )?;
    for event in [
        ExecutionEvent::Release,
        ExecutionEvent::Abort,
        ExecutionEvent::Consent,
    ] {
        let what = format!("{event:?} on terminal");
        expect_equal(
            &fix.ledger
                .apply(command(event, None), None)
                .await
                .map_err(|e| e.to_string())?,
            &LedgerReply::Refused {
                reason: RefusalReason::Terminal,
            },
            &what,
        )?;
    }
    Ok(())
}

pub fn ledger_cases() -> BTreeMap<&'static str, LedgerCase> {
    let mut map: BTreeMap<&'static str, LedgerCase> = BTreeMap::new();
    map.insert("LEDGER-01", Arc::new(|a| Box::pin(identity(a))));
    map.insert("LEDGER-02", Arc::new(|a| Box::pin(preparation(a))));
    map.insert("LEDGER-03", Arc::new(|a| Box::pin(consent(a))));
    map.insert("LEDGER-04", Arc::new(|a| Box::pin(dispatch(a))));
    map.insert("LEDGER-05", Arc::new(|a| Box::pin(cancel_first(a))));
    map.insert("LEDGER-06", Arc::new(|a| Box::pin(result_first(a))));
    map.extend(ledger_recovery_cases());
    map
}

pub const LEDGER_CASES: fn() -> BTreeMap<&'static str, LedgerCase> = ledger_cases;
