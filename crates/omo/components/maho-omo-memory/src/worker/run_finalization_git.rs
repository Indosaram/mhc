pub type WarnCallback<'a> = Option<&'a (dyn Fn(&str, &std::collections::BTreeMap<String, String>) + Send + Sync)>;
use super::{
    reservation_run_ledger::{
        ReservationRunLedger, parse_reservation_run_ledger, worktree_from_ledger,
    },
    run_artifacts::{RunOutcome, read_run_json, read_run_text_tail, update_run_ledger},
    run_finalization_types::DurableFinalizationDecision,
};
use memory_core::{
    identity::resolve::MemoryIdentity,
    reflection::{
        CompletionValidation, IntegrateValidatedReflectionInput, LegacyAutoRunReceiptProbe,
        ReflectionIntegrationMode, ReflectionIntegrationProbe, ValidatedReflectionTip,
        cleanup_reflection_worktree, describe_dropped_entries, integrate_validated_reflection,
        preflight_reflection_descriptions, probe_legacy_auto_run_receipt,
        probe_reflection_integration, read_dropped_trailers, validate_completion,
    },
};
use std::path::Path;

fn checkpoint(path: &Path, decision: &DurableFinalizationDecision) -> Result<(), String> {
    let mut fields = serde_json::Map::new();
    fields.insert("finalizeOutcome".into(), decision.outcome.clone().into());
    if decision.outcome == "merged" {
        fields.insert("finalizePhase".into(), "integrated".into());
        if let Some(sha) = &decision.integration_sha {
            fields.insert("integrationSha".into(), sha.clone().into());
        }
    }
    if let Some(reason) = &decision.reason {
        fields.insert("finalizeReason".into(), reason.clone().into());
    }
    if let Some(detail) = &decision.detail {
        fields.insert("finalizeDetail".into(), detail.clone().into());
    }
    update_run_ledger(path, &fields).map_err(|error| error.to_string())
}

/// Cleanup is bookkeeping that runs after the decision is durable: a leftover worktree or branch
/// settles the run on its real outcome, the ledger records the leftover, and the orphan sweep reclaims it.
pub fn cleanup_and_record(
    worktree: &memory_core::reflection::worktree::ReflectionWorktree,
    run_dir: &Path,
) -> Result<(), String> {
    let receipt = cleanup_reflection_worktree(worktree);
    if receipt.worktree_removed && receipt.branch_removed {
        return Ok(());
    }
    update_run_ledger(
        &run_dir.join("ledger.json"),
        &serde_json::Map::from_iter([("cleanupIncomplete".into(), true.into())]),
    )
    .map_err(|error| error.to_string())
}

fn validate_integrated_dream_budget(
    identity: &MemoryIdentity,
    ledger: &ReservationRunLedger,
    decision: &mut DurableFinalizationDecision,
) -> Result<(), String> {
    let value = ledger.value();
    let (Some("dream"), Some(origin), Some(target)) = (
        value["kind"].as_str(),
        value.get("origin").filter(|origin| origin.is_string()),
        value["systemTokenTarget"].as_u64(),
    ) else {
        return Ok(());
    };
    let origin: memory_core::reflection::DreamOrigin =
        serde_json::from_value(origin.clone()).map_err(|error| error.to_string())?;
    let target = usize::try_from(target).map_err(|error| error.to_string())?;
    let estimate = crate::commands::tokens::estimate_system_tokens(&identity.paths.repo);
    match memory_core::reflection::completion_validation::validate_dream_token_budget(origin, estimate.total_tokens, target) {
        memory_core::reflection::completion_validation::DreamTokenBudgetValidation::BudgetNotMet { detail, .. } => { decision.reason = Some("budget_not_met".into()); decision.detail = Some(detail); }
        memory_core::reflection::completion_validation::DreamTokenBudgetValidation::Valid { total_tokens, target_tokens } => { decision.detail = Some(format!("Committed system/ estimate is {total_tokens} tokens; dream target is below {target_tokens} tokens")); }
    }
    Ok(())
}

fn decision(
    outcome: &str,
    reason: Option<&str>,
    detail: Option<String>,
    integration_sha: Option<String>,
) -> DurableFinalizationDecision {
    DurableFinalizationDecision {
        outcome: outcome.into(),
        reason: reason.map(str::to_owned),
        detail,
        integration_sha,
        recovered_from_worktree: false,
    }
}

