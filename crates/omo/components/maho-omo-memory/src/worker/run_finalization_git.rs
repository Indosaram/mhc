pub type WarnCallback<'a> =
    Option<&'a (dyn Fn(&str, &std::collections::BTreeMap<String, String>) + Send + Sync)>;
use super::{
    reservation_run_ledger::{
        ReservationRunLedger, parse_reservation_run_ledger, worktree_from_ledger,
    },
    run_artifacts::{RunOutcome, read_run_json, read_run_text_tail, update_run_ledger},
    run_finalization_types::DurableFinalizationDecision,
};
use memory_core::git::exec::GitExecOptions;
use memory_core::receipts::owner::{SuppressionSet, TipScrub, markdown_content_lines};
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
use std::collections::{BTreeMap, BTreeSet};
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

/// Ledger key: the paths the suppression scrub put back to their base version, so the run's other
/// edits to them never vanish silently (surfaced in the finalize detail).
const REVERTED_PATHS_KEY: &str = "suppressionRevertedPaths";

fn git_in(
    worktree: &memory_core::reflection::ReflectionWorktree,
    args: &[&str],
) -> Result<String, String> {
    let output = worktree
        .exec
        .run_in(&worktree.dir, args)
        .map_err(|error| error.to_string())?;
    if output.code == 0 {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            output.stderr.trim()
        ))
    }
}

fn exists_in(
    worktree: &memory_core::reflection::ReflectionWorktree,
    revision: &str,
    relative: &str,
) -> Result<bool, String> {
    Ok(worktree
        .exec
        .run_in(
            &worktree.dir,
            &["cat-file", "-e", format!("{revision}:{relative}").as_str()],
        )
        .map_err(|error| error.to_string())?
        .code
        == 0)
}

fn owner_path(relative: &str) -> bool {
    // Case-insensitive: on a case-insensitive checkout `System/Owner/x.md` is the owner file.
    let dir = crate::owner_memory::OWNER_MEMORY_DIR;
    relative
        .get(..dir.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(dir))
        && relative[dir.len()..].starts_with('/')
}

