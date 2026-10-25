//! Path-bound import, staged payload checks, and file-authoritative status.
use super::bundle::*;
use super::move_landing::LandingOutcome;
use super::move_ledger::{DeclineReason, FailReason, MoveLedger, MoveStatus};
use super::move_steps::{MoveStep, move_step};
use super::ownership::*;
use serde_json::json;
use std::path::{Path, PathBuf};

pub type RepositoryIdentityReader<'a> = &'a dyn Fn(&Path) -> Result<Option<RepositoryIdentity>, String>;
pub struct ImportMovedSessionOptions<'a> {
    pub bundle_dir: &'a Path,
    pub target_cwd: &'a Path,
    pub ledger: &'a MoveLedger,
    pub session_dir: Option<&'a Path>,
    pub from: Option<&'a str>,
    pub read_identity: Option<RepositoryIdentityReader<'a>>,
}
pub fn import_outcome(file: &Path, move_id: &str) -> LandingOutcome {
    let mut outcome = LandingOutcome::Absent;
    for entry in super::entries(file) {
        if entry["type"] != "custom" || entry["data"]["move_id"] != move_id {
            continue;
        }
        if entry["customType"] == SESSION_MOVED_IN {
            outcome = LandingOutcome::MovedIn;
        } else if outcome == LandingOutcome::MovedIn
            && entry["customType"] == SESSION_MOVED
            && entry["data"]["reason"] == "revoked"
        {
            outcome = LandingOutcome::Revoked;
        }
    }
    outcome
}
pub async fn answer_move_status(
    ledger: &MoveLedger,
    move_id: &str,
    session_file: Option<&Path>,
) -> Result<MoveStatus, String> {
    let landed = |target: &str| import_outcome(Path::new(target), move_id);
    let Some(file) = session_file.filter(|file| file.exists()) else {
        return ledger
            .answer_status_with(move_id, false, false, Some(&landed))
            .await
            .map_err(|e| e.to_string());
    };
    crate::session_sidecar_store::serialize_by_key(&super::session_lock::move_lock_key(file), async {
        let outcome = import_outcome(file, move_id);
        if outcome == LandingOutcome::Absent { return Err(format!("move_mismatch: {} is not the copy this device imported for move {move_id}; nothing was answered", file.display())); }
        ledger.answer_status_with(move_id, true, outcome == LandingOutcome::Revoked, Some(&landed)).await.map_err(|e| e.to_string())
    }).await
}
pub async fn import_moved_session(
    options: ImportMovedSessionOptions<'_>,
) -> Result<PathBuf, String> {
    let manifest = read_manifest(options.bundle_dir).map_err(|e| e.to_string())?;
    let cwd = super::session_lock::real_session_path(options.target_cwd);
    let session_dir = options
        .session_dir
        .map(super::session_lock::real_session_path)
        .unwrap_or_else(|| {
            PathBuf::from(crate::session_manager::get_default_session_dir(
                &cwd.to_string_lossy(),
            ))
        });
    let target = session_dir.join(&manifest.session_file_name);
    let landed = || import_outcome(&target, &manifest.move_id) != LandingOutcome::Absent;
    if landed() {
        options
            .ledger
            .confirm_landed(
                &manifest.move_id,
                &target.to_string_lossy(),
                import_outcome(&target, &manifest.move_id) == LandingOutcome::Revoked,
            )
            .await
            .map_err(|e| e.to_string())?;
        return Ok(target);
    }
    if let Err(error) = verify_bundle_async(options.bundle_dir, &manifest).await {
        options
            .ledger
            .fail_unless_landed(&manifest.move_id, FailReason::BundleInvalid, landed)
            .await
            .map_err(|e| e.to_string())?;
        return Err(error.to_string());
    }
    let identity = match options.read_identity {
        Some(read) => read(&cwd)?,
        None => read_identity(&cwd).await?,
    };
    if !same_repository(manifest.repository.as_ref(), identity.as_ref()) {
        if !landed() {
            options
                .ledger
                .decline(&manifest.move_id, DeclineReason::Repository)
                .await
                .map_err(|e| e.to_string())?;
        }
        return Err(format!(
            "move_repository_mismatch: {} is not the same repository the session was recorded in ({})",
            cwd.display(),
            manifest.source_cwd
        ));
    }
    options
        .ledger
        .begin_receive(&manifest.move_id, &target.to_string_lossy())
        .await
        .map_err(|e| e.to_string())?;
    move_step(MoveStep::ImportReceived)?;
    let staging = session_dir.join(".move-staging").join(&manifest.move_id);
    let result: Result<PathBuf, String> = async {
        let staged = stage(
            options.bundle_dir,
            &manifest,
            &staging,
            options.from.unwrap_or_default(),
        )
        .await?;
        let file = options
            .ledger
            .commit(
                &manifest.move_id,
                &target.to_string_lossy(),
                || land(&staged, &target, &cwd, &manifest),
                || {
                    replace_predecessor(&target, &manifest.session_id, &session_dir)?;
                    place_extension_state(
                        &staging,
                        &manifest,
                        &super::extension_state::moved_state_dir(&target, &manifest.move_id)?,
                    )
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        move_step(MoveStep::ImportAccepted)?;
        super::remove(&staging)?;
        Ok(file)
    }
    .await;
    if let Err(error) = &result
        && !error.contains("move_fenced") && !error.contains("can no longer be imported") {
            let reason = if error.contains("could not be verified") {
                FailReason::BundleInvalid
            } else {
                FailReason::ImportFailed
            };
            options
                .ledger
                .fail_unless_landed(&manifest.move_id, reason, landed)
                .await
                .map_err(|e| e.to_string())?;
        }
    result
}
fn replace_predecessor(target: &Path, session_id: &str, session_dir: &Path) -> Result<(), String> {
    if !target.exists() {
        return Ok(());
    }
    let entries = super::entries(target);
    if header_session_id(&entries) != Some(session_id)
        || session_ownership(&entries) == SessionOwnership::Writable
    {
        return Err(format!(
            "move_target_occupied: a writable session already sits at {}",
            target.display()
        ));
    }
    let extensions = session_dir.join("extensions");
    if extensions.exists() {
        for entry in std::fs::read_dir(extensions).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                super::remove(&entry.path().join(format!(
                    "{}.json",
                    crate::session_sidecar_store::encoded_session_id(session_id)
                )))?;
            }
        }
    }
    super::remove(&session_dir.join("resident-blobs").join(session_id))?;
    super::remove(target)
}
pub fn stage_payloads(
    bundle_dir: &Path,
    manifest: &MoveManifest,
    staging: &Path,
) -> Result<(), String> {
    for file in manifest.payloads.iter().flat_map(|p| &p.files) {
        let to = staging.join(".payloads").join(&file.path);
        super::copy(&bundle_dir.join(&file.path), &to)?;
        if sha256_file(&to).map_err(|e| e.to_string())? != file.sha256 {
            return Err(
                MoveBundleInvalidError::new(format!("{} changed in transit", file.path))
                    .to_string(),
            );
        }
    }
    Ok(())
}
pub fn place_extension_state(
    staging: &Path,
    manifest: &MoveManifest,
    dir: &Path,
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
    super::remove(dir)?;
    for file in files {
        super::copy(
            &staging.join(".payloads").join(&file.path),
            &dir.join(
                file.path
                    .strip_prefix(EXTENSION_STATE_PREFIX)
                    .ok_or("invalid extension state path")?,
            ),
        )?;
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
    if !file.exists() || import_outcome(&file, &manifest.move_id) == LandingOutcome::Absent {
        super::remove(staging)?;
        super::copy(&bundle_dir.join(&manifest.session.path), &file)?;
        for item in &manifest.sidecars {
            super::copy(
                &bundle_dir.join(&item.path),
                &staging.join(item.path.strip_prefix("files/").unwrap_or(&item.path)),
            )?;
        }
        stage_payloads(bundle_dir, manifest, staging)?;
        super::append_custom(
            &file,
            SESSION_MOVED_IN,
            json!({"move_id":manifest.move_id,"session_id":manifest.session_id,"from":from,"bundle_sha256":manifest.session.sha256,"moved_in_at":crate::session_manager::now_iso_millis()}),
        )?;
    }
    super::requeue::requeue_input(&file, &manifest.move_id, &manifest.dropped_user_messages)
        .await?;
    Ok(file)
}
fn land(
    staged: &Path,
    target: &Path,
    cwd: &Path,
    manifest: &MoveManifest,
) -> Result<PathBuf, String> {
    let content = std::fs::read_to_string(staged).map_err(|e| e.to_string())?;
    let (first, rest) = content.split_once('\n').unwrap_or((&content, ""));
    let mut header: serde_json::Value = serde_json::from_str(first).map_err(|e| e.to_string())?;
    header["cwd"] = json!(cwd.to_string_lossy());
    let dir = target.parent().ok_or("missing target directory")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!(
        ".{}.rebind-{}.tmp",
        manifest.session_file_name,
        std::process::id()
    ));
    std::fs::write(&temp, format!("{header}\n{rest}")).map_err(|e| e.to_string())?;
    for item in &manifest.sidecars {
        let relative = item.path.strip_prefix("files/").unwrap_or(&item.path);
        super::copy(
            &staged
                .parent()
                .ok_or("missing staging directory")?
                .join(relative),
            &dir.join(relative),
        )?;
    }
    std::fs::rename(temp, target).map_err(|e| e.to_string())?;
    super::remove(staged)?;
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
async fn read_identity(dir: &Path) -> Result<Option<RepositoryIdentity>, String> {
    async fn git(dir: &Path, args: &[&str]) -> Result<Option<String>, String> {
        let mut command = tokio::process::Command::new("git");
        command.arg("-C").arg(dir).args(args).kill_on_drop(true);
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
        ] {
            command.env_remove(key);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), command.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
            }
            Ok(Ok(_)) | Err(_) => Ok(None),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Ok(Err(e)) => Err(e.to_string()),
        }
    }
    let roots = git(dir, &["rev-list", "--max-parents=0", "HEAD"])
        .await?
        .unwrap_or_default();
    let origin = git(dir, &["config", "--get", "remote.origin.url"]).await?;
    let mut root_commits: Vec<String> = roots
        .split_whitespace()
        .filter(|root| {
            (40..=64).contains(&root.len())
                && root
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
        .map(str::to_owned)
        .collect();
    root_commits.sort();
    let origin_url = origin
        .filter(|url| !url.trim().is_empty())
        .map(|url| normalize_remote_url(&url));
    Ok(
        (!root_commits.is_empty() || origin_url.is_some()).then_some(RepositoryIdentity {
            root_commits,
            origin_url,
        }),
    )
}
fn normalize_remote_url(url: &str) -> String {
    let trimmed = url.trim();
    let mut value = if !trimmed.contains("://") {
        let bare = trimmed.rsplit_once('@').map_or(trimmed, |(_, rest)| rest);
        bare.split_once(':')
            .filter(|(_, path)| !path.starts_with('/'))
            .map_or_else(|| bare.to_owned(), |(host, path)| format!("{host}/{path}"))
    } else {
        let bare = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
        let bare = bare.split_once('@').map_or(bare, |(_, rest)| rest);
        let (host, path) = bare.split_once('/').unwrap_or((bare, ""));
        let host = host
            .split_once(':')
            .filter(|(_, port)| port.bytes().all(|c| c.is_ascii_digit()))
            .map_or(host, |(host, _)| host);
        format!("{host}/{path}")
    }
    .trim_end_matches('/')
    .to_lowercase();
    if value.ends_with(".git") {
        value.truncate(value.len() - 4);
    }
    value
}
