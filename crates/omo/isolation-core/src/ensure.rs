use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backend::{
    platform_name, resolve_candidates, BackendKind, IsolationContext, IsolationError, Result,
    StartDetail, BACKEND_FILE,
};
use crate::backend_marker::mark_started;
use crate::backends::btrfs::remove_dir_all_force;
use crate::base_dir::{choose_base_dir, FilesystemBaseDirIo};
use crate::git::command::{exists, git, git_result, str_args};
use crate::git::detach_git_dir::{detach_git_dir, scan_nested_git_dirs, DetachOutcome, NestedGitResult};
use crate::owner::{write_owner_marker, IsolationOwner};
use crate::process_identity::get_process_start_identity;
use crate::sweep::BackendRef;
use crate::util::{dirname, home_dir, now_ms, random_hex};

pub type StopFn = Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>;
pub type HandleRelocateFn = Arc<dyn Fn(&Path, &Path) -> Result<()> + Send + Sync>;

pub struct IsolationHandle {
    pub merged_dir: PathBuf,
    pub base_dir: PathBuf,
    pub backend: BackendKind,
    pub fell_back: bool,
    pub fallback_reason: Option<String>,
    pub strategy_detail: Option<String>,
    pub nested_git_rewritten: Vec<String>,
    pub nested_git_skipped: Vec<String>,
    stop: StopFn,
    relocate: HandleRelocateFn,
}

impl IsolationHandle {
    pub fn stop(&self, merged_dir: &Path) -> Result<()> {
        (self.stop)(merged_dir)
    }

    pub fn relocate(&self, from: &Path, to: &Path) -> Result<()> {
        (self.relocate)(from, to)
    }
}

pub struct EnsureIsolationOptions {
    pub repo_root: PathBuf,
    pub id: String,
    pub preferred: Option<BackendKind>,
    pub backends: Vec<BackendRef>,
    pub platform: Option<String>,
    pub home_dir: Option<PathBuf>,
    pub owner: Option<IsolationOwner>,
    pub max_copy_bytes: Option<u64>,
}

pub fn relocate_sandbox(backend: &dyn crate::backend::IsolationBackend, from: &Path, to: &Path) -> Result<()> {
    backend.relocate(from, to)
}

fn write_backend_only(base: &Path, kind: BackendKind) -> Result<()> {
    let text = serde_json::json!({ "backend": kind.as_str() }).to_string();
    std::fs::write(base.join(BACKEND_FILE), text)?;
    Ok(())
}

