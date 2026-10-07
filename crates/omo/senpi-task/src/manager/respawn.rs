//! Respawn of a persisted task onto a fresh child (`manager/manager-respawn.ts`).

use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use crate::lifecycle::RespawnResult;
use crate::lifecycle::host_session_resume_path;
use crate::lifecycle::port::{RespawnDisposition, RespawnFailureCode};
use crate::manager::ManagedChildHandle;
use crate::manager::child_handle::discard_managed_handle;
use crate::manager::helpers::{build_respawn_managed_spec, child_state_dir, is_terminal_record};
use crate::manager::interrupted_turn::session_tail_needs_continuation;
use crate::manager::types::{
    ManagedRunnerError, ManagedRunners, RpcRespawnRunner, TrustedRespawnLaunch,
    TrustedRespawnLaunchResolver,
};
use crate::runners::RunnerFailureKind;
use crate::runners::types::RpcRunnerSpec;
use crate::state::TaskRecord;

pub const CONTINUATION_MESSAGE: &str = "Your previous turn was interrupted by a host process restart. Resume your task from its current state and finish it - do not restart from scratch, and do not repeat work already recorded in this session.";
pub const RESPAWN_CLEANUP_FAILURE_REASON: &str = "rpc respawn cleanup failed";

pub struct RespawnInput<'a> {
    pub record: &'a TaskRecord,
    pub session_path: Option<&'a Path>,
    pub state_dir: &'a Path,
    pub runners: &'a ManagedRunners,
    pub rpc_runner: &'a dyn RpcRespawnRunner,
    pub trusted_launch: Option<&'a TrustedRespawnLaunchResolver>,
}

pub fn respawn_managed_task(input: &RespawnInput<'_>) -> RespawnResult {
    // A daemon-hosted child resumes its RECORDED session path; the recorded identity wins over the
    // caller-supplied path (TS manager-respawn.ts `hostSessionResumePath(record) ?? input.sessionPath`).
    let recorded_path = host_session_resume_path(input.record).map(std::path::PathBuf::from);
    match recorded_path.as_deref().or(input.session_path) {
        None => respawn_fresh(input),
        Some(path) if input.record.execution_mode == "in-process" => {
            respawn_in_process(input, path)
        }
        Some(path) => respawn_process(input, path),
    }
}

fn respawn_fresh(input: &RespawnInput<'_>) -> RespawnResult {
    let spec = match build_respawn_managed_spec(input.record, input.state_dir) {
        Ok(spec) => spec,
        Err(reason) => {
            return failure(
                RespawnDisposition::Unrecoverable,
                RespawnFailureCode::SpawnSpecUnavailable,
                reason,
            );
        }
    };
    if input.record.execution_mode == "in-process" {
        return match input.runners.in_process.start(&spec) {
            Ok(handle) => RespawnResult::Ok(handle),
            Err(error) => classify_resume_failure(&error),
        };
    }
    let started = trusted(input).and_then(|trusted| {
        input.rpc_runner.start(&rpc_spec(
            input.record,
            spec.cwd.clone(),
            spec.state_dir.clone(),
            spec.prompt.clone(),
            None,
            trusted,
        ))
    });
    match started {
        Ok(handle) => RespawnResult::Ok(handle),
        Err(error) => {
            if is_team_runtime_unavailable(&error) {
                return team_inactive();
            }
            log_failure("senpi-task fresh rpc respawn failed", input.record, &error);
            failure(
                RespawnDisposition::Retryable,
                RespawnFailureCode::RespawnFailed,
                "rpc respawn failed",
            )
        }
    }
}

fn respawn_in_process(input: &RespawnInput<'_>, session_path: &Path) -> RespawnResult {
    let spec = match build_respawn_managed_spec(input.record, input.state_dir) {
        Ok(spec) => spec,
        Err(reason) => {
            return failure(
                RespawnDisposition::Unrecoverable,
                RespawnFailureCode::SpawnSpecUnavailable,
                reason,
            );
        }
    };
    let Some(resumed) = input
        .runners
        .in_process
        .resume(&spec, &session_path.to_string_lossy())
    else {
        return failure(
            RespawnDisposition::Unrecoverable,
            RespawnFailureCode::RespawnFailed,
            "in-process runner cannot resume sessions",
        );
    };
    let handle = match resumed {
        Ok(handle) => handle,
        Err(error) => return classify_resume_failure(&error),
    };
    if let Err(error) = continue_interrupted_turn(input.record, session_path, handle.as_ref()) {
        if let Err(cleanup) = discard_managed_handle(handle.as_ref()) {
            utils::logger::log(
                "senpi-task in-process respawn cleanup failed",
                Some(&json!({ "taskId": input.record.task_id, "error": cleanup.to_string() })),
            );
        }
        return classify_resume_failure(&ManagedRunnerError::Other(error.to_string()));
    }
    RespawnResult::Ok(handle)
}

