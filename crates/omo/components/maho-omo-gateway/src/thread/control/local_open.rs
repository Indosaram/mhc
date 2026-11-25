//! Local session open & git worktree lifecycle (`thread/control/local-open.ts`).

use std::path::Path;

use serde_json::{Value, json};

use super::super::contracts_results::ThreadDataError;
use super::super::tools::ports::{OpenSessionParams, ThreadHost};
use super::super::worktree::discard_worktree::{DiscardWorktreeOptions, discard_worktree};
use super::super::worktree::failures::{FolderFailureReason, folder_failure};
use super::super::worktree::thread_worktree::{
    WorktreeOutcome, check_folder, create_thread_worktree, run_git,
};
use super::open_reply::model_refusal;
use super::session_host::{HostedThread, SessionOpenOutcome, SessionOpenRequest, opened_thread};

fn open_params(request: &SessionOpenRequest, cwd_override: Option<String>) -> OpenSessionParams {
    OpenSessionParams {
        cwd: cwd_override.or_else(|| request.cwd.clone()),
        session_path: request.session_path.clone(),
        name: request.name.clone(),
        fork_from: request.fork_from.clone(),
        initial_skills: request.initial_skills.clone(),
        permission_preset: request.permission_preset.clone(),
        provider: request.provider.clone(),
        model_id: request.model_id.clone(),
        thinking_level: request.thinking_level.clone(),
        durable_session_id: request.durable_session_id.clone(),
    }
}

pub async fn open_locally(
    host: &dyn ThreadHost,
    request: SessionOpenRequest,
    worktree_root: &Path,
) -> Result<SessionOpenOutcome, String> {
    if let Some(cwd) = request.cwd.as_deref()
        && let Some(err) = check_folder(Path::new(cwd))
    {
        return Ok(SessionOpenOutcome::Error(
            serde_json::to_value(err).unwrap_or_default(),
        ));
    }
    let model = request
        .provider
        .as_deref()
        .map(|p| format!("{p}/{}", request.model_id.as_deref().unwrap_or("")));
    let Some(worktree) = request.worktree.as_ref() else {
        return match host.open_session(open_params(&request, None)).await {
            Ok(session) => Ok(SessionOpenOutcome::Ok(HostedThread {
                summary: opened_thread(&session),
                worktree: None,
                warnings: None,
            })),
            Err(err) => {
                if let Some(refused) = model_refusal(&err, model.as_deref(), None) {
                    Ok(SessionOpenOutcome::Error(refused))
                } else {
                    Err(err)
                }
            }
        };
    };

    let Some(cwd) = request.cwd.as_deref() else {
        let err = folder_failure(
            FolderFailureReason::RelativePath,
            "A worktree needs the repository folder; pass cwd.",
            None,
        );
        return Ok(SessionOpenOutcome::Error(
            serde_json::to_value(err).unwrap_or_default(),
        ));
    };

    let made = match create_thread_worktree(Path::new(cwd), worktree, worktree_root, None).await {
        WorktreeOutcome::Ok(wt) => wt,
        WorktreeOutcome::Error(err) => {
            return Ok(SessionOpenOutcome::Error(
                serde_json::to_value(err).unwrap_or_default(),
            ));
        }
    };

    match host
        .open_session(open_params(&request, Some(made.path.clone())))
        .await
    {
        Ok(session) => Ok(SessionOpenOutcome::Ok(HostedThread {
            summary: opened_thread(&session),
            worktree: Some(made),
            warnings: None,
        })),
        Err(err) => {
            let git = run_git();
            let left = discard_worktree(DiscardWorktreeOptions {
                repository: Path::new(&made.repository),
                path: Path::new(&made.path),
                branch: &made.branch,
                root: worktree_root,
                git: &git,
                created_folder: None,
                remove_unregistered_folder: true,
            })
            .await;
            match left {
                None => {
                    if let Some(refused) = model_refusal(&err, model.as_deref(), None) {
                        Ok(SessionOpenOutcome::Error(refused))
                    } else {
                        Err(err)
                    }
                }
                Some(ThreadDataError::Error { mut error }) => {
                    let mut details = error
                        .details
                        .as_ref()
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    details.insert("cause".into(), Value::String(err));
                    error.details = Some(Value::Object(details));
                    Ok(SessionOpenOutcome::Error(
                        json!({ "kind": "error", "error": error }),
                    ))
                }
            }
        }
    }
}