/// Puts `text` in the index as `relative` and lets git write the file. Git never writes through a
/// symlinked ancestor (it replaces the symlink with a directory), so a run that committed a
/// symlink cannot redirect the scrub outside the worktree. The mode is `100755` when `relative` is
/// executable at `mode_from` (the version the text derives from), else `100644`.
fn stage_text(
    worktree: &memory_core::reflection::ReflectionWorktree,
    relative: &str,
    text: &str,
    mode_from: &str,
) -> Result<(), String> {
    let entry = git_in(
        worktree,
        &["--literal-pathspecs", "ls-tree", mode_from, "--", relative],
    )?;
    let mode = if entry.starts_with("100755 ") {
        "100755"
    } else {
        "100644"
    };
    let argv: Vec<String> = ["hash-object", "-w", "--stdin"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let options = GitExecOptions {
        cwd: worktree.dir.clone(),
        timeout_ms: 30_000,
        stdin: Some(text.as_bytes().to_vec()),
        ..GitExecOptions::default()
    };
    let output = memory_core::git::exec::GitExec::run(worktree.exec.as_ref(), &argv, &options)
        .map_err(|error| error.to_string())?;
    if output.code != 0 {
        return Err(format!(
            "git hash-object failed for {relative}: {}",
            output.stderr.trim()
        ));
    }
    git_in(
        worktree,
        &[
            "update-index",
            "--add",
            "--replace",
            "--cacheinfo",
            mode,
            output.stdout.trim(),
            relative,
        ],
    )?;
    git_in(
        worktree,
        &["--literal-pathspecs", "checkout", "--", relative],
    )?;
    Ok(())
}

/// What the suppression scrub reads from the run. `base_sha` and the validated tip never change,
/// so it is read before the memory-write lock is taken; only the suppression store is read inside.
struct TipSnapshot {
    /// The run's version of each changed markdown file outside `system/owner/` (those always go
    /// back to their base); `None`: deleted at the tip.
    tips: BTreeMap<String, Option<String>>,
    /// Every markdown file at `base_sha`.
    base_files: BTreeMap<String, String>,
    /// All the content of the tree at `base_sha` (`markdown_content_lines`).
    existing: BTreeSet<String>,
    /// The existing lines and descriptions the run removed from the whole tree.
    lost: BTreeSet<String>,
}

impl TipSnapshot {
    fn read(
        worktree: &memory_core::reflection::ReflectionWorktree,
        base_sha: &str,
        validated: &ValidatedReflectionTip,
    ) -> Result<Self, String> {
        let mut tips: BTreeMap<String, Option<String>> = BTreeMap::new();
        for relative in validated
            .changed_paths
            .iter()
            .filter(|path| path.ends_with(".md") && !owner_path(path))
        {
            let tip = if exists_in(worktree, &validated.tip_sha, relative)? {
                Some(git_in(
                    worktree,
                    &["show", format!("{}:{relative}", validated.tip_sha).as_str()],
                )?)
            } else {
                None
            };
            tips.insert(relative.clone(), tip);
        }
        let mut base_files: BTreeMap<String, String> = BTreeMap::new();
        if !tips.is_empty() {
            for relative in git_in(worktree, &["ls-tree", "-r", "--name-only", "-z", base_sha])?
                .split('\0')
                .filter(|path| path.ends_with(".md"))
            {
                base_files.insert(
                    relative.to_owned(),
                    git_in(
                        worktree,
                        &["show", format!("{base_sha}:{relative}").as_str()],
                    )?,
                );
            }
        }
        let existing = markdown_content_lines(base_files.values().map(String::as_str));
        let at_tip = markdown_content_lines(
            base_files
                .iter()
                .filter(|(path, _)| !tips.contains_key(*path))
                .map(|(_, content)| content.as_str())
                .chain(tips.values().flatten().map(String::as_str)),
        );
        let lost = markdown_content_lines(
            tips.keys()
                .filter_map(|path| base_files.get(path))
                .map(String::as_str),
        )
        .into_iter()
        .filter(|line| !at_tip.contains(line))
        .collect();
        Ok(Self {
            tips,
            base_files,
            existing,
            lost,
        })
    }
}

/// Reflection and dream output honor owner suppression (`memory_core::receipts::owner`): when the
/// validated tip's changed markdown restates forgotten or corrected content, the run branch is
/// reset to that tip, the content is dropped in one more commit and the checkpoint moves to it
/// before integration (`SuppressionSet::scrub_tip` decides per file). Existing memory is all the
/// markdown content anywhere in the tree at `base_sha`, so a line the run only moved is never
/// treated as new, and existing memory is never deleted on containment: when the run removed an
/// existing line that contains a suppressed record without restating it exactly ("Never use X"
/// for a forgotten "Use X"), every changed file whose base held that line goes back to its base
/// (without exact restatements), wherever the run moved or reworded it. A `system/owner/` change
/// (another receipt's file) is always put back to its base, suppression or not. The reverted paths
/// are returned (and kept in the ledger across resumes). The caller holds the memory-write lock
/// through integration (the lock every forget and correct holds; `inside_writer_lock` is a test
/// seam that runs first thing here), and the scrub is recomputed from the recorded tip on every
/// resume, so neither a forget landing mid-finalize nor a crash between the scrub commit and the
/// checkpoint can integrate the unscrubbed tip. `snapshot` is read before the lock when the store
/// already held a suppression, else here. An unreadable suppression store is an error: nothing
/// integrates.
fn scrub_suppressed_tip(
    identity: &MemoryIdentity,
    worktree: &memory_core::reflection::ReflectionWorktree,
    ledger_path: &Path,
    base_sha: &str,
    validated: ValidatedReflectionTip,
    snapshot: Option<TipSnapshot>,
    inside_writer_lock: &mut dyn FnMut(),
) -> Result<(ValidatedReflectionTip, Vec<String>), String> {
    inside_writer_lock();
    let set = SuppressionSet::load(&identity.paths.runtime)?;
    // Without the worktree nothing can integrate the tip (only an already-merged probe proceeds).
    if !worktree.dir.exists() {
        return Ok((validated, Vec::new()));
    }
    // The version each path is put back to; `None`: its base, byte for byte (absent there: removed).
    let mut fixes: BTreeMap<String, Option<String>> = validated
        .changed_paths
        .iter()
        .filter(|path| owner_path(path))
        .map(|path| (path.clone(), None))
        .collect();
    let mut reverted: Vec<String> = fixes.keys().cloned().collect();
    if !set.is_empty()
        && validated
            .changed_paths
            .iter()
            .any(|path| path.ends_with(".md") && !owner_path(path))
    {
        let snapshot = match snapshot {
            Some(snapshot) => snapshot,
            None => TipSnapshot::read(worktree, base_sha, &validated)?,
        };
        // Removed existing lines a revert must bring back (an exact restatement stays removed).
        let restore: BTreeSet<&String> = snapshot
            .lost
            .iter()
            .filter(|line| set.suppresses(line) && !set.suppresses_exactly(line))
            .collect();
        for (relative, tip) in &snapshot.tips {
            let base = snapshot.base_files.get(relative).map(String::as_str);
            let scrub = match (base, tip) {
                (Some(base), _)
                    if markdown_content_lines([base])
                        .iter()
                        .any(|line| restore.contains(line)) =>
                {
                    TipScrub::Reverted(set.scrub_base(base, &snapshot.existing))
                }
                (_, Some(tip)) => set.scrub_tip(tip, base, &snapshot.existing),
                (_, None) => TipScrub::Clean,
            };
            match scrub {
                TipScrub::Clean => {}
                TipScrub::Edited(text) => {
                    fixes.insert(relative.clone(), Some(text));
                }
                // The base without its exact restatements is what the run itself wrote: nothing to do.
                TipScrub::Reverted(Some(text)) if tip.as_ref() == Some(&text) => {}
                TipScrub::Reverted(text) => {
                    fixes.insert(relative.clone(), text);
                    reverted.push(relative.clone());
                }
            }
        }
    }
    if fixes.is_empty() {
        return Ok((validated, Vec::new()));
    }
    let git = |args: &[&str]| git_in(worktree, args);
    git(&["reset", "--hard", validated.tip_sha.as_str()])?;
    // Paths are run-chosen: every pathspec is literal (a file named `system/owner/[x].md` must
    // never match the other owner files) and every write goes through git (`stage_text`).
    for (relative, fix) in &fixes {
        match fix {
            // A revert derives from the base, an edit from the run's version.
            Some(text) => stage_text(
                worktree,
                relative,
                text,
                if reverted.contains(relative) {
                    base_sha
                } else {
                    validated.tip_sha.as_str()
                },
            )?,
            None if exists_in(worktree, base_sha, relative)? => {
                git(&[
                    "--literal-pathspecs",
                    "checkout",
                    base_sha,
                    "--",
                    relative.as_str(),
                ])?;
            }
            None => {
                git(&[
                    "--literal-pathspecs",
                    "rm",
                    "-q",
                    "-f",
                    "--ignore-unmatch",
                    "--",
                    relative.as_str(),
                ])?;
            }
        }
    }
    git(&["add", "--all"])?;
    if git(&["status", "--porcelain"])?.trim().is_empty() {
        return Ok((validated, Vec::new()));
    }
    git(&[
        "-c",
        "user.name=memory-suppression",
        "-c",
        "user.email=memory-suppression@localhost",
        "commit",
        "-m",
        "Drop suppressed owner memory",
    ])?;
    // `-z`: raw path bytes; without it a non-ASCII path is quoted and the resume scrub skips it.
    let changed_paths: Vec<String> = git(&["diff", "--name-only", "-z", base_sha, "HEAD"])?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    let tip_sha = if changed_paths.is_empty() {
        base_sha.to_owned()
    } else {
        git(&["rev-parse", "HEAD"])?.trim().to_owned()
    };
    let recorded: serde_json::Value =
        read_run_json(ledger_path).map_err(|error| error.to_string())?;
    let all_reverted: BTreeSet<String> = recorded[REVERTED_PATHS_KEY]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .chain(reverted.iter().cloned())
        .collect();
    let mut fields = serde_json::Map::new();
    fields.insert("validatedTipSha".into(), tip_sha.clone().into());
    fields.insert("validatedChangedPaths".into(), changed_paths.clone().into());
    fields.insert(
        REVERTED_PATHS_KEY.into(),
        all_reverted.into_iter().collect::<Vec<_>>().into(),
    );
    update_run_ledger(ledger_path, &fields).map_err(|error| error.to_string())?;
    Ok((
        ValidatedReflectionTip {
            tip_sha,
            changed_paths,
        },
        reverted,
    ))
}

/// `detail` plus the paths the scrub reverted, if any: a `system/owner/` change from the run, or a
/// file whose run version would have dropped or restated owner-suppressed memory.
fn with_reverted(detail: Option<String>, reverted: &BTreeSet<String>) -> Option<String> {
    if reverted.is_empty() {
        return detail;
    }
    let note = format!(
        "kept the base version (owner memory protection: system/owner change or owner suppression) of: {}",
        reverted
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    );
    Some(match detail {
        Some(detail) => format!("{detail}; {note}"),
        None => note,
    })
}

pub fn resolve_finalization_decision(
    identity: &MemoryIdentity,
    run_dir: &Path,
    input: &ReservationRunLedger,
    child: &RunOutcome,
    warn: WarnCallback<'_>,
) -> Result<DurableFinalizationDecision, String> {
    finalize(
        identity,
        run_dir,
        input,
        child,
        warn,
        &mut || {},
        &mut || {},
    )
}

/// [`resolve_finalization_decision`] with test seams: `before_writer_lock` runs after the tip is
/// validated and before the memory-write lock is taken for the suppression scrub and integration,
/// exactly where a concurrent forget can land; `inside_writer_lock` runs first thing inside the
/// suppression scrub, so a test can assert the scrub holds that lock. Production passes empty
/// closures.
#[doc(hidden)]
pub fn resolve_finalization_decision_interleaved(
    identity: &MemoryIdentity,
    run_dir: &Path,
    input: &ReservationRunLedger,
    child: &RunOutcome,
    before_writer_lock: &mut dyn FnMut(),
    inside_writer_lock: &mut dyn FnMut(),
) -> Result<DurableFinalizationDecision, String> {
    finalize(
        identity,
        run_dir,
        input,
        child,
        None,
        before_writer_lock,
        inside_writer_lock,
    )
}

fn finalize(
    identity: &MemoryIdentity,
    run_dir: &Path,
    input: &ReservationRunLedger,
    child: &RunOutcome,
    warn: WarnCallback<'_>,
    before_writer_lock: &mut dyn FnMut(),
    inside_writer_lock: &mut dyn FnMut(),
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
    let base_sha = ledger.value()["baseSha"]
        .as_str()
        .ok_or("Missing baseSha")?
        .to_owned();
    let mode = if ledger.value()["mergePolicy"] == "auto" {
        ReflectionIntegrationMode::Auto
    } else {
        ReflectionIntegrationMode::Integration
    };
    let trigger = ledger.value()["trigger"].as_str().map(str::to_owned);
    let run_id = ledger.run_id().to_owned();
    let mut reverted: BTreeSet<String> = ledger.value()[REVERTED_PATHS_KEY]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect();
    before_writer_lock();
    // Read outside the critical section (base_sha and the tip never change); a forget that adds
    // the first suppression after this point is still seen inside, which then reads it there.
    let snapshot = if worktree.dir.exists()
        && !SuppressionSet::load(&identity.paths.runtime).is_ok_and(|set| set.is_empty())
    {
        Some(TipSnapshot::read(&worktree, &base_sha, &validated)?)
    } else {
        None
    };
    let engine = crate::engine_session::prepare_memory_engine_session(
        &identity.id,
        &identity.paths,
        Default::default(),
    )
    .map_err(|error| error.to_string())?;
    // One critical section from the suppression scrub through the merge: a forget either finished
    // before it (and the scrub sees its suppression) or runs after it (and removes what merged).
    let (result, keep_worktree) = engine
        .lock
        .run(
            "memory-write",
            || -> Result<(DurableFinalizationDecision, bool), String> {
                let (validated, now_reverted) = scrub_suppressed_tip(
                    identity,
                    &worktree,
                    &path,
                    &base_sha,
                    validated,
                    snapshot,
                    inside_writer_lock,
                )?;
                reverted.extend(now_reverted);
                if validated.tip_sha == base_sha {
                    return Ok((
                        decision("no_changes", None, with_reverted(None, &reverted), None),
                        false,
                    ));
                }
                Ok(
                    match probe_reflection_integration(&worktree, mode, &run_id, &validated.tip_sha)
                    {
                        ReflectionIntegrationProbe::AlreadyMerged { integration_sha } => (
                            decision(
                                "merged",
                                None,
                                with_reverted(None, &reverted),
                                Some(integration_sha),
                            ),
                            false,
                        ),
                        ReflectionIntegrationProbe::NotIntegrated if !worktree.dir.exists() => (
                            decision("failed", Some("missing_worktree"), None, None),
                            true,
                        ),
                        ReflectionIntegrationProbe::NotIntegrated => {
                            // Read from the validated commit's trailers, so a run resumed after the validated
                            // checkpoint still reports what the gate dropped before it merged the rest.
                            let dropped_note = read_dropped_trailers(
                                worktree.exec.as_ref(),
                                &worktree.dir,
                                &base_sha,
                                &validated.tip_sha,
                            )
                            .ok()
                            .filter(|entries| !entries.is_empty())
                            .map(|entries| describe_dropped_entries(&entries));
                            let summary = format!(
                                "{} {run_id}",
                                trigger.as_deref().ok_or("Missing trigger")?
                            );
                            // The writer lock is already held; the integration runs inside it directly.
                            let integrated = integrate_validated_reflection(
                                &worktree,
                                IntegrateValidatedReflectionInput {
                                    mode,
                                    run_id: run_id.clone(),
                                    summary,
                                    validated,
                                },
                                |operation| operation(),
                            );
                            let outcome = serde_json::to_value(integrated.outcome)
                                .map_err(|error| error.to_string())?;
                            let detail = match (integrated.detail, dropped_note) {
                                (Some(detail), Some(note)) => Some(format!("{detail}; {note}")),
                                (detail, note) => detail.or(note),
                            };
                            let mut merged = decision(
                                outcome.as_str().ok_or("Invalid integration outcome")?,
                                None,
                                with_reverted(detail, &reverted),
                                integrated.integration_sha,
                            );
                            if merged.outcome == "merged" {
                                validate_integrated_dream_budget(identity, &ledger, &mut merged)?;
                            }
                            (merged, false)
                        }
                    },
                )
            },
        )
        .map_err(|error| error.message)?;
    checkpoint(&path, &result)?;
    if keep_worktree {
        return Ok(result);
    }
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
        fixture_seeded(root, &[])
    }
    /// [`fixture`] with `seed` files committed to the memory repository before the run branches.
    fn fixture_seeded(
        root: &Path,
        seed: &[(&str, &str)],
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
        if !seed.is_empty() {
            for (path, content) in seed {
                let file = identity.paths.repo.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(&file, content).unwrap();
            }
            for args in [
                vec!["add", "--all"],
                vec![
                    "-c",
                    "user.name=agent",
                    "-c",
                    "user.email=agent@example.invalid",
                    "commit",
                    "-m",
                    "Seed",
                ],
            ] {
                let output = exec.run_in(&identity.paths.repo, &args).unwrap();
                assert_eq!(output.code, 0, "{}", output.stderr);
            }
        }
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
    #[test]
    fn an_unreadable_suppression_store_integrates_nothing() {
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
        std::fs::create_dir_all(
            identity
                .paths
                .runtime
                .join(memory_core::receipts::owner::SUPPRESSIONS_FILE),
        )
        .unwrap();
        let head = memory_core::git::GitMemoryRepo::open(&identity.paths.repo, "agent")
            .unwrap()
            .head()
            .unwrap();
        assert!(
            resolve_finalization_decision(
                &identity,
                &root.path().join("run"),
                &ledger,
                &child,
                None
            )
            .is_err(),
            "fails closed"
        );
        assert!(
            !identity.paths.repo.join("reference-fact.md").exists(),
            "nothing merged"
        );
        assert_eq!(
            memory_core::git::GitMemoryRepo::open(&identity.paths.repo, "agent")
                .unwrap()
                .head()
                .unwrap(),
            head
        );
        assert!(worktree.dir.exists(), "the run stays resumable");
        assert!(
            !memory_core::locks::memory_writer_lock_path(&identity.paths.locks).exists(),
            "the writer lock is released"
        );
        let checkpoint: serde_json::Value =
            read_run_json(&root.path().join("run/ledger.json")).unwrap();
        assert!(checkpoint.get("finalizeOutcome").is_none());
    }
    #[test]
    fn a_run_change_under_system_owner_never_integrates_and_is_surfaced() {
        let root = tempfile::tempdir().unwrap();
        let (identity, ledger, child, worktree) = fixture(root.path());
        std::fs::create_dir_all(worktree.dir.join("system/owner")).unwrap();
        std::fs::write(
            worktree.dir.join("system/owner/rcpt_forged.md"),
            "---\ndescription: Forged\n---\nForged owner rule\n",
        )
        .unwrap();
        std::fs::write(
            worktree.dir.join("reference-fact.md"),
            "---\ndescription: Fact\n---\nNew fact\n",
        )
        .unwrap();
        for args in [
            vec!["add", "--all"],
            vec![
                "-c",
                "user.name=agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-m",
                "Record reflection",
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
        assert!(identity.paths.repo.join("reference-fact.md").exists());
        assert!(
            !identity
                .paths
                .repo
                .join("system/owner/rcpt_forged.md")
                .exists(),
            "a forged owner file never merges"
        );
        assert!(
            result
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("system/owner/rcpt_forged.md")),
            "{:?}",
            result.detail
        );
        let checkpoint: serde_json::Value =
            read_run_json(&root.path().join("run/ledger.json")).unwrap();
        assert_eq!(
            checkpoint["validatedChangedPaths"],
            serde_json::json!(["reference-fact.md"])
        );
        assert_eq!(
            checkpoint["suppressionRevertedPaths"],
            serde_json::json!(["system/owner/rcpt_forged.md"])
        );
        assert_eq!(
            checkpoint["finalizeDetail"].as_str(),
            result.detail.as_deref()
        );
    }
    #[test]
    fn a_glob_named_owner_file_reverts_only_itself() {
        let root = tempfile::tempdir().unwrap();
        let (identity, ledger, child, worktree) = fixture_seeded(
            root.path(),
            &[
                (
                    "system/owner/rcpt_a.md",
                    "---\ndescription: A\n---\n- Owner rule A.\n",
                ),
                (
                    "system/owner/rcpt_b.md",
                    "---\ndescription: B\n---\n- Owner rule B.\n",
                ),
            ],
        );
        // `[ab]` is a glob character class matching rcpt_a.md and rcpt_b.md; valid on Windows too.
        std::fs::write(
            worktree.dir.join("system/owner/rcpt_[ab].md"),
            "---\ndescription: Glob\n---\n- Glob\n",
        )
        .unwrap();
        std::fs::write(
            worktree.dir.join("reference-fact.md"),
            "---\ndescription: Fact\n---\nNew fact\n",
        )
        .unwrap();
        for args in [
            vec!["add", "--all"],
            vec![
                "-c",
                "user.name=agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-m",
                "Record reflection",
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
        assert_eq!(result.outcome, "merged", "{:?}", result.detail);
        assert_eq!(
            std::fs::read_to_string(identity.paths.repo.join("system/owner/rcpt_a.md")).unwrap(),
            "---\ndescription: A\n---\n- Owner rule A.\n"
        );
        assert_eq!(
            std::fs::read_to_string(identity.paths.repo.join("system/owner/rcpt_b.md")).unwrap(),
            "---\ndescription: B\n---\n- Owner rule B.\n"
        );
        assert!(
            !identity
                .paths
                .repo
                .join("system/owner/rcpt_[ab].md")
                .exists()
        );
        assert!(identity.paths.repo.join("reference-fact.md").exists());
        let checkpoint: serde_json::Value =
            read_run_json(&root.path().join("run/ledger.json")).unwrap();
        assert_eq!(
            checkpoint["suppressionRevertedPaths"],
            serde_json::json!(["system/owner/rcpt_[ab].md"])
        );
        assert_eq!(
            checkpoint["validatedChangedPaths"],
            serde_json::json!(["reference-fact.md"])
        );
    }
    #[test]
    fn owner_path_ignores_case() {
        assert!(owner_path("system/owner/rcpt_a.md"));
        assert!(owner_path("System/Owner/rcpt_a.md"));
        assert!(!owner_path("system/owners/rcpt_a.md"));
        assert!(!owner_path("system/owner"));
    }
    #[cfg(unix)]
    #[test]
    fn a_restored_file_is_never_written_through_a_symlink_the_run_committed() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let base = "- Never use the QA remote host for prod deploys.\n- Use the QA remote host.\n";
        let (identity, ledger, child, worktree) =
            fixture_seeded(root.path(), &[("notes/rules.md", base)]);
        // The forget lands after the run started: the base still restates it exactly.
        memory_core::receipts::owner::append_suppressions(
            &identity.paths.runtime,
            memory_core::receipts::owner::SUPPRESSION_FORGET,
            "Use the QA remote host.",
            None,
            "forget-src",
        )
        .unwrap();
        // The run deletes notes/rules.md and commits `notes` as a symlink to a directory outside.
        std::fs::remove_dir_all(worktree.dir.join("notes")).unwrap();
        std::os::unix::fs::symlink(outside.path(), worktree.dir.join("notes")).unwrap();
        // `--no-verify`: a hostile run bypasses the staged-memory hook that refuses symlinks.
        for args in [
            vec!["add", "--all"],
            vec![
                "-c",
                "user.name=agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "--no-verify",
                "-m",
                "Record reflection",
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
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "nothing is written outside the worktree"
        );
        let result = result.unwrap();
        assert_eq!(result.outcome, "merged", "{:?}", result.detail);
        let restored = identity.paths.repo.join("notes/rules.md");
        assert!(
            !std::fs::symlink_metadata(identity.paths.repo.join("notes"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(restored).unwrap(),
            "- Never use the QA remote host for prod deploys.\n"
        );
        let checkpoint: serde_json::Value =
            read_run_json(&root.path().join("run/ledger.json")).unwrap();
        assert_eq!(
            checkpoint["suppressionRevertedPaths"],
            serde_json::json!(["notes/rules.md"])
        );
    }
    #[test]
    fn a_non_ascii_path_is_recorded_raw_and_scrubbed_on_resume() {
        let root = tempfile::tempdir().unwrap();
        let person = "people/김철수.md";
        let (identity, _ledger, _child, worktree) = fixture_seeded(
            root.path(),
            &[(person, "---\ndescription: Kim\n---\n- Kim likes tea.\n")],
        );
        let rule = "Use the QA remote host for every build.";
        std::fs::write(
            worktree.dir.join(person),
            format!("---\ndescription: Kim\n---\n- Kim likes tea.\n- {rule}\n"),
        )
        .unwrap();
        std::fs::create_dir_all(worktree.dir.join("system/owner")).unwrap();
        std::fs::write(
            worktree.dir.join("system/owner/rcpt_forged.md"),
            "---\ndescription: Forged\n---\n- Forged\n",
        )
        .unwrap();
        for args in [
            vec!["add", "--all"],
            vec![
                "-c",
                "user.name=agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-m",
                "Record reflection",
            ],
        ] {
            let output = worktree.exec.run_in(&worktree.dir, &args).unwrap();
            assert_eq!(output.code, 0, "{}", output.stderr);
        }
        let ledger_path = root.path().join("run/ledger.json");
        let base_sha = worktree.base_commit_sha.clone();
        let tip_sha = git_in(&worktree, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        // First scrub, no suppression yet: only the owner file reverts, and the checkpoint records the raw path.
        let first = ValidatedReflectionTip {
            tip_sha,
            changed_paths: vec![person.to_owned(), "system/owner/rcpt_forged.md".to_owned()],
        };
        let (scrubbed, _) = scrub_suppressed_tip(
            &identity,
            &worktree,
            &ledger_path,
            &base_sha,
            first,
            None,
            &mut || {},
        )
        .unwrap();
        assert_eq!(scrubbed.changed_paths, vec![person.to_owned()]);
        let checkpoint: serde_json::Value = read_run_json(&ledger_path).unwrap();
        assert_eq!(
            checkpoint["validatedChangedPaths"],
            serde_json::json!([person])
        );
        // A forget lands before integration (the crash window); the resume rebuilds the tip from the ledger.
        memory_core::receipts::owner::append_suppressions(
            &identity.paths.runtime,
            memory_core::receipts::owner::SUPPRESSION_FORGET,
            rule,
            None,
            "forget-src",
        )
        .unwrap();
        let resumed = ValidatedReflectionTip {
            tip_sha: checkpoint["validatedTipSha"].as_str().unwrap().to_owned(),
            changed_paths: checkpoint["validatedChangedPaths"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
        };
        let (rescrubbed, _) = scrub_suppressed_tip(
            &identity,
            &worktree,
            &ledger_path,
            &base_sha,
            resumed,
            None,
            &mut || {},
        )
        .unwrap();
        assert_ne!(
            rescrubbed.tip_sha, scrubbed.tip_sha,
            "the resume scrub committed"
        );
        assert_eq!(
            git_in(
                &worktree,
                &["show", format!("{}:{person}", rescrubbed.tip_sha).as_str()]
            )
            .unwrap(),
            "---\ndescription: Kim\n---\n- Kim likes tea.\n"
        );
    }
    #[test]
    fn an_edited_executable_file_keeps_its_mode() {
        let root = tempfile::tempdir().unwrap();
        let (identity, ledger, child, worktree) = fixture(root.path());
        let rule = "Use the QA remote host for every build.";
        std::fs::create_dir_all(worktree.dir.join("reference")).unwrap();
        std::fs::write(
            worktree.dir.join("reference/tool.md"),
            format!("---\ndescription: Tool\n---\n- Tool runs fast.\n- {rule}\n"),
        )
        .unwrap();
        // `update-index --chmod`: the mode is set in the index, so this works without a filesystem exec bit (Windows).
        // The closing checkout makes git materialize that mode in the worktree, so with core.filemode=true the
        // run's committed state is clean (completion validation reports a mode-only diff as dirty_uncommitted).
        for args in [
            vec!["add", "--all"],
            vec!["update-index", "--chmod=+x", "reference/tool.md"],
            vec![
                "-c",
                "user.name=agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-m",
                "Record reflection",
            ],
            vec!["checkout", "--", "reference/tool.md"],
        ] {
            let output = worktree.exec.run_in(&worktree.dir, &args).unwrap();
            assert_eq!(output.code, 0, "{}", output.stderr);
        }
        let status = worktree
            .exec
            .run_in(&worktree.dir, &["status", "--porcelain"])
            .unwrap()
            .stdout;
        assert!(
            status.trim().is_empty(),
            "the run commits a clean worktree: {status}"
        );
        memory_core::receipts::owner::append_suppressions(
            &identity.paths.runtime,
            memory_core::receipts::owner::SUPPRESSION_FORGET,
            rule,
            None,
            "forget-src",
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
        assert_eq!(result.outcome, "merged", "{:?}", result.detail);
        let exec = memory_core::git::exec::create_git_exec(Default::default());
        let entry = exec
            .run_in(
                &identity.paths.repo,
                &["ls-tree", "HEAD", "--", "reference/tool.md"],
            )
            .unwrap()
            .stdout;
        assert!(entry.starts_with("100755 "), "{entry}");
        assert_eq!(
            exec.run_in(&identity.paths.repo, &["show", "HEAD:reference/tool.md"])
                .unwrap()
                .stdout,
            "---\ndescription: Tool\n---\n- Tool runs fast.\n"
        );
    }
}