fn respawn_process(input: &RespawnInput<'_>, session_path: &Path) -> RespawnResult {
    let Some(spawn_spec) = input.record.spawn_spec.as_ref() else {
        return failure(
            RespawnDisposition::Unrecoverable,
            RespawnFailureCode::SpawnSpecUnavailable,
            "persisted spawn spec unavailable",
        );
    };
    let cwd = match spawn_spec {
        crate::state::TaskSpawnSpec::LegacyProcess { cwd, .. } => cwd.clone(),
        crate::state::TaskSpawnSpec::V1(spec) => spec.cwd.clone(),
    };
    let session = session_path.to_string_lossy().into_owned();
    let started = trusted(input).and_then(|trusted| {
        input.rpc_runner.start(&rpc_spec(
            input.record,
            cwd,
            child_state_dir(input.state_dir, &input.record.task_id),
            String::new(),
            Some(session.clone()),
            trusted,
        ))
    });
    let handle = match started {
        Ok(handle) => handle,
        Err(error) => {
            if is_team_runtime_unavailable(&error) {
                return team_inactive();
            }
            log_failure("senpi-task rpc respawn failed", input.record, &error);
            return failure(
                RespawnDisposition::Retryable,
                RespawnFailureCode::RespawnFailed,
                "rpc respawn failed",
            );
        }
    };
    let switched = match handle.switch_session(&session) {
        None => return cleanup_failure(&handle, "respawned RPC handle cannot switch sessions"),
        Some(Ok(result)) => result,
        Some(Err(error)) => {
            return rpc_failure_after_start(input.record, &handle, &error.to_string());
        }
    };
    if switched.cancelled {
        return cleanup_failure(&handle, "switch_session was cancelled");
    }
    if let Err(error) = continue_interrupted_turn(input.record, session_path, handle.as_ref()) {
        return rpc_failure_after_start(input.record, &handle, &error.to_string());
    }
    RespawnResult::Ok(handle)
}

fn rpc_failure_after_start(
    record: &TaskRecord,
    handle: &Arc<dyn ManagedChildHandle>,
    error: &str,
) -> RespawnResult {
    let cleaned = dispose_rpc(handle);
    utils::logger::log(
        "senpi-task rpc respawn failed",
        Some(&json!({ "taskId": record.task_id, "error": error })),
    );
    failure(
        RespawnDisposition::Retryable,
        RespawnFailureCode::RespawnFailed,
        if cleaned {
            "rpc respawn failed"
        } else {
            RESPAWN_CLEANUP_FAILURE_REASON
        },
    )
}

fn trusted(input: &RespawnInput<'_>) -> Result<Option<TrustedRespawnLaunch>, ManagedRunnerError> {
    match input.trusted_launch {
        None => Ok(None),
        Some(resolve) => resolve(input.record),
    }
}

fn rpc_spec(
    record: &TaskRecord,
    cwd: String,
    state_dir: String,
    prompt: String,
    resume_session_path: Option<String>,
    trusted: Option<TrustedRespawnLaunch>,
) -> RpcRunnerSpec {
    let trusted = trusted.unwrap_or_default();
    RpcRunnerSpec {
        task_id: record.task_id.clone(),
        cwd,
        state_dir,
        prompt,
        resume_session_path,
        model: Some(record.model.clone()),
        variant: record
            .resolved_model
            .as_ref()
            .and_then(|model| model.variant.clone()),
        extensions: trusted.extensions,
        member_env: trusted.member_env,
        ..RpcRunnerSpec::default()
    }
}

fn continue_interrupted_turn(
    record: &TaskRecord,
    session_path: &Path,
    handle: &dyn ManagedChildHandle,
) -> Result<(), crate::host::HostError> {
    if !is_terminal_record(record) && session_tail_needs_continuation(session_path) {
        handle.follow_up(CONTINUATION_MESSAGE)?;
    }
    Ok(())
}

fn cleanup_failure(handle: &Arc<dyn ManagedChildHandle>, reason: &str) -> RespawnResult {
    let reason = if dispose_rpc(handle) {
        reason
    } else {
        RESPAWN_CLEANUP_FAILURE_REASON
    };
    failure(
        RespawnDisposition::Retryable,
        RespawnFailureCode::RespawnFailed,
        reason,
    )
}

fn dispose_rpc(handle: &Arc<dyn ManagedChildHandle>) -> bool {
    match discard_managed_handle(handle.as_ref()) {
        Ok(()) => true,
        Err(error) => {
            utils::logger::log(
                "senpi-task failed respawn cleanup rejected",
                Some(&json!({ "taskId": handle.task_id(), "error": error.to_string() })),
            );
            false
        }
    }
}

fn classify_resume_failure(error: &ManagedRunnerError) -> RespawnResult {
    if let ManagedRunnerError::Runner(failure_value) = error {
        let code = match failure_value.kind {
            RunnerFailureKind::ModelUnavailable => Some(RespawnFailureCode::ModelUnavailable),
            RunnerFailureKind::ToolsUnavailable => Some(RespawnFailureCode::ToolsUnavailable),
            RunnerFailureKind::SessionUnavailable => Some(RespawnFailureCode::SessionUnavailable),
            _ => None,
        };
        if let Some(code) = code {
            return failure(RespawnDisposition::Retryable, code, &failure_value.message);
        }
    }
    failure(
        RespawnDisposition::Retryable,
        RespawnFailureCode::RespawnFailed,
        "in-process respawn failed",
    )
}

fn is_team_runtime_unavailable(error: &ManagedRunnerError) -> bool {
    matches!(
        error,
        ManagedRunnerError::TeamRespawnLaunch { code }
            if matches!(
                code.as_str(),
                "runtime_unavailable" | "runtime_inactive" | "member_missing" | "task_mapping_mismatch"
            )
    )
}

fn team_inactive() -> RespawnResult {
    failure(
        RespawnDisposition::Retryable,
        RespawnFailureCode::TeamInactive,
        "team runtime is not active",
    )
}

fn log_failure(message: &str, record: &TaskRecord, error: &ManagedRunnerError) {
    utils::logger::log(
        message,
        Some(&json!({ "taskId": record.task_id, "error": error.to_string() })),
    );
}

fn failure(
    disposition: RespawnDisposition,
    code: RespawnFailureCode,
    reason: &str,
) -> RespawnResult {
    RespawnResult::Failed {
        disposition,
        code,
        reason: reason.to_string(),
    }
}
