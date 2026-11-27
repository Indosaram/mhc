//! Path-bound import, staged payload checks, and file-authoritative status.
use super::bundle::*;
use super::landed_file::{LandedFile, outcome_in, read_landing};
use super::move_landing::LandingOutcome;
use super::move_ledger::{
    DeclineReason, FailReason, MoveFencedError, MoveLedger, MoveLedgerError, MoveRecord, MoveStatus,
};
use super::move_steps::{MoveStep, move_step};
use super::name_folding::{ProbeFoldingFn, refuse_names_that_fold};
use super::ownership::*;
use super::repository_check::{CheckoutVerdict, checkout_refusal, checkout_verdict};
use super::staged_layout::{STAGED_PAYLOADS, staged_name, staged_names, staged_twice};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type RepositoryIdentityReader<'a> =
    &'a dyn Fn(&Path) -> Result<Option<RepositoryIdentity>, String>;
pub struct ImportMovedSessionOptions<'a> {
    pub bundle_dir: &'a Path,
    pub target_cwd: &'a Path,
    pub ledger: &'a MoveLedger,
    pub session_dir: Option<&'a Path>,
    pub from: Option<&'a str>,
    pub read_identity: Option<RepositoryIdentityReader<'a>>,
}

#[derive(Default)]
pub struct ImportMovedSessionExtras<'a> {
    pub sent: Option<&'a HashMap<String, String>>,
    pub probe_name_folding: Option<ProbeFoldingFn<'a>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusFileKind {
    Copy,
    Landing,
}

pub struct StatusFile<'a> {
    pub file: &'a Path,
    pub kind: StatusFileKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusAndLanding {
    pub answer: MoveStatus,
    pub landed: Option<LandedFile>,
}

pub async fn import_outcome(file: &Path, move_id: &str) -> LandingOutcome {
    outcome_in(&super::entries(file).await, move_id)
}

pub async fn answer_move_status(
    ledger: &MoveLedger,
    move_id: &str,
    session_file: Option<&Path>,
) -> Result<MoveStatus, String> {
    let asked = session_file.map(|file| StatusFile {
        file,
        kind: StatusFileKind::Copy,
    });
    answer_move_status_and_landing(ledger, move_id, asked)
        .await
        .map(|res| res.answer)
}

pub async fn answer_move_status_and_landing(
    ledger: &MoveLedger,
    move_id: &str,
    asked: Option<StatusFile<'_>>,
) -> Result<StatusAndLanding, String> {
    let seen_file = std::sync::Arc::new(std::sync::Mutex::new(None::<LandedFile>));
    let mut seen_landing: Option<LandedFile> = None;
    let seen_file_reader = std::sync::Arc::clone(&seen_file);
    let landed = |target: String| -> std::pin::Pin<
        Box<dyn std::future::Future<Output = LandingOutcome> + Send>,
    > {
        let move_id = move_id.to_owned();
        let seen_file_reader = std::sync::Arc::clone(&seen_file_reader);
        Box::pin(async move {
            let target = PathBuf::from(target);
            let read = read_landing(&target, &move_id).await;
            if let Ok(mut guard) = seen_file_reader.lock() {
                *guard = Some(read.file);
            }
            read.outcome
        })
    };
    let answer = match asked {
        Some(StatusFile { file, kind }) if super::path_exists(file).await => {
            crate::session_sidecar_store::serialize_by_key(
                &super::session_lock::move_lock_key(file).await,
                async {
                    let read = read_landing(file, move_id).await;
                    if read.outcome == LandingOutcome::Absent {
                        if kind == StatusFileKind::Copy {
                            return Err(format!(
                                "move_mismatch: {} is not the copy this device imported for move {move_id}; nothing was answered",
                                file.display()
                            ));
                        }
                        return ledger
                            .answer_status_with(move_id, false, false, Some(&landed))
                            .await
                            .map_err(|e| e.to_string());
                    }
                    if kind == StatusFileKind::Landing {
                        seen_landing = Some(read.file);
                    }
                    ledger
                        .answer_status_with(
                            move_id,
                            true,
                            read.outcome == LandingOutcome::Revoked,
                            Some(&landed),
                        )
                        .await
                        .map_err(|e| e.to_string())
                },
            )
            .await?
        }
        _ => ledger
            .answer_status_with(move_id, false, false, Some(&landed))
            .await
            .map_err(|e| e.to_string())?,
    };
    let file_landed = seen_file.lock().ok().and_then(|g| g.clone());
    let where_landed = file_landed.or(seen_landing);
    let landed_out = if matches!(answer, MoveStatus::Accepted | MoveStatus::AcceptedRevoked) {
        where_landed
    } else {
        None
    };
    Ok(StatusAndLanding {
        answer,
        landed: landed_out,
    })
}

