//! Port of senpi `packages/agent/src/execution/testing/ledger-recovery-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::{DurableStoreError, DurableStoreErrorCode, DurableStoreOptions};
use crate::durable::testing::store_harness::{StoreCaseAdapters, expect_equal, expect_store_error};
use crate::execution::ledger_types::{
    Actor, ExecutionEvent, ExecutionOutcome, ExecutionProgress, LedgerReply,
    MAX_EXECUTION_LIFETIME_MS, RefusalReason,
};

use super::ledger_harness::{
    accepted, command, dispatch_events, fixture, frozen_call, ledger_on, prepare_events, row,
    run_events, test_admission,
};

type LedgerCase = CaseImplementation<StoreCaseAdapters>;

fn reply_status(reply: &LedgerReply) -> &'static str {
    match reply {
        LedgerReply::Applied { .. } => "applied",
        LedgerReply::Replayed { .. } => "replayed",
        LedgerReply::Refused { .. } => "refused",
        LedgerReply::ResultExpired => "result_expired",
    }
}

async fn renewal(adapters: StoreCaseAdapters) -> ConformanceResult {
    let fix = fixture(adapters.store, None).await?;
    let renew = ExecutionEvent::Renew {
        seq: 1,
        lease_until: 100_000,
    };
    let mut wrong_actor = command(renew.clone(), None);
    wrong_actor.actor.id = "hand".to_string();
    expect_equal(
        &fix.ledger
            .apply(wrong_actor, None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::Unauthorized,
        },
        "issuer-only renewal",
    )?;
    expect_equal(
        &reply_status(
            &fix.ledger
                .apply(command(renew.clone(), None), None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"applied",
        "renewal starts at admission",
    )?;
    expect_equal(
        &reply_status(
            &fix.ledger
                .apply(command(renew, None), None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"replayed",
        "duplicate renewal",
    )?;
    let adm = test_admission();
    expect_equal(
        &fix.ledger
            .apply(
                command(
                    ExecutionEvent::Renew {
                        seq: 2,
                        lease_until: adm.admitted_at + MAX_EXECUTION_LIFETIME_MS + 1,
                    },
                    None,
                ),
                None,
            )
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::InvalidRenewal,
        },
        "24-hour cap",
    )?;
    expect_equal(
        &fix.ledger
            .apply(
                command(
                    ExecutionEvent::Renew {
                        seq: 2,
                        lease_until: 90_000,
                    },
                    None,
                ),
                None,
            )
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::InvalidRenewal,
        },
        "lease cannot shrink",
    )?;
    fix.ledger
        .apply(command(ExecutionEvent::Cancel, None), None)
        .await
        .map_err(|e| e.to_string())?;
    let terminal = row(fix.ledger.as_ref()).await?;
    let after_terminal = accepted(
        fix.ledger
            .apply(
                command(
                    ExecutionEvent::Renew {
                        seq: 3,
                        lease_until: 100_000,
                    },
                    None,
                ),
                None,
            )
            .await
            .map_err(|e| e.to_string())?,
    )?;
    expect_equal(
        &after_terminal,
        &terminal,
        "terminal renewal returns stored outcome",
    )?;
    Ok(())
}

async fn acknowledgement(adapters: StoreCaseAdapters) -> ConformanceResult {
    let fix = fixture(adapters.store, None).await?;
    expect_equal(
        &fix.ledger
            .apply(command(ExecutionEvent::Ack, None), None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::InvalidTransition,
        },
        "early ACK",
    )?;
    fix.ledger
        .apply(command(ExecutionEvent::Cancel, None), None)
        .await
        .map_err(|e| e.to_string())?;
    let mut wrong_actor = command(ExecutionEvent::Ack, None);
    wrong_actor.actor.id = "other".to_string();
    expect_equal(
        &fix.ledger
            .apply(wrong_actor, None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::Unauthorized,
        },
        "issuer-only ACK",
    )?;
    fix.ledger
        .apply(command(ExecutionEvent::Ack, None), None)
        .await
        .map_err(|e| e.to_string())?;
    let reopened = fix.reopen(None).await?;
    expect_equal(
        &reply_status(
            &reopened
                .apply(command(ExecutionEvent::Ack, None), None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"replayed",
        "duplicate ACK",
    )?;
    expect_equal(
        &reopened
            .admit(test_admission(), None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::ResultExpired,
        "dedup after ACK",
    )?;
    expect_equal(
        &row(reopened.as_ref()).await?.progress,
        &ExecutionProgress::Acked,
        "payload removed, identity retained",
    )?;
    Ok(())
}

async fn preparing_restart(adapters: StoreCaseAdapters) -> ConformanceResult {
    let prep = prepare_events();
    for effects in [false, true] {
        let fix = fixture(adapters.store.clone(), None).await?;
        let slice = if effects { &prep[..2] } else { &prep[..1] };
        run_events(fix.ledger.as_ref(), slice).await?;
        let reopened = fix.reopen(None).await?;
        reopened.recover(102).await.map_err(|e| e.to_string())?;
        expect_equal(
            &row(reopened.as_ref()).await?.progress,
            &ExecutionProgress::Settled {
                outcome: ExecutionOutcome::HandRestarted {
                    hooks_ran: effects,
                    uncertain: effects,
                },
            },
            "preparing never reruns after restart",
        )?;
        expect_equal(
            &reply_status(
                &reopened
                    .apply(command(ExecutionEvent::Prepare, Some(103)), None)
                    .await
                    .map_err(|e| e.to_string())?,
            ),
            &"replayed",
            "no new preparation permission",
        )?;
    }
    Ok(())
}

async fn prepared_restart(adapters: StoreCaseAdapters) -> ConformanceResult {
    let prep = prepare_events();
    for state in ["prepared", "prepared_awaiting_consent", "released"] {
        let fix = fixture(adapters.store.clone(), None).await?;
        let mut events = prep[..2].to_vec();
        events.push(ExecutionEvent::Freeze {
            call: frozen_call(),
            awaiting_consent: state == "prepared_awaiting_consent",
        });
        run_events(fix.ledger.as_ref(), &events).await?;
        if state == "released" {
            fix.ledger
                .apply(command(ExecutionEvent::Release, None), None)
                .await
                .map_err(|e| e.to_string())?;
        }
        let reopened = fix.reopen(None).await?;
        reopened.recover(102).await.map_err(|e| e.to_string())?;
        let expected = match state {
            "prepared" => ExecutionProgress::Prepared {
                call: frozen_call(),
            },
            "prepared_awaiting_consent" => ExecutionProgress::PreparedAwaitingConsent {
                call: frozen_call(),
            },
            _ => ExecutionProgress::Released {
                call: frozen_call(),
            },
        };
        expect_equal(
            &row(reopened.as_ref()).await?.progress,
            &expected,
            "frozen state survives restart",
        )?;
    }
    Ok(())
}

async fn reconciliation(adapters: StoreCaseAdapters) -> ConformanceResult {
    for reconcilable in [false, true] {
        let mut adm = test_admission();
        adm.reconcilable = reconcilable;
        let fix = fixture(adapters.store.clone(), Some(adm)).await?;
        run_events(fix.ledger.as_ref(), &dispatch_events()).await?;
        let reopened = fix.reopen(None).await?;
        reopened.recover(102).await.map_err(|e| e.to_string())?;
        if !reconcilable {
            expect_equal(
                &row(reopened.as_ref()).await?.progress,
                &ExecutionProgress::Settled {
                    outcome: ExecutionOutcome::HandRestarted {
                        hooks_ran: true,
                        uncertain: true,
                    },
                },
                "unreconcilable dispatch",
            )?;
            continue;
        }
        expect_equal(
            &row(reopened.as_ref()).await?.progress,
            &ExecutionProgress::Reconciling {
                call: frozen_call(),
                until: 30_102,
            },
            "30-second grace",
        )?;
        reopened.recover(103).await.map_err(|e| e.to_string())?;
        expect_equal(
            &row(reopened.as_ref()).await?.progress,
            &ExecutionProgress::Reconciling {
                call: frozen_call(),
                until: 30_102,
            },
            "another restart cannot extend grace",
        )?;
        reopened
            .apply(command(ExecutionEvent::Expire, Some(30_102)), None)
            .await
            .map_err(|e| e.to_string())?;
        expect_equal(
            &row(reopened.as_ref()).await?.progress.state(),
            &"settled",
            "grace deadline settles without dispatching again",
        )?;
    }

    let mut adm = test_admission();
    adm.reconcilable = true;
    let fix = fixture(adapters.store, Some(adm)).await?;
    run_events(fix.ledger.as_ref(), &dispatch_events()).await?;
    let restarted = fix.reopen(None).await?;
    restarted.recover(102).await.map_err(|e| e.to_string())?;
    restarted
        .apply(
            command(
                ExecutionEvent::Result {
                    value: json!("reconciled"),
                },
                Some(103),
            ),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &row(restarted.as_ref()).await?.progress,
        &ExecutionProgress::Settled {
            outcome: ExecutionOutcome::Result {
                value: json!("reconciled"),
                hooks_ran: true,
                uncertain: false,
            },
        },
        "authoritative reconciliation resolves uncertainty",
    )?;
    Ok(())
}

async fn expiry_and_fencing(adapters: StoreCaseAdapters) -> ConformanceResult {
    let fix = fixture(adapters.store.clone(), None).await?;
    run_events(fix.ledger.as_ref(), &prepare_events()).await?;
    let adm = test_admission();
    let expired = accepted(
        fix.ledger
            .apply(
                command(ExecutionEvent::Release, Some(adm.lease_until)),
                None,
            )
            .await
            .map_err(|e| e.to_string())?,
    )?;
    expect_equal(
        &expired.progress,
        &ExecutionProgress::Settled {
            outcome: ExecutionOutcome::LeaseExpired {
                hooks_ran: true,
                uncertain: true,
            },
        },
        "lease reason retained at boundary",
    )?;

    let next = adapters
        .store
        .open(&fix.medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    next.claim(1, 2).await.map_err(|e| e.to_string())?;
    let mut old_adm = adm.clone();
    old_adm.command_id = "old".to_string();
    expect_store_error(
        fix.ledger
            .admit(old_adm, None)
            .await
            .map_err(DurableStoreError::from),
        DurableStoreErrorCode::StaleEpoch,
        "old owner cannot admit",
    )?;
    let current = ledger_on(next, 2);
    let mut new_adm = adm.clone();
    new_adm.command_id = "new".to_string();
    expect_equal(
        &reply_status(
            &current
                .admit(new_adm, None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"applied",
        "new owner",
    )?;
    expect_equal(
        &fix.store.read_epoch().await.map_err(|e| e.to_string())?,
        &2u64,
        "fence shared by reopened instances",
    )?;

    let concurrent = fixture(adapters.store, None).await?;
    let cmd1 = command(ExecutionEvent::Prepare, None);
    let cmd2 = command(ExecutionEvent::Prepare, None);
    let (r1, r2) = tokio::join!(
        concurrent.ledger.apply(cmd1, None),
        concurrent.ledger.apply(cmd2, None)
    );
    let s1 = reply_status(&r1.map_err(|e| e.to_string())?);
    let s2 = reply_status(&r2.map_err(|e| e.to_string())?);
    expect_equal(
        &vec![s1, s2],
        &vec!["applied", "replayed"],
        "one preparation grant under concurrency",
    )?;

    let wrong_approver = crate::execution::ledger_types::ExecutionCommand {
        key: adm.key(),
        generation: 1,
        actor: Actor {
            id: "approver".to_string(),
            approved_workspace: Some("wrong".to_string()),
        },
        now: 101,
        event: ExecutionEvent::Cancel,
    };
    expect_equal(
        &concurrent
            .ledger
            .apply(wrong_approver, None)
            .await
            .map_err(|e| e.to_string())?,
        &LedgerReply::Refused {
            reason: RefusalReason::Unauthorized,
        },
        "approval cannot cross workspaces",
    )?;
    let ok_approver = crate::execution::ledger_types::ExecutionCommand {
        key: adm.key(),
        generation: 1,
        actor: Actor {
            id: "approver".to_string(),
            approved_workspace: Some(adm.workspace),
        },
        now: 101,
        event: ExecutionEvent::Cancel,
    };
    expect_equal(
        &reply_status(
            &concurrent
                .ledger
                .apply(ok_approver, None)
                .await
                .map_err(|e| e.to_string())?,
        ),
        &"applied",
        "workspace approver may cancel",
    )?;
    Ok(())
}

pub fn ledger_recovery_cases() -> BTreeMap<&'static str, LedgerCase> {
    let mut map: BTreeMap<&'static str, LedgerCase> = BTreeMap::new();
    map.insert("LEDGER-07", Arc::new(|a| Box::pin(renewal(a))));
    map.insert("LEDGER-08", Arc::new(|a| Box::pin(acknowledgement(a))));
    map.insert("LEDGER-09", Arc::new(|a| Box::pin(preparing_restart(a))));
    map.insert("LEDGER-10", Arc::new(|a| Box::pin(prepared_restart(a))));
    map.insert("LEDGER-11", Arc::new(|a| Box::pin(reconciliation(a))));
    map.insert("LEDGER-12", Arc::new(|a| Box::pin(expiry_and_fencing(a))));
    map
}
