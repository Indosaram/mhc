//! Port of `tools/control/send-shutdown.ts`.
//!
//! Routes structured shutdown protocol messages (`shutdown_request` / `shutdown_response`) sent via
//! `task_send` to the lead team tools service, mapping known shutdown failures to structured results.

use std::sync::Arc;

use crate::team::normalize::TEAM_LEAD_SENTINEL;
use crate::team::shutdown::SenpiShutdownErrorCode;
use crate::tools::control::send_results::{SendInvariantError, invalid_arguments};
use crate::tools::control::send_schema::{StructuredMessageInput, TaskSendInput};
use crate::tools::control::tool_result::tool_result;
use crate::tools::control::types::{SendResultDetails, SendToolResult, ShutdownFailedCode, ShutdownOperation};
use crate::tools::team::types::{TeamToolServiceError, TeamToolsService};

/// Outcome of the wiring's default team run id resolver (single-owned-team defaulting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultTeamRunIdResolution {
    Resolved { team_run_id: String },
    None,
    Ambiguous { reason: String },
}

impl DefaultTeamRunIdResolution {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Resolved { .. } => "resolved",
            Self::None => "none",
            Self::Ambiguous { .. } => "ambiguous",
        }
    }
}

pub type ResolveDefaultTeamRunIdFn = Arc<dyn Fn() -> DefaultTeamRunIdResolution + Send + Sync>;

/// Team routing available to `task_send`: the service, the sender identity, an optional bound team
/// run id, and an optional default team run id resolver.
#[derive(Clone)]
pub struct TaskSendTeamRouting {
    pub service: Arc<dyn TeamToolsService>,
    pub from: String,
    pub team_run_id: Option<String>,
    pub resolve_default_team_run_id: Option<ResolveDefaultTeamRunIdFn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendTeamRunIdResolution {
    Resolved { team_run_id: String },
    None,
    Error { reason: String },
}

impl SendTeamRunIdResolution {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Resolved { .. } => "resolved",
            Self::None => "none",
            Self::Error { .. } => "error",
        }
    }
}