pub async fn import_moved_session(
    options: ImportMovedSessionOptions<'_>,
) -> Result<PathBuf, String> {
    import_moved_session_with(options, ImportMovedSessionExtras::default()).await
}

pub async fn import_moved_session_with(
    options: ImportMovedSessionOptions<'_>,
    extras: ImportMovedSessionExtras<'_>,
) -> Result<PathBuf, String> {
    let manifest = read_manifest(options.bundle_dir).map_err(|e| e.to_string())?;
    let cwd = super::session_lock::real_session_path_async(options.target_cwd).await;
    let cwd_str = cwd.to_string_lossy().into_owned();
    let session_dir = match options.session_dir {
        Some(dir) => super::session_lock::real_session_path_async(dir).await,
        None => PathBuf::from(crate::session_manager::get_default_session_dir(&cwd_str)),
    };
    let target = session_dir.join(&manifest.session_file_name);
    // Once this move's file landed, the move records no failure: only its `accepted` write can have been lost.
    let landed = || async {
        super::path_exists(&target).await
            && import_outcome(&target, &manifest.move_id).await != LandingOutcome::Absent
    };
    let confirm_landed = || async {
        let revoked = import_outcome(&target, &manifest.move_id).await == LandingOutcome::Revoked;
        options
            .ledger
            .confirm_landed(&manifest.move_id, &target.to_string_lossy(), revoked)
            .await
            .map_err(|e| e.to_string())?;
        if let Ok(Some(landed_for)) = crate::session_cwd::read_session_cwd(&target).await
            && landed_for != cwd_str
        {
            return Err(MoveFencedError::new(
                manifest.move_id.clone(),
                MoveRecord::Status(if revoked {
                    MoveStatus::AcceptedRevoked
                } else {
                    MoveStatus::Accepted
                }),
                Some(format!(
                    "it already landed here for {landed_for}, and a move lands for one folder"
                )),
            )
            .to_string());
        }
        Ok(target.clone())
    };
    if landed().await {
        return confirm_landed().await;
    }
    let listed_paths: Vec<String> = listed_files(&manifest)
        .into_iter()
        .map(|f| f.path)
        .collect();
    let staged_paths = staged_names(&manifest);
    refuse_names_that_fold(
        &[&listed_paths, &staged_paths],
        &session_dir,
        extras.probe_name_folding,
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(twice) = staged_twice(&manifest) {
        let err = MoveBundleInvalidError::new(twice);
        options
            .ledger
            .fail_unless_landed(&manifest.move_id, FailReason::BundleInvalid, landed())
            .await
            .map_err(|e| e.to_string())?;
        return Err(err.to_string());
    }
    if let Err(error) =
        verify_bundle_with_sent_async(options.bundle_dir, &manifest, extras.sent.cloned()).await
    {
        options
            .ledger
            .fail_unless_landed(&manifest.move_id, FailReason::BundleInvalid, landed())
            .await
            .map_err(|e| e.to_string())?;
        return Err(error.to_string());
    }
    options
        .ledger
        .require_readied_for(&manifest.move_id, &cwd_str)
        .await
        .map_err(|e| e.to_string())?;
    let verdict = if let Some(read) = options.read_identity {
        let identity = read(&cwd)?;
        if same_repository(manifest.repository.as_ref(), identity.as_ref()) {
            CheckoutVerdict::Same
        } else if super::path_exists(&cwd).await {
            CheckoutVerdict::Other
        } else {
            CheckoutVerdict::Missing
        }
    } else {
        checkout_verdict(&cwd_str, manifest.repository.as_ref(), None).await
    };
    if verdict != CheckoutVerdict::Same {
        if !landed().await {
            options
                .ledger
                .decline(&manifest.move_id, DeclineReason::Repository)
                .await
                .map_err(|e| e.to_string())?;
        }
        return Err(
            checkout_refusal(verdict, cwd_str, Some(manifest.source_cwd.clone())).to_string(),
        );
    }
    if let Err(error) = options
        .ledger
        .begin_receive_with_cwd(&manifest.move_id, &target.to_string_lossy(), Some(&cwd_str))
        .await
    {
        if matches!(error, MoveLedgerError::Fenced(_)) && landed().await {
            return confirm_landed().await;
        }
        return Err(error.to_string());
    }
    move_step(MoveStep::ImportReceived)?;
    let staging = session_dir.join(".move-staging").join(&manifest.move_id);
    let assert_held = || {
        options
            .ledger
            .assert_held(&manifest.move_id)
            .map_err(|e| e.to_string())
    };
    let result: Result<PathBuf, String> = async {
        let staged = stage(
            options.bundle_dir,
            &manifest,
            &staging,
            options.from.unwrap_or_default(),
        )
        .await?;
        let target_for_land = target.clone();
        let manifest_for_land = manifest.clone();
        let file = options
            .ledger
            .commit_with_stamp(
                &manifest.move_id,
                &target.to_string_lossy(),
                |stamp_stands| async move {
                    stamp_stands().map_err(|e| e.to_string())?;
                    land(&staged, &target_for_land, &cwd, &manifest_for_land).await
                },
                async {
                    replace_predecessor(&target, &manifest.session_id, &session_dir, &assert_held)
                        .await?;
                    // Before the rename: a crash here re-copies on retry, and after it the state is already in place.
                    place_extension_state(
                        &staging,
                        &manifest,
                        &super::extension_state::moved_state_dir(&target, &manifest.move_id)?,
                        &assert_held,
                    )
                    .await
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        move_step(MoveStep::ImportAccepted)?;
        super::remove(&staging).await?;
        Ok(file)
    }
    .await;
    if let Err(error) = &result {
        let is_fenced =
            error.contains("move_fenced") || error.contains("can no longer be imported");
        if is_fenced && landed().await {
            let _ = super::remove(&staging).await;
            return confirm_landed().await;
        }
        if !is_fenced {
            let reason = if error.contains("could not be verified") {
                FailReason::BundleInvalid
            } else {
                FailReason::ImportFailed
            };
            options
                .ledger
                .fail_unless_landed(&manifest.move_id, reason, landed())
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    result
}
async fn replace_predecessor(
    target: &Path,
    session_id: &str,
    session_dir: &Path,
    assert_held: &(dyn Fn() -> Result<(), String> + Send + Sync),
) -> Result<(), String> {
    if !super::path_exists(target).await {
        return Ok(());
    }
    let entries = super::entries(target).await;
    if header_session_id(&entries) != Some(session_id)
        || session_ownership(&entries) == SessionOwnership::Writable
    {
        return Err(format!(
            "move_target_occupied: a writable session already sits at {}",
            target.display()
        ));
    }
    let extensions = session_dir.join("extensions");
    if super::path_exists(&extensions).await {
        let mut entries = tokio::fs::read_dir(extensions)
            .await
            .map_err(|e| e.to_string())?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            if entry.file_type().await.map_err(|e| e.to_string())?.is_dir() {
                assert_held()?;
                super::remove(&entry.path().join(format!(
                    "{}.json",
                    crate::session_sidecar_store::encoded_session_id(session_id)
                )))
                .await?;
            }
        }
    }
    assert_held()?;
    super::remove(&session_dir.join("resident-blobs").join(session_id)).await?;
    assert_held()?;
    super::remove(target).await
}
pub async fn stage_payloads(
    bundle_dir: &Path,
    manifest: &MoveManifest,
    staging: &Path,
) -> Result<(), String> {
    for file in manifest.payloads.iter().flat_map(|p| &p.files) {
        let to = staging.join(STAGED_PAYLOADS).join(&file.path);
        super::copy(&bundle_dir.join(&file.path), &to).await?;
        if sha256_file_async(&to).await.map_err(|e| e.to_string())? != file.sha256 {
            return Err(
                MoveBundleInvalidError::new(format!("{} changed in transit", file.path))
                    .to_string(),
            );
        }
    }
    Ok(())
}
pub async fn place_extension_state(
    staging: &Path,
    manifest: &MoveManifest,
    dir: &Path,
    assert_held: &(dyn Fn() -> Result<(), String> + Send + Sync),
) -> Result<(), String> {
    let files: Vec<_> = manifest
        .payloads
        .iter()
        .filter(|p| p.kind == "extension-state")
        .flat_map(|p| &p.files)
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    assert_held()?;
    super::remove(dir).await?;
    for file in files {
        assert_held()?;
        super::copy(
            &staging.join(STAGED_PAYLOADS).join(&file.path),
            &dir.join(
                file.path
                    .strip_prefix(EXTENSION_STATE_PREFIX)
                    .ok_or("invalid extension state path")?,
            ),
        )
        .await?;
    }
    Ok(())
}
async fn stage(
    bundle_dir: &Path,
    manifest: &MoveManifest,
    staging: &Path,
    from: &str,
) -> Result<PathBuf, String> {
    let file = staging.join(&manifest.session_file_name);
    if !super::path_exists(&file).await
        || import_outcome(&file, &manifest.move_id).await == LandingOutcome::Absent
    {
        super::remove(staging).await?;
        let mut listed = vec![&manifest.session];
        listed.extend(manifest.sidecars.iter());
        let staged_at = |item: &BundleFile| {
            if item.path == manifest.session.path {
                file.clone()
            } else {
                staging.join(staged_name(manifest, item))
            }
        };
        for item in &listed {
            super::copy(&bundle_dir.join(&item.path), &staged_at(item)).await?;
        }
        stage_payloads(bundle_dir, manifest, staging).await?;
        for item in &listed {
            if sha256_file_async(&staged_at(item))
                .await
                .map_err(|e| e.to_string())?
                != item.sha256
            {
                return Err(MoveBundleInvalidError::new(format!(
                    "{} changed in transit",
                    item.path
                ))
                .to_string());
            }
        }
        super::append_custom(
            &file,
            SESSION_MOVED_IN,
            json!({"move_id":manifest.move_id,"session_id":manifest.session_id,"from":from,"bundle_sha256":manifest.session.sha256,"moved_in_at":crate::session_manager::now_iso_millis()}),
        )
        .await?;
    }
    super::requeue::requeue_input(&file, &manifest.move_id, &manifest.dropped_user_messages)
        .await?;
    Ok(file)
}
async fn land(
    staged: &Path,
    target: &Path,
    cwd: &Path,
    manifest: &MoveManifest,
) -> Result<PathBuf, String> {
    let content = tokio::fs::read_to_string(staged)
        .await
        .map_err(|e| e.to_string())?;
    let (first, rest) = content.split_once('\n').unwrap_or((&content, ""));
    let mut header: serde_json::Value = serde_json::from_str(first).map_err(|e| e.to_string())?;
    header["cwd"] = json!(cwd.to_string_lossy());
    let dir = target.parent().ok_or("missing target directory")?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| e.to_string())?;
    let temp = dir.join(format!(
        ".{}.rebind-{}.tmp",
        manifest.session_file_name,
        std::process::id()
    ));
    tokio::fs::write(&temp, format!("{header}\n{rest}"))
        .await
        .map_err(|e| e.to_string())?;
    for item in &manifest.sidecars {
        let relative = item.path.strip_prefix("files/").unwrap_or(&item.path);
        super::copy(
            &staged
                .parent()
                .ok_or("missing staging directory")?
                .join(relative),
            &dir.join(relative),
        )
        .await?;
    }
    tokio::fs::rename(temp, target)
        .await
        .map_err(|e| e.to_string())?;
    super::remove(staged).await?;
    Ok(target.into())
}
fn same_repository(a: Option<&RepositoryIdentity>, b: Option<&RepositoryIdentity>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    if !a.root_commits.is_empty() && !b.root_commits.is_empty() {
        return a
            .root_commits
            .iter()
            .any(|root| b.root_commits.contains(root));
    }
    matches!((&a.origin_url, &b.origin_url), (Some(a),Some(b)) if a == b)
}
