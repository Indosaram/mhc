use super::{
    reservation_run_ledger::{ReservationRunLedger, parse_reservation_run_ledger},
    run_artifacts::{
        CHILD_EXIT_FILENAME, RunChildExit, RunOutcome, read_run_json, run_outcome_matches_ledger,
        write_run_json_atomic,
    },
    run_finalization_claim::{ClaimedRunResult, with_run_finalization_claim},
    run_finalization_git::resolve_finalization_decision,
    run_finalization_settlement::settle_reservation_run,
    run_finalization_types::{ReservationRunResult, RunFinalizationContext},
};
use std::path::Path;

const RECOVERED_DETAIL: &str =
    "recovered from the worktree tip after the supervisor died before publishing an outcome";

/// A supervisor that died after its child exited never published `outcome.json`. Recovery trusts the
/// child's tip only when the bootstrap durably recorded that this attempt's child exited 0, by itself,
/// before the hard deadline, and the tip passes the validation a successful child's tip passes.
fn recover_unpublished_worktree_tip(
    context: &RunFinalizationContext<'_>,
    run_dir: &Path,
    ledger: &ReservationRunLedger,
) -> Result<bool, String> {
    if run_dir.join("outcome.json").exists() {
        return Ok(false);
    }
    let Some(exit) = read_clean_child_exit(run_dir, ledger) else {
        return Ok(false);
    };
    let worktree = super::reservation_run_ledger::worktree_from_ledger(
        &context.identity.paths.repo,
        &context.identity.id,
        ledger,
    )
    .map_err(|error| error.to_string())?;
    if !worktree.dir.exists() {
        return Ok(false);
    }
    let base_sha = ledger.value()["baseSha"]
        .as_str()
        .ok_or("Missing baseSha")?;
    // The recovered tip is a completed run's branch: repair description overruns before the gate reads it.
    super::run_finalization_git::preflight_descriptions(&worktree, base_sha, context.warn);
    if !matches!(
        memory_core::reflection::validate_completion(&worktree, base_sha, worktree.exec.as_ref()),
        memory_core::reflection::CompletionValidation::Valid { .. }
    ) {
        return Ok(false);
    }
    let attempt = ledger.value()["attempt"]
        .as_u64()
        .map(|attempt| u32::try_from(attempt).map_err(|error| error.to_string()))
        .transpose()?;
    let outcome = RunOutcome {
        version: 1,
        run_id: ledger.run_id().into(),
        attempt,
        finished_at: exit.finished_at,
        child_exit: super::run_artifacts::ChildExit {
            code: exit.code,
            signal: exit.signal,
        },
        timed_out: false,
        recovered_from_worktree: Some(true),
    };
    write_run_json_atomic(&run_dir.join("outcome.json"), &outcome, 0o600)
        .map_err(|error| error.to_string())?;
    emit_recovered_receipt(context, ledger);
    Ok(true)
}
fn read_clean_child_exit(run_dir: &Path, ledger: &ReservationRunLedger) -> Option<RunChildExit> {
    let exit: RunChildExit = read_run_json(&run_dir.join(CHILD_EXIT_FILENAME)).ok()?;
    let value = ledger.value();
    let finished = memory_core::support::time::parse_rfc3339(&exit.finished_at)?;
    let started = memory_core::support::time::parse_rfc3339(value["startedAt"].as_str()?)?;
    let attempt = value["attempt"].as_u64()?;
    let clean = exit.run_id == ledger.run_id()
        && u64::from(exit.attempt) == attempt
        && exit.code == Some(0)
        && exit.signal.is_none()
        && !exit.timed_out
        && finished >= started
        && (finished as f64) < value["hardDeadlineAt"].as_f64()?;
    clean.then_some(exit)
}
fn emit_recovered_receipt(context: &RunFinalizationContext<'_>, ledger: &ReservationRunLedger) {
    let value = ledger.value();
    let run_ledger = crate::receipts_port::RunReceiptLedger {
        kind: value["kind"].as_str().unwrap_or("reflection").to_string(),
        run_id: ledger.run_id().to_string(),
        trigger: value["trigger"].as_str().unwrap_or("manual").to_string(),
        origin: value
            .get("origin")
            .and_then(|origin| origin.as_str())
            .map(str::to_string),
        started_at: value["startedAt"].as_str().unwrap_or("").to_string(),
    };
    let receipt = crate::receipts_port::run_receipt(
        &run_ledger,
        "recovered",
        crate::receipts_port::ReceiptExtra {
            sha: None,
            reason: None,
            detail: None,
        },
    );
    crate::receipts_port::emit_memory_receipt(
        &context.identity.paths.runtime,
        &receipt,
        context.receipts,
        context
            .warn
            .map(|warn| warn as &dyn Fn(&str, &std::collections::BTreeMap<String, String>)),
    );
}
fn with_recovery_marker(
    outcome: &RunOutcome,
    mut decision: super::run_finalization_types::DurableFinalizationDecision,
) -> super::run_finalization_types::DurableFinalizationDecision {
    if outcome.recovered_from_worktree == Some(true) {
        decision.recovered_from_worktree = true;
        decision.detail = Some(match decision.detail.take() {
            Some(detail) => format!("{detail}\n{RECOVERED_DETAIL}"),
            None => RECOVERED_DETAIL.to_string(),
        });
    }
    decision
}