pub fn ensure_isolation(options: EnsureIsolationOptions) -> Result<IsolationHandle> {
    let repo_root = options.repo_root.clone();
    let id = options.id.clone();
    let home = options.home_dir.clone().unwrap_or_else(home_dir);
    let owner = options.owner.clone().unwrap_or_default();
    let selection = choose_base_dir(&repo_root, &home, &id, &FilesystemBaseDirIo)?;
    let base_dir = selection.base_dir;
    let cross_device = selection.cross_device;
    let platform = options
        .platform
        .clone()
        .unwrap_or_else(|| platform_name().to_string());
    let candidates = resolve_candidates(&platform, options.preferred).candidates;
    let creating = PathBuf::from(format!(
        "{}.creating-{}",
        base_dir.to_string_lossy(),
        std::process::id()
    ));
    let merged = creating.join("m");
    if exists(&base_dir)? {
        return Err(IsolationError::exists(format!(
            "Isolation already exists: {}",
            base_dir.display()
        )));
    }
    let mut fallback_reason: Option<String> = None;
    for (index, kind) in candidates.iter().enumerate() {
        let Some(backend) = options.backends.iter().find(|backend| backend.kind() == *kind) else {
            if fallback_reason.is_none() {
                fallback_reason = Some(format!("No implementation for {}", kind.as_str()));
            }
            continue;
        };
        let context = IsolationContext {
            id: id.clone(),
            base_dir: creating.clone(),
            cross_device,
            max_copy_bytes: options.max_copy_bytes,
        };
        let probe = match backend.probe(&repo_root, Some(&context)) {
            Ok(probe) => probe,
            Err(error) if error.is_unavailable() => {
                if fallback_reason.is_none() {
                    fallback_reason = Some(error.to_string());
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        let availability = (|| -> Result<()> {
            if !probe.available {
                return Err(IsolationError::unavailable(
                    probe
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("{} is unavailable", kind.as_str())),
                ));
            }
            if cross_device && backend.clones_tree() {
                return Err(IsolationError::unavailable(format!(
                    "{} requires the same device",
                    kind.as_str()
                )));
            }
            Ok(())
        })();
        if let Err(error) = availability {
            if fallback_reason.is_none() {
                fallback_reason = Some(error.to_string());
            }
            continue;
        }
        std::fs::create_dir_all(dirname(&base_dir))?;
        // Exclusive creation prevents another ensure in this process from deleting an active clone.
        std::fs::create_dir(&creating)?;
        let mut start_attempted = false;
        let attempt = (|| -> Result<(Option<StartDetail>, NestedGitResult)> {
            write_owner_marker(&creating, &id, &owner, &get_process_start_identity)?;
            write_backend_only(&creating, *kind)?;
            start_attempted = true;
            let mut detail: Option<StartDetail> = None;
            let mut nested = NestedGitResult::default();
            let common = if exists(&repo_root.join(".git"))? {
                String::from_utf8_lossy(&git(
                    &repo_root,
                    &str_args(&[
                        "rev-parse",
                        "--path-format=absolute",
                        "--git-common-dir",
                    ]),
                    None,
                )?)
                .trim()
                .to_string()
            } else {
                repo_root.join(".git").to_string_lossy().into_owned()
            };
            for attempt in 0..2 {
                detail = backend.start(&repo_root, &merged, &context)?;
                mark_started(&creating, *kind, &[])?;
                let detached = detach_git_dir(&merged, Path::new(&common))?;
                nested = scan_nested_git_dirs(&merged)?;
                if detached == DetachOutcome::NoGit {
                    break;
                }
                let status = git_result(&merged, &str_args(&["status", "--porcelain"]), None)?;
                if status.code == 0 {
                    break;
                }
                if attempt == 1 {
                    return Err(IsolationError::other(format!(
                        "Git snapshot inconsistent after retry: {}",
                        status.stderr
                    )));
                }
                backend.stop(&merged)?;
                // Mount backends may remove the whole base during teardown.
                std::fs::create_dir_all(&creating)?;
                write_owner_marker(&creating, &id, &owner, &get_process_start_identity)?;
                write_backend_only(&creating, *kind)?;
            }
            backend.relocate(&creating, &base_dir)?;
            Ok((detail, nested))
        })();
        match attempt {
            Ok((detail, nested)) => {
                let stop_backend = Arc::clone(backend);
                let relocate_backend = Arc::clone(backend);
                return Ok(IsolationHandle {
                    merged_dir: base_dir.join("m"),
                    base_dir,
                    backend: *kind,
                    fell_back: index > 0,
                    fallback_reason,
                    strategy_detail: detail.map(|detail| detail.strategy_detail),
                    nested_git_rewritten: nested.nested_git_rewritten,
                    nested_git_skipped: nested.nested_git_skipped,
                    stop: Arc::new(move |path| stop_backend.stop(path)),
                    relocate: Arc::new(move |from, to| relocate_backend.relocate(from, to)),
                });
            }
            Err(error) => {
                if start_attempted {
                    if let Err(stop_error) = backend.stop(&merged) {
                        return Err(IsolationError::other(format!(
                            "Isolation teardown failed: {}: {error}; {stop_error}",
                            creating.display()
                        )));
                    }
                }
                let _ = std::fs::remove_dir_all(&creating);
                if !error.is_unavailable() {
                    return Err(error);
                }
                if fallback_reason.is_none() {
                    fallback_reason = Some(error.to_string());
                }
            }
        }
    }
    Err(IsolationError::unavailable(
        fallback_reason.unwrap_or_else(|| "No isolation backend is available".to_string()),
    ))
}

pub fn cleanup_isolation(handle: &IsolationHandle) -> Result<()> {
    handle.stop(&handle.merged_dir)?;
    remove_dir_all_force(&handle.base_dir)
}

pub fn retain_isolation(handle: &IsolationHandle, reason: &str) -> Result<PathBuf> {
    let retained = PathBuf::from(format!(
        "{}.retained-{}-{}",
        handle.base_dir.to_string_lossy(),
        now_ms(),
        random_hex(6)
    ));
    std::fs::write(
        handle.base_dir.join(".omo-isolation-retained.json"),
        serde_json::json!({ "reason": reason }).to_string(),
    )?;
    handle.relocate(&handle.base_dir, &retained)?;
    Ok(retained)
}