/// Failures `task_send` propagates instead of mapping to a structured result (the TS rethrows).
#[derive(Debug, thiserror::Error)]
pub enum TaskSendError {
    #[error(transparent)]
    Service(#[from] TeamToolServiceError),
    #[error("{0}")]
    Manager(String),
    #[error(transparent)]
    Invariant(#[from] SendInvariantError),
}

// Explicit ids (bound routing first, then the param) always win. Without one, the wiring's default
// resolver (single-owned-team defaulting) is consulted; without a resolver the historical
// team_run_id-required error is preserved so un-wired adapters behave exactly as before.
pub fn resolve_send_team_run_id(params: &TaskSendInput, team_routing: &TaskSendTeamRouting) -> SendTeamRunIdResolution {
    if let Some(explicit) = team_routing.team_run_id.as_ref().or(params.team_run_id.as_ref()) {
        return SendTeamRunIdResolution::Resolved {
            team_run_id: explicit.clone(),
        };
    }
    let Some(resolver) = &team_routing.resolve_default_team_run_id else {
        return SendTeamRunIdResolution::Error {
            reason: "team_run_id is required to message a team member".to_string(),
        };
    };
    match resolver() {
        DefaultTeamRunIdResolution::Resolved { team_run_id } => SendTeamRunIdResolution::Resolved { team_run_id },
        DefaultTeamRunIdResolution::None => SendTeamRunIdResolution::None,
        DefaultTeamRunIdResolution::Ambiguous { reason } => SendTeamRunIdResolution::Error { reason },
    }
}

/// The TS `error instanceof SenpiShutdownError` / `isMissingStateError(error)` checks. Service errors
/// cross the seam as `{ name, message }`, so the shutdown code is recovered from the stable
/// `SenpiShutdownError` messages and a missing state is recognised by its ENOENT signature.
pub fn classify_shutdown_service_error(error: &TeamToolServiceError) -> Option<ShutdownFailedCode> {
    if error.name == "SenpiShutdownError" {
        if error.message.starts_with("unknown team member") {
            return Some(ShutdownFailedCode::Shutdown(SenpiShutdownErrorCode::UnknownMember));
        }
        if error.message.starts_with("no pending shutdown request") {
            return Some(ShutdownFailedCode::Shutdown(SenpiShutdownErrorCode::NoPendingRequest));
        }
    }
    if is_missing_state_service_error(error) {
        return Some(ShutdownFailedCode::TeamStateMissing);
    }
    None
}

fn is_missing_state_service_error(error: &TeamToolServiceError) -> bool {
    error.name == "ENOENT" || error.message.contains("ENOENT") || error.message.contains("No such file or directory")
}

pub fn route_structured_message(
    to: &str,
    message: &StructuredMessageInput,
    params: &TaskSendInput,
    team_routing: Option<&TaskSendTeamRouting>,
) -> Result<SendToolResult, TaskSendError> {
    let Some(team_routing) = team_routing else {
        return Ok(invalid_arguments("not in a team"));
    };
    if team_routing.from != TEAM_LEAD_SENTINEL {
        return Ok(invalid_arguments("shutdown is lead-only"));
    }

    let run_id = match resolve_send_team_run_id(params, team_routing) {
        SendTeamRunIdResolution::None => return Ok(invalid_arguments("not in a team")),
        SendTeamRunIdResolution::Error { reason } => return Ok(invalid_arguments(&reason)),
        SendTeamRunIdResolution::Resolved { team_run_id } => team_run_id,
    };
    let service = team_routing.service.as_ref();

    match message {
        StructuredMessageInput::ShutdownRequest { .. } => {
            if let Err(error) = service.request_shutdown(&run_id, to) {
                return shutdown_failure(error, context(ShutdownOperation::Request, &run_id, to));
            }
            Ok(tool_result(
                &format!("Shutdown requested for {to} (team {run_id})."),
                SendResultDetails::ShutdownRequested {
                    team_run_id: run_id.clone(),
                    member: to.to_string(),
                },
            ))
        }
        StructuredMessageInput::ShutdownResponse { approve: true, .. } => {
            if let Err(error) = service.approve_shutdown(&run_id, to) {
                return shutdown_failure(error, context(ShutdownOperation::Approve, &run_id, to));
            }
            Ok(tool_result(
                &format!("Shutdown approved for {to} (team {run_id})."),
                SendResultDetails::ShutdownResponded {
                    team_run_id: run_id.clone(),
                    member: to.to_string(),
                    approved: true,
                },
            ))
        }
        StructuredMessageInput::ShutdownResponse { reason, .. } => {
            let Some(reason) = reason.as_deref().filter(|reason| !reason.trim().is_empty()) else {
                return Ok(invalid_arguments("reason is required when rejecting a shutdown"));
            };
            if let Err(error) = service.reject_shutdown(&run_id, to, reason) {
                return shutdown_failure(error, context(ShutdownOperation::Reject, &run_id, to));
            }
            Ok(tool_result(
                &format!("Shutdown rejected for {to} (team {run_id})."),
                SendResultDetails::ShutdownResponded {
                    team_run_id: run_id.clone(),
                    member: to.to_string(),
                    approved: false,
                },
            ))
        }
    }
}

struct ShutdownFailureContext {
    operation: ShutdownOperation,
    team_run_id: String,
    member: String,
}

fn context(operation: ShutdownOperation, team_run_id: &str, member: &str) -> ShutdownFailureContext {
    ShutdownFailureContext {
        operation,
        team_run_id: team_run_id.to_string(),
        member: member.to_string(),
    }
}

fn shutdown_failure(
    error: TeamToolServiceError,
    context: ShutdownFailureContext,
) -> Result<SendToolResult, TaskSendError> {
    let Some(code) = classify_shutdown_service_error(&error) else {
        return Err(error.into());
    };
    let reason = shutdown_failure_reason(code);
    Ok(tool_result(
        &format!(
            "Shutdown {} failed for {}: {reason}",
            context.operation.as_str(),
            context.member
        ),
        SendResultDetails::ShutdownFailed {
            operation: context.operation,
            team_run_id: context.team_run_id,
            member: context.member,
            code,
            reason: reason.to_string(),
        },
    ))
}

fn shutdown_failure_reason(code: ShutdownFailedCode) -> &'static str {
    match code {
        ShutdownFailedCode::TeamStateMissing => "Team state is unavailable.",
        ShutdownFailedCode::Shutdown(SenpiShutdownErrorCode::UnknownMember) => "Team member is unavailable.",
        ShutdownFailedCode::Shutdown(SenpiShutdownErrorCode::NoPendingRequest) => {
            "No pending shutdown request exists."
        }
    }
}