pub fn fail_reservation_run(
    context: &RunFinalizationContext<'_>,
    run_dir: &Path,
    input: &ReservationRunLedger,
    timed_out: bool,
    detail: Option<String>,
    recover_worktree_tip: bool,
    gate: impl FnOnce(
        &mut dyn FnMut() -> Result<Option<ReservationRunResult>, String>,
    ) -> Result<Option<ReservationRunResult>, String>,
) -> Result<Option<ReservationRunResult>, String> {
    checkpoint_reservation_failure(
        context,
        run_dir,
        input,
        super::run_finalization_types::DurableFinalizationDecision {
            outcome: if timed_out { "timed_out" } else { "failed" }.into(),
            reason: Some(
                if timed_out {
                    "deadline_exceeded"
                } else {
                    "supervisor_failed"
                }
                .into(),
            ),
            detail,
            integration_sha: None,
            recovered_from_worktree: false,
        },
        false,
        recover_worktree_tip,
        gate,
    )
}
pub fn override_failed_reservation_run(
    context: &RunFinalizationContext<'_>,
    run_dir: &Path,
    input: &ReservationRunLedger,
    detail: String,
    gate: impl FnOnce(
        &mut dyn FnMut() -> Result<Option<ReservationRunResult>, String>,
    ) -> Result<Option<ReservationRunResult>, String>,
) -> Result<Option<ReservationRunResult>, String> {
    checkpoint_reservation_failure(
        context,
        run_dir,
        input,
        super::run_finalization_types::DurableFinalizationDecision {
            outcome: "failed".into(),
            reason: Some("spawn_failed".into()),
            detail: Some(detail),
            integration_sha: None,
            recovered_from_worktree: false,
        },
        true,
        false,
        gate,
    )
}
pub fn abandon_reservation_run(
    context: &RunFinalizationContext<'_>,
    run_dir: &Path,
    input: &ReservationRunLedger,
    mut classify: impl FnMut(
        Option<u64>,
        Option<Option<&str>>,
    ) -> super::run_liveness::RunProcessVerdict,
    gate: impl FnOnce(
        &mut dyn FnMut() -> Result<Option<ReservationRunResult>, String>,
    ) -> Result<Option<ReservationRunResult>, String>,
) -> Result<Option<ReservationRunResult>, String> {
    let claimed = with_run_finalization_claim(
        &context.identity.paths.locks,
        run_dir,
        input.run_id(),
        || {
            gate(&mut || {
                let active = context.reservation.read_state()?.active;
                let precedence = super::run_terminal_precedence::check_run_abandonment_precedence(
                    run_dir,
                    input.run_id(),
                    &mut classify,
                    |path, id, attempt, kind| {
                        super::run_terminal_claim::claim_run_terminal(
                            path,
                            id,
                            attempt,
                            kind,
                            std::process::id(),
                        )
                    },
                )
                .map_err(|error| format!("{error:?}"))?;
                match precedence.decision {
                    super::run_terminal_precedence::AbandonmentDecision::Veto => Ok(None),
                    super::run_terminal_precedence::AbandonmentDecision::Finalize => {
                        let child: RunOutcome = read_run_json(&run_dir.join("outcome.json"))
                            .map_err(|error| error.to_string())?;
                        let decision = with_recovery_marker(
                            &child,
                            resolve_finalization_decision(
                                context.identity,
                                run_dir,
                                &precedence.ledger,
                                &child,
                                context.warn,
                            )?,
                        );
                        settle_reservation_run(context, run_dir, &precedence.ledger, decision)
                            .map(Some)
                    }
                    super::run_terminal_precedence::AbandonmentDecision::Abandon => {
                        let abandoned = chrono::DateTime::from_timestamp_millis((context.now_ms)())
                            .ok_or("Invalid abandonment timestamp")?
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        let started_at = precedence.ledger.value()["startedAt"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        super::run_artifacts::write_run_json_atomic(&run_dir.join("abandoned.json"),&serde_json::json!({"version":1,"runId":precedence.ledger.run_id(),"outcome":"abandoned_unknown","abandonedAt":abandoned,"generation":started_at}),0o600).map_err(|error|error.to_string())?;
                        let run_ledger = crate::receipts_port::RunReceiptLedger {
                            kind: precedence.ledger.value()["kind"]
                                .as_str()
                                .unwrap_or("reflection")
                                .to_string(),
                            run_id: precedence.ledger.run_id().to_string(),
                            trigger: precedence.ledger.value()["trigger"]
                                .as_str()
                                .unwrap_or("manual")
                                .to_string(),
                            origin: precedence
                                .ledger
                                .value()
                                .get("origin")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            started_at: started_at.clone(),
                        };
                        let receipt_input = crate::receipts_port::run_receipt(
                            &run_ledger,
                            "abandoned",
                            crate::receipts_port::ReceiptExtra {
                                sha: None,
                                reason: Some("abandoned_unknown".to_string()),
                                detail: None,
                            },
                        );
                        crate::receipts_port::emit_memory_receipt(
                            &context.identity.paths.runtime,
                            &receipt_input,
                            context.receipts,
                            context.warn.map(|w| {
                                w as &dyn Fn(&str, &std::collections::BTreeMap<String, String>)
                            }),
                        );
                        if active
                            .as_ref()
                            .is_some_and(|run| run.run_id == precedence.ledger.run_id())
                        {
                            let transition = context.reservation.complete(
                                precedence.ledger.run_id(),
                                memory_core::reflection::ReflectionOutcome::Failed,
                            )?;
                            if let (Some(run), Some(launch)) = (&transition.launch, context.launch)
                            {
                                launch(run);
                            }
                        }
                        Ok(Some(ReservationRunResult {
                            run_id: precedence.ledger.run_id().into(),
                            outcome: "abandoned_unknown".into(),
                            reason: None,
                            detail: None,
                            completion: None,
                            launch: None,
                        }))
                    }
                }
            })
        },
    )?;
    Ok(match claimed {
        ClaimedRunResult::Completed(result) => result,
        ClaimedRunResult::Terminal(result) => Some(ReservationRunResult {
            run_id: result.run_id,
            outcome: result.outcome,
            reason: None,
            detail: None,
            completion: None,
            launch: None,
        }),
        ClaimedRunResult::Busy => None,
    })
}
fn checkpoint_reservation_failure(
    context: &RunFinalizationContext<'_>,
    run_dir: &Path,
    input: &ReservationRunLedger,
    decision: super::run_finalization_types::DurableFinalizationDecision,
    override_outcome: bool,
    recover_worktree_tip: bool,
    gate: impl FnOnce(
        &mut dyn FnMut() -> Result<Option<ReservationRunResult>, String>,
    ) -> Result<Option<ReservationRunResult>, String>,
) -> Result<Option<ReservationRunResult>, String> {
    let claimed = with_run_finalization_claim(
        &context.identity.paths.locks,
        run_dir,
        input.run_id(),
        || {
            gate(&mut || {
                let ledger = parse_reservation_run_ledger(
                    read_run_json(&run_dir.join("ledger.json"))
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                if ledger.run_id() != input.run_id() {
                    return Err(format!("Finalization ledger mismatch: {}", input.run_id()));
                }
                let attempt = ledger.value()["attempt"]
                    .as_u64()
                    .map(|attempt| u32::try_from(attempt).map_err(|error| error.to_string()))
                    .transpose()?;
                if !override_outcome && run_dir.join("outcome.json").exists() {
                    let child: RunOutcome = read_run_json(&run_dir.join("outcome.json"))
                        .map_err(|error| error.to_string())?;
                    if run_outcome_matches_ledger(attempt, &child) {
                        let decision = with_recovery_marker(
                            &child,
                            resolve_finalization_decision(
                                context.identity,
                                run_dir,
                                &ledger,
                                &child,
                                context.warn,
                            )?,
                        );
                        return settle_reservation_run(context, run_dir, &ledger, decision)
                            .map(Some);
                    }
                }
                if !override_outcome
                    && recover_worktree_tip
                    && recover_unpublished_worktree_tip(context, run_dir, &ledger)?
                {
                    let child: RunOutcome = read_run_json(&run_dir.join("outcome.json"))
                        .map_err(|error| error.to_string())?;
                    let decision = with_recovery_marker(
                        &child,
                        resolve_finalization_decision(
                            context.identity,
                            run_dir,
                            &ledger,
                            &child,
                            context.warn,
                        )?,
                    );
                    return settle_reservation_run(context, run_dir, &ledger, decision).map(Some);
                }
                let mut fields = serde_json::Map::from_iter([(
                    "finalizeOutcome".into(),
                    decision.outcome.clone().into(),
                )]);
                if let Some(reason) = &decision.reason {
                    fields.insert("finalizeReason".into(), reason.clone().into());
                }
                if let Some(detail) = &decision.detail {
                    fields.insert("finalizeDetail".into(), detail.clone().into());
                }
                super::run_artifacts::update_run_ledger(&run_dir.join("ledger.json"), &fields)
                    .map_err(|error| error.to_string())?;
                let worktree = super::reservation_run_ledger::worktree_from_ledger(
                    &context.identity.paths.repo,
                    &context.identity.id,
                    &ledger,
                )
                .map_err(|error| error.to_string())?;
                super::run_finalization_git::cleanup_and_record(&worktree, run_dir)?;
                let result = super::run_finalization_types::DurableFinalizationDecision {
                    outcome: decision.outcome.clone(),
                    reason: decision.reason.clone(),
                    detail: decision.detail.clone(),
                    integration_sha: None,
                    recovered_from_worktree: false,
                };
                settle_reservation_run(context, run_dir, &ledger, result).map(Some)
            })
        },
    )?;
    Ok(match claimed {
        ClaimedRunResult::Completed(result) => result,
        ClaimedRunResult::Terminal(result) => Some(ReservationRunResult {
            run_id: result.run_id,
            outcome: result.outcome,
            reason: None,
            detail: None,
            completion: None,
            launch: None,
        }),
        ClaimedRunResult::Busy => None,
    })
}

pub fn finalize_recorded_outcome(
    context: &RunFinalizationContext<'_>,
    run_dir: &Path,
    input: &ReservationRunLedger,
) -> Result<Option<ReservationRunResult>, String> {
    let claimed = with_run_finalization_claim(
        &context.identity.paths.locks,
        run_dir,
        input.run_id(),
        || {
            let ledger = parse_reservation_run_ledger(
                read_run_json(&run_dir.join("ledger.json")).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            if ledger.run_id() != input.run_id() {
                return Err(format!("Finalization ledger mismatch: {}", input.run_id()));
            }
            let outcome: RunOutcome =
                read_run_json(&run_dir.join("outcome.json")).map_err(|error| error.to_string())?;
            let attempt = ledger.value()["attempt"]
                .as_u64()
                .map(|value| u32::try_from(value).map_err(|error| error.to_string()))
                .transpose()?;
            if !run_outcome_matches_ledger(attempt, &outcome) {
                return Err(format!(
                    "Run outcome attempt {} does not match {}",
                    outcome
                        .attempt
                        .map_or("legacy".into(), |value| value.to_string()),
                    attempt.map_or("legacy".into(), |value| value.to_string())
                ));
            }
            let decision = with_recovery_marker(
                &outcome,
                resolve_finalization_decision(
                    context.identity,
                    run_dir,
                    &ledger,
                    &outcome,
                    context.warn,
                )?,
            );
            settle_reservation_run(context, run_dir, &ledger, decision)
        },
    )?;
    Ok(match claimed {
        ClaimedRunResult::Completed(result) => Some(result),
        ClaimedRunResult::Terminal(result) => Some(ReservationRunResult {
            run_id: result.run_id,
            outcome: result.outcome,
            reason: None,
            detail: None,
            completion: None,
            launch: None,
        }),
        ClaimedRunResult::Busy => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Inactive;
    impl super::super::runner_types::ReflectionReservationPort for Inactive {
        fn read_state(&self) -> Result<memory_core::reflection::ReservationState, String> {
            Ok(Default::default())
        }
        fn complete(
            &self,
            _: &str,
            _: memory_core::reflection::ReflectionOutcome,
        ) -> Result<memory_core::reflection::CompletionResult, String> {
            panic!("inactive reservation")
        }
    }
    #[test]
    fn failure_checkpoints_and_cleans_before_terminal_settlement() {
        for timed_out in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let identity = memory_core::identity::resolve::MemoryIdentity {
                id: "agent".into(),
                safe_slug: "agent".into(),
                paths: memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
            };
            let engine = crate::engine_session::prepare_memory_engine_session(
                "agent",
                &identity.paths,
                Default::default(),
            )
            .unwrap();
            let exec = memory_core::git::exec::create_git_exec(Default::default());
            let worktree = memory_core::reflection::create_reflection_worktree(
                &engine.repo,
                "run",
                &identity.paths.worktrees,
                exec.as_ref(),
                None,
            )
            .unwrap();
            let dir = root.path().join("run");
            std::fs::create_dir_all(&dir).unwrap();
            let value = serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","category":"quick","conversationIds":["conversation"],"startedAt":"2026-08-16T00:00:00.000Z","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":worktree.dir,"worktreeBranch":worktree.branch,"baseSha":worktree.base_commit_sha,"gitFilePath":worktree.git_file_path,"gitFileSnapshot":worktree.git_file_snapshot,"commonConfigPath":worktree.common_config_path,"commonConfigSnapshot":worktree.common_config_snapshot});
            super::super::run_artifacts::write_run_json_atomic(
                &dir.join("ledger.json"),
                &value,
                0o600,
            )
            .unwrap();
            let ledger = parse_reservation_run_ledger(value).unwrap();
            let context = RunFinalizationContext {
                identity: &identity,
                reservation: &Inactive,
                launch: None,
                now_ms: &|| 1786838400000,
                receipts: None,
                warn: None,
            };
            let result = fail_reservation_run(
                &context,
                &dir,
                &ledger,
                timed_out,
                Some("supervisor detail".into()),
                false,
                |operation| operation(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                result.outcome,
                if timed_out { "timed_out" } else { "failed" }
            );
            assert_eq!(
                result.reason.as_deref(),
                Some(if timed_out {
                    "deadline_exceeded"
                } else {
                    "supervisor_failed"
                })
            );
            assert!(!worktree.dir.exists());
            assert!(dir.join("final.json").exists());
            assert!(result.completion.is_some());
            let checkpoint: serde_json::Value = read_run_json(&dir.join("ledger.json")).unwrap();
            assert_eq!(checkpoint["finalizePhase"], "settled");
        }
    }
    #[test]
    fn terminal_sentinel_returns_without_reservation_or_git_work() {
        struct Reservation;
        impl super::super::runner_types::ReflectionReservationPort for Reservation {
            fn read_state(&self) -> Result<memory_core::reflection::ReservationState, String> {
                panic!("terminal reservation must not be read")
            }
            fn complete(
                &self,
                _: &str,
                _: memory_core::reflection::ReflectionOutcome,
            ) -> Result<memory_core::reflection::CompletionResult, String> {
                panic!("terminal reservation must not be completed")
            }
        }
        let root = tempfile::tempdir().unwrap();
        let identity = memory_core::identity::resolve::MemoryIdentity {
            id: "agent".into(),
            safe_slug: "agent".into(),
            paths: memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
        };
        let ledger = parse_reservation_run_ledger(serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"/unused","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"/unused/.git","gitFileSnapshot":"gitdir: unused","commonConfigPath":"/unused/config","commonConfigSnapshot":null})).unwrap();
        super::super::run_artifacts::write_run_json_atomic(
            &root.path().join("final.json"),
            &serde_json::json!({"runId":"run","outcome":"merged"}),
            0o600,
        )
        .unwrap();
        let result = finalize_recorded_outcome(
            &RunFinalizationContext {
                identity: &identity,
                reservation: &Reservation,
                launch: None,
                now_ms: &|| 0,
                receipts: None,
                warn: None,
            },
            root.path(),
            &ledger,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.outcome, "merged");
        assert!(result.completion.is_none());
    }
}