pub fn resolve_finalization_decision(
    identity: &MemoryIdentity,
    run_dir: &Path,
    input: &ReservationRunLedger,
    child: &RunOutcome,
    warn: WarnCallback<'_>,
) -> Result<DurableFinalizationDecision, String> {
    let path = run_dir.join("ledger.json");
    let mut ledger =
        parse_reservation_run_ledger(read_run_json(&path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if ledger.run_id() != input.run_id() || child.run_id != ledger.run_id() {
        return Err("Finalization run id changed".into());
    }
    let worktree = worktree_from_ledger(&identity.paths.repo, &identity.id, &ledger)
        .map_err(|error| error.to_string())?;
    let cleanup = || cleanup_and_record(&worktree, run_dir);
    if child.timed_out || child.child_exit.code != Some(0) || child.child_exit.signal.is_some() {
        let detail = read_run_text_tail(&run_dir.join("child-stderr.log"), 64 * 1024)
            .map_err(|error| error.to_string())?;
        let result = decision(
            if child.timed_out {
                "timed_out"
            } else {
                "failed"
            },
            Some(if child.timed_out {
                "deadline_exceeded"
            } else {
                "child_exit"
            }),
            (!detail.trim().is_empty()).then(|| detail.trim().to_owned()),
            None,
        );
        checkpoint(&path, &result)?;
        cleanup()?;
        return Ok(result);
    }
    if let Some(outcome) = ledger.value()["finalizeOutcome"].as_str() {
        let result = decision(
            outcome,
            ledger.value()["finalizeReason"].as_str(),
            ledger.value()["finalizeDetail"].as_str().map(str::to_owned),
            ledger.value()["integrationSha"].as_str().map(str::to_owned),
        );
        cleanup()?;
        return Ok(result);
    }
    let mut validated =
        ledger.value()["validatedTipSha"]
            .as_str()
            .map(|sha| ValidatedReflectionTip {
                tip_sha: sha.into(),
                changed_paths: ledger.value()["validatedChangedPaths"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            });
    if validated.is_none() && worktree.dir.exists() {
        let base_sha = ledger.value()["baseSha"]
            .as_str()
            .ok_or("Missing baseSha")?;
        // A run that only overran the description cap is repaired here, before the pure gate runs.
        preflight_descriptions(&worktree, base_sha, warn);
        match validate_completion(&worktree, base_sha, worktree.exec.as_ref()) {
            CompletionValidation::Valid {
                tip_sha,
                changed_paths,
                ..
            } => {
                if ledger.value()["targetDoc"]
                    .as_str()
                    .is_some_and(|target| changed_paths.iter().any(|path| path != target))
                {
                    let result = decision(
                        "failed",
                        Some("invalid_target"),
                        Some("Dream changed paths outside targetDoc".into()),
                        None,
                    );
                    checkpoint(&path, &result)?;
                    cleanup()?;
                    return Ok(result);
                }
                update_run_ledger(&path, serde_json::json!({"finalizePhase":"validated","validatedTipSha":tip_sha,"validatedChangedPaths":changed_paths}).as_object().ok_or("Invalid validation checkpoint")?).map_err(|error| error.to_string())?;
                validated = Some(ValidatedReflectionTip {
                    tip_sha,
                    changed_paths,
                });
            }
            CompletionValidation::NoChanges {
                tip_sha, dropped, ..
            } => {
                // Every entry the run wrote was refused: say so instead of reading as "nothing to do".
                let note = (!dropped.is_empty()).then(|| describe_dropped_entries(&dropped));
                let mut checkpoint_fields = serde_json::json!({"finalizePhase":"validated","validatedTipSha":tip_sha,"validatedChangedPaths":[],"finalizeOutcome":"no_changes"});
                if let Some(note) = &note
                    && let Some(fields) = checkpoint_fields.as_object_mut()
                {
                    fields.insert("finalizeDetail".into(), note.clone().into());
                }
                update_run_ledger(
                    &path,
                    checkpoint_fields
                        .as_object()
                        .ok_or("Invalid validation checkpoint")?,
                )
                .map_err(|error| error.to_string())?;
                cleanup()?;
                return Ok(decision("no_changes", None, note, None));
            }
            CompletionValidation::DirtyUncommitted { detail } => {
                let result = decision(
                    "dirty_uncommitted",
                    Some("completion_validation"),
                    Some(detail),
                    None,
                );
                checkpoint(&path, &result)?;
                cleanup()?;
                return Ok(result);
            }
            CompletionValidation::Failed { detail } => {
                let result = decision("failed", Some("completion_validation"), Some(detail), None);
                checkpoint(&path, &result)?;
                cleanup()?;
                return Ok(result);
            }
        }
        ledger =
            parse_reservation_run_ledger(read_run_json(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
    }
    let Some(validated) = validated else {
        if ledger.value()["mergePolicy"] == "auto"
            && let LegacyAutoRunReceiptProbe::AlreadyRecorded { integration_sha } =
                probe_legacy_auto_run_receipt(&worktree, ledger.run_id())
        {
            let result = decision("merged", None, None, integration_sha);
            checkpoint(&path, &result)?;
            cleanup()?;
            return Ok(result);
        }
        let result = decision("failed", Some("missing_validated_tip"), None, None);
        checkpoint(&path, &result)?;
        cleanup()?;
        return Ok(result);
    };
    let result = if Some(validated.tip_sha.as_str()) == ledger.value()["baseSha"].as_str() {
        decision("no_changes", None, None, None)
    } else {
        let mode = if ledger.value()["mergePolicy"] == "auto" {
            ReflectionIntegrationMode::Auto
        } else {
            ReflectionIntegrationMode::Integration
        };
        match probe_reflection_integration(&worktree, mode, ledger.run_id(), &validated.tip_sha) {
            ReflectionIntegrationProbe::AlreadyMerged { integration_sha } => {
                decision("merged", None, None, Some(integration_sha))
            }
            ReflectionIntegrationProbe::NotIntegrated if !worktree.dir.exists() => {
                let result = decision("failed", Some("missing_worktree"), None, None);
                checkpoint(&path, &result)?;
                return Ok(result);
            }
            ReflectionIntegrationProbe::NotIntegrated => {
                // Read from the validated commit's trailers, so a run resumed after the validated checkpoint still
                // reports what the gate dropped before it merged the rest.
                let dropped_note = read_dropped_trailers(
                    worktree.exec.as_ref(),
                    &worktree.dir,
                    ledger.value()["baseSha"]
                        .as_str()
                        .ok_or("Missing baseSha")?,
                    &validated.tip_sha,
                )
                .ok()
                .filter(|entries| !entries.is_empty())
                .map(|entries| describe_dropped_entries(&entries));
                let engine = crate::engine_session::prepare_memory_engine_session(
                    &identity.id,
                    &identity.paths,
                    Default::default(),
                )
                .map_err(|error| error.to_string())?;
                let integrated = integrate_validated_reflection(
                    &worktree,
                    IntegrateValidatedReflectionInput {
                        mode,
                        run_id: ledger.run_id().into(),
                        summary: format!(
                            "{} {}",
                            ledger.value()["trigger"]
                                .as_str()
                                .ok_or("Missing trigger")?,
                            ledger.run_id()
                        ),
                        validated,
                    },
                    |operation| {
                        engine
                            .lock
                            .run("memory-write", operation)
                            .map_err(|error| error.to_string())
                    },
                );
                let outcome =
                    serde_json::to_value(integrated.outcome).map_err(|error| error.to_string())?;
                let detail = match (integrated.detail, dropped_note) {
                    (Some(detail), Some(note)) => Some(format!("{detail}; {note}")),
                    (detail, note) => detail.or(note),
                };
                let mut merged = decision(
                    outcome.as_str().ok_or("Invalid integration outcome")?,
                    None,
                    detail,
                    integrated.integration_sha,
                );
                if merged.outcome == "merged" {
                    validate_integrated_dream_budget(identity, &ledger, &mut merged)?;
                }
                merged
            }
        }
    };
    checkpoint(&path, &result)?;
    cleanup()?;
    Ok(result)
}

pub(super) fn preflight_descriptions(
    worktree: &memory_core::reflection::ReflectionWorktree,
    base: &str,
    warn: WarnCallback<'_>,
) {
    let adapter = |message: &str, fields: &serde_json::Value| {
        if let Some(warn) = warn {
            let fields = fields
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string()),
                    )
                })
                .collect();
            warn(message, &fields);
        }
    };
    preflight_reflection_descriptions(worktree, base, worktree.exec.as_ref(), Some(&adapter));
}

#[cfg(test)]
mod tests {
    use super::super::run_artifacts::{ChildExit, write_run_json_atomic};
    use super::*;
    fn fixture(
        root: &Path,
    ) -> (
        MemoryIdentity,
        ReservationRunLedger,
        RunOutcome,
        memory_core::reflection::ReflectionWorktree,
    ) {
        let identity = MemoryIdentity {
            id: "agent".into(),
            safe_slug: "agent".into(),
            paths: memory_core::identity::layout::build_identity_paths(root, "agent"),
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
            &root.join("worktrees"),
            exec.as_ref(),
            None,
        )
        .unwrap();
        let value = serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":worktree.dir,"worktreeBranch":worktree.branch,"baseSha":worktree.base_commit_sha,"gitFilePath":worktree.git_file_path,"gitFileSnapshot":worktree.git_file_snapshot,"commonConfigPath":worktree.common_config_path,"commonConfigSnapshot":worktree.common_config_snapshot});
        let dir = root.join("run");
        std::fs::create_dir_all(&dir).unwrap();
        write_run_json_atomic(&dir.join("ledger.json"), &value, 0o600).unwrap();
        (
            identity,
            parse_reservation_run_ledger(value).unwrap(),
            RunOutcome {
                version: 1,
                run_id: "run".into(),
                attempt: None,
                finished_at: "now".into(),
                child_exit: ChildExit {
                    code: Some(0),
                    signal: None,
                },
                timed_out: false,
                recovered_from_worktree: None,
            },
            worktree,
        )
    }
    #[test]
    fn unchanged_tip_is_checkpointed_before_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let (identity, ledger, child, worktree) = fixture(root.path());
        let result = resolve_finalization_decision(
            &identity,
            &root.path().join("run"),
            &ledger,
            &child,
            None,
        )
        .unwrap();
        assert_eq!(result.outcome, "no_changes");
        assert!(!worktree.dir.exists());
        let checkpoint: serde_json::Value =
            read_run_json(&root.path().join("run/ledger.json")).unwrap();
        assert_eq!(checkpoint["finalizePhase"], "validated");
        assert_eq!(checkpoint["finalizeOutcome"], "no_changes");
        assert_eq!(
            resolve_finalization_decision(
                &identity,
                &root.path().join("run"),
                &ledger,
                &child,
                None
            )
            .unwrap()
            .outcome,
            "no_changes"
        );
    }
    #[test]
    fn child_failure_records_stderr_and_cleans_worktree() {
        let root = tempfile::tempdir().unwrap();
        let (identity, ledger, mut child, worktree) = fixture(root.path());
        child.child_exit.code = Some(2);
        std::fs::write(
            root.path().join("run/child-stderr.log"),
            " failure detail\n",
        )
        .unwrap();
        let result = resolve_finalization_decision(
            &identity,
            &root.path().join("run"),
            &ledger,
            &child,
            None,
        )
        .unwrap();
        assert_eq!(result.outcome, "failed");
        assert_eq!(result.reason.as_deref(), Some("child_exit"));
        assert_eq!(result.detail.as_deref(), Some("failure detail"));
        assert!(!worktree.dir.exists());
    }
    #[test]
    fn changed_run_id_rejects_without_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let (identity, ledger, mut child, worktree) = fixture(root.path());
        child.run_id = "other".into();
        assert_eq!(
            resolve_finalization_decision(
                &identity,
                &root.path().join("run"),
                &ledger,
                &child,
                None
            )
            .err()
            .as_deref(),
            Some("Finalization run id changed")
        );
        assert!(worktree.dir.exists());
    }
    #[test]
    fn committed_reflection_is_integrated_under_writer_lock() {
        let root = tempfile::tempdir().unwrap();
        let (identity, ledger, child, worktree) = fixture(root.path());
        std::fs::write(
            worktree.dir.join("reference-fact.md"),
            "---\ndescription: Fact\n---\nNew fact\n",
        )
        .unwrap();
        for args in [
            vec!["add", "reference-fact.md"],
            vec![
                "-c",
                "user.name=agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-m",
                "Record reflection fact",
            ],
        ] {
            let output = worktree.exec.run_in(&worktree.dir, &args).unwrap();
            assert_eq!(output.code, 0, "{}", output.stderr);
        }
        let result = resolve_finalization_decision(
            &identity,
            &root.path().join("run"),
            &ledger,
            &child,
            None,
        )
        .unwrap();
        assert_eq!(result.outcome, "merged");
        assert!(result.integration_sha.is_some());
        assert!(identity.paths.repo.join("reference-fact.md").exists());
        assert!(!worktree.dir.exists());
        assert!(!memory_core::locks::memory_writer_lock_path(&identity.paths.locks).exists());
        let checkpoint: serde_json::Value =
            read_run_json(&root.path().join("run/ledger.json")).unwrap();
        assert_eq!(checkpoint["finalizePhase"], "integrated");
        assert_eq!(
            checkpoint["validatedChangedPaths"],
            serde_json::json!(["reference-fact.md"])
        );
    }
}
