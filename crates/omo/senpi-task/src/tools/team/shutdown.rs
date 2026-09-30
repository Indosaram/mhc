//! Port of `tools/team/shutdown.ts`.
//!
//! Shutdown has no standalone tool registration: the model-facing surface is the structured-message
//! union on task_send. These input types describe the runner calls only.

use serde::Serialize;

use crate::team::shutdown::SenpiShutdownError;
use crate::tools::control::tool_result::{AgentToolResult, tool_result};
use crate::tools::team::types::{TeamToolServiceError, TeamToolsService};

const SHUTDOWN_ERROR_NAME: &str = "SenpiShutdownError";
const REJECT_REASON_ECHO_MAX: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamShutdownRequestInput {
    pub team_run_id: String,
    pub member: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamApproveShutdownInput {
    pub team_run_id: String,
    pub member: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRejectShutdownInput {
    pub team_run_id: String,
    pub member: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShutdownErrorView {
    UnknownMember { member: String, reason: String },
    NoPendingRequest { member: String, reason: String },
}

impl ShutdownErrorView {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UnknownMember { .. } => "unknown_member",
            Self::NoPendingRequest { .. } => "no_pending_request",
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::UnknownMember { reason, .. } | Self::NoPendingRequest { reason, .. } => reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamShutdownRequestDetails {
    Requested { team_run_id: String, member: String },
    UnknownMember { member: String, reason: String },
    NoPendingRequest { member: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamApproveShutdownDetails {
    Approved { team_run_id: String, member: String },
    UnknownMember { member: String, reason: String },
    NoPendingRequest { member: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamRejectShutdownDetails {
    Rejected {
        team_run_id: String,
        member: String,
        reason: String,
    },
    UnknownMember {
        member: String,
        reason: String,
    },
    NoPendingRequest {
        member: String,
        reason: String,
    },
}

macro_rules! impl_from_error_view {
    ($target:ident) => {
        impl From<ShutdownErrorView> for $target {
            fn from(view: ShutdownErrorView) -> Self {
                match view {
                    ShutdownErrorView::UnknownMember { member, reason } => Self::UnknownMember { member, reason },
                    ShutdownErrorView::NoPendingRequest { member, reason } => {
                        Self::NoPendingRequest { member, reason }
                    }
                }
            }
        }
    };
}

impl_from_error_view!(TeamShutdownRequestDetails);
impl_from_error_view!(TeamApproveShutdownDetails);
impl_from_error_view!(TeamRejectShutdownDetails);

impl TeamShutdownRequestDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Requested { .. } => "requested",
            Self::UnknownMember { .. } => "unknown_member",
            Self::NoPendingRequest { .. } => "no_pending_request",
        }
    }
}

impl TeamApproveShutdownDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Approved { .. } => "approved",
            Self::UnknownMember { .. } => "unknown_member",
            Self::NoPendingRequest { .. } => "no_pending_request",
        }
    }
}

impl TeamRejectShutdownDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Rejected { .. } => "rejected",
            Self::UnknownMember { .. } => "unknown_member",
            Self::NoPendingRequest { .. } => "no_pending_request",
        }
    }
}

/// Carries a `SenpiShutdownError` across the service seam (`{ name, message }`).
pub fn shutdown_error_to_service_error(error: &SenpiShutdownError) -> TeamToolServiceError {
    TeamToolServiceError::with_code(SHUTDOWN_ERROR_NAME, error.message.clone(), error.code.as_str())
}

// Maps the two lead-driven shutdown failures onto the shared error view; every other failure
// propagates. The member is the one the lead named (the TS `error.memberName`).
fn shutdown_error_view(error: TeamToolServiceError, member: &str) -> Result<ShutdownErrorView, TeamToolServiceError> {
    if error.name == SHUTDOWN_ERROR_NAME {
        let code = error.code.clone();
        if code.as_deref() == Some("unknown_member")
            || (code.is_none() && error.message.starts_with("unknown team member"))
        {
            return Ok(ShutdownErrorView::UnknownMember {
                member: member.to_string(),
                reason: error.message,
            });
        }
        if code.as_deref() == Some("no_pending_request")
            || (code.is_none() && error.message.starts_with("no pending shutdown request"))
        {
            return Ok(ShutdownErrorView::NoPendingRequest {
                member: member.to_string(),
                reason: error.message,
            });
        }
    }
    Err(error)
}

fn error_result<D: From<ShutdownErrorView>>(view: ShutdownErrorView) -> AgentToolResult<D> {
    let reason = view.reason().to_string();
    tool_result(&reason, D::from(view))
}

fn is_js_whitespace(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

// `reason.replace(/\s+/g, " ").trim().slice(0, 160)` with JS UTF-16 slicing.
fn echo_reason(reason: &str) -> String {
    let collapsed = reason
        .split(is_js_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut units = 0usize;
    let mut head = String::new();
    for c in collapsed.chars() {
        let width = c.len_utf16();
        if units + width > REJECT_REASON_ECHO_MAX {
            break;
        }
        units += width;
        head.push(c);
    }
    head
}

pub fn run_team_shutdown_request(
    service: &dyn TeamToolsService,
    params: &TeamShutdownRequestInput,
) -> Result<AgentToolResult<TeamShutdownRequestDetails>, TeamToolServiceError> {
    match service.request_shutdown(&params.team_run_id, &params.member) {
        Ok(_) => Ok(tool_result(
            &format!(
                "Requested shutdown for '{}' (team {}).",
                params.member, params.team_run_id
            ),
            TeamShutdownRequestDetails::Requested {
                team_run_id: params.team_run_id.clone(),
                member: params.member.clone(),
            },
        )),
        Err(error) => Ok(error_result(shutdown_error_view(error, &params.member)?)),
    }
}

pub fn run_team_approve_shutdown(
    service: &dyn TeamToolsService,
    params: &TeamApproveShutdownInput,
) -> Result<AgentToolResult<TeamApproveShutdownDetails>, TeamToolServiceError> {
    match service.approve_shutdown(&params.team_run_id, &params.member) {
        Ok(_) => Ok(tool_result(
            &format!(
                "Approved shutdown for '{}' (team {}).",
                params.member, params.team_run_id
            ),
            TeamApproveShutdownDetails::Approved {
                team_run_id: params.team_run_id.clone(),
                member: params.member.clone(),
            },
        )),
        Err(error) => Ok(error_result(shutdown_error_view(error, &params.member)?)),
    }
}

pub fn run_team_reject_shutdown(
    service: &dyn TeamToolsService,
    params: &TeamRejectShutdownInput,
) -> Result<AgentToolResult<TeamRejectShutdownDetails>, TeamToolServiceError> {
    match service.reject_shutdown(&params.team_run_id, &params.member, &params.reason) {
        Ok(_) => Ok(tool_result(
            &format!(
                "Rejected shutdown for '{}' (team {}): {}",
                params.member,
                params.team_run_id,
                echo_reason(&params.reason)
            ),
            TeamRejectShutdownDetails::Rejected {
                team_run_id: params.team_run_id.clone(),
                member: params.member.clone(),
                reason: params.reason.clone(),
            },
        )),
        Err(error) => Ok(error_result(shutdown_error_view(error, &params.member)?)),
    }
}
