//! Port of `tools/control/types.ts`.

use std::sync::Arc;

use serde::Serialize;
use serde::ser::Serializer;

use crate::manager::TaskManager;
use crate::manager::types::ListScope;
use crate::state::{DeliverAs, TaskStatus};
use crate::steering::{
    CancelOptions, CancelOutcome as SteeringCancelOutcome, SendInput as SteeringSendInput,
    SendOutcome as SteeringSendOutcome,
};
use crate::team::shutdown::SenpiShutdownErrorCode;
use crate::tools::control::tool_result::AgentToolResult;
use crate::tools::team::messaging::TeamSendDetails;

/// `ctx.sessionManager` view: only the session id read.
pub trait SessionIdSource {
    fn get_session_id(&self) -> String;
}

/// The minimal read of the harness context the control tools need to name the calling session.
pub trait SessionIdCarrier {
    fn session_manager(&self) -> &dyn SessionIdSource;
}

pub type CallerSessionResolver = Arc<dyn Fn(&dyn SessionIdCarrier) -> Option<String> + Send + Sync>;

/// Scope passed to the manager `list` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlListScope {
    All,
    ParentSession { session_id: String },
}

/// The narrow listed-task record view the control tools read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlTaskRecord {
    pub task_id: String,
    pub name: Option<String>,
    pub parent_session_id: String,
    pub root_session_id: String,
}

/// Input for the manager `send_to_task` call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlSendInput {
    pub id_or_name: String,
    pub message: String,
    pub caller_session_id: Option<String>,
    pub all_scope: Option<bool>,
}

/// Outcome of the manager `send_to_task` call.
#[derive(Debug, Clone, PartialEq)]
pub enum SendOutcome {
    Steered {
        task_id: String,
        status: TaskStatus,
        delivered: String,
    },
    Revived {
        task_id: String,
        run_epoch: u64,
    },
    Queued {
        task_id: String,
        queue_position: usize,
    },
    NotContinuable {
        task_id: String,
        reason: String,
        suggestion: String,
    },
    OneShotAgent {
        task_id: String,
        agent: String,
        message: String,
    },
    ScopeDenied {
        task_id: String,
        owning_session_id: String,
        reason: String,
    },
    NotFound {
        reason: String,
    },
}

/// The engine surface the send tool drives (`Pick<TaskManager, "sendToTask" | "list">`).
pub trait SendManager: Send + Sync {
    fn send_to_task(&self, input: &ControlSendInput) -> Result<SendOutcome, String>;
    fn list(&self, scope: &ControlListScope) -> Vec<ControlTaskRecord>;
}

/// Outcome of the manager `cancel_task` call.
#[derive(Debug, Clone, PartialEq)]
pub enum CancelOutcome {
    Cancelled {
        task_id: String,
        previous_status: TaskStatus,
    },
    Noop {
        task_id: String,
        status: TaskStatus,
        reason: String,
    },
    NotFound {
        reason: String,
    },
}

/// The engine surface the cancel tool drives (`Pick<TaskManager, "cancelTask" | "get">`).
pub trait CancelManager: Send + Sync {
    fn cancel_task(&self, id_or_name: &str, reason: Option<&str>) -> CancelOutcome;
    /// `manager.get(task_id)?.status`.
    fn get_status(&self, task_id: &str) -> Option<TaskStatus>;
}

/// The TS tools take the task manager itself (`Pick<TaskManager, ...>`); these impls are that pick.
impl SendManager for TaskManager {
    fn send_to_task(&self, input: &ControlSendInput) -> Result<SendOutcome, String> {
        let outcome = TaskManager::send_to_task(
            self,
            &SteeringSendInput {
                id_or_name: input.id_or_name.clone(),
                message: input.message.clone(),
                deliver_as: Some(DeliverAs::Steer),
                caller_session_id: input.caller_session_id.clone(),
                all_scope: input.all_scope == Some(true),
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(match outcome {
            SteeringSendOutcome::Steered {
                task_id,
                status,
                delivered,
            } => SendOutcome::Steered {
                task_id,
                status,
                delivered: match delivered {
                    DeliverAs::Steer => "steer",
                    DeliverAs::FollowUp => "followUp",
                }
                .to_string(),
            },
            SteeringSendOutcome::Revived { task_id, run_epoch } => SendOutcome::Revived {
                task_id,
                run_epoch: u64::try_from(run_epoch).unwrap_or_default(),
            },
            SteeringSendOutcome::Queued {
                task_id,
                queue_position,
            } => SendOutcome::Queued {
                task_id,
                queue_position,
            },
            SteeringSendOutcome::NotContinuable {
                task_id,
                reason,
                suggestion,
            } => SendOutcome::NotContinuable {
                task_id,
                reason,
                suggestion,
            },
            SteeringSendOutcome::OneShotAgent {
                task_id,
                agent,
                message,
            } => SendOutcome::OneShotAgent {
                task_id,
                agent,
                message,
            },
            SteeringSendOutcome::ScopeDenied {
                task_id,
                owning_session_id,
                reason,
            } => SendOutcome::ScopeDenied {
                task_id,
                owning_session_id,
                reason,
            },
            SteeringSendOutcome::NotFound { reason, .. } => SendOutcome::NotFound { reason },
        })
    }

    fn list(&self, scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        let scope = match scope {
            ControlListScope::All => ListScope::All,
            ControlListScope::ParentSession { session_id } => ListScope::ParentSession(session_id.clone()),
        };
        TaskManager::list(self, &scope)
            .into_iter()
            .map(|listed| ControlTaskRecord {
                task_id: listed.record.task_id,
                name: listed.record.name,
                parent_session_id: listed.record.parent_session_id,
                root_session_id: listed.record.root_session_id,
            })
            .collect()
    }
}

impl CancelManager for TaskManager {
    fn cancel_task(&self, id_or_name: &str, reason: Option<&str>) -> CancelOutcome {
        match TaskManager::cancel_task(self, id_or_name, reason, CancelOptions::default()) {
            Ok(SteeringCancelOutcome::Cancelled {
                task_id,
                previous_status,
            }) => CancelOutcome::Cancelled {
                task_id,
                previous_status,
            },
            Ok(SteeringCancelOutcome::Noop {
                task_id,
                status,
                reason,
            }) => CancelOutcome::Noop {
                task_id,
                status,
                reason,
            },
            Ok(SteeringCancelOutcome::NotFound { reason }) => CancelOutcome::NotFound { reason },
            Err(error) => CancelOutcome::NotFound {
                reason: error.to_string(),
            },
        }
    }

    fn get_status(&self, task_id: &str) -> Option<TaskStatus> {
        self.get(task_id).map(|record| record.status)
    }
}

pub fn serialize_task_status<S: Serializer>(status: &TaskStatus, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(status.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShutdownOperation {
    Request,
    Approve,
    Reject,
}

impl ShutdownOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Approve => "approve",
            Self::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownFailedCode {
    Shutdown(SenpiShutdownErrorCode),
    TeamStateMissing,
}

impl ShutdownFailedCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shutdown(code) => code.as_str(),
            Self::TeamStateMissing => "team_state_missing",
        }
    }
}

impl Serialize for ShutdownFailedCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SendResultDetails {
    Steered {
        task_id: String,
        #[serde(serialize_with = "serialize_task_status")]
        status: TaskStatus,
        delivered: String,
    },
    Revived {
        task_id: String,
        run_epoch: u64,
    },
    Queued {
        task_id: String,
        queue_position: usize,
    },
    NotContinuable {
        task_id: String,
        reason: String,
        suggestion: String,
    },
    OneShotAgent {
        task_id: String,
        agent: String,
        message: String,
    },
    ScopeDenied {
        task_id: String,
        owning_session_id: String,
        reason: String,
    },
    NotFound {
        reason: String,
        known_tasks: Vec<String>,
    },
    InvalidArguments {
        reason: String,
    },
    TeamMessage {
        team: TeamSendDetails,
    },
    ShutdownRequested {
        team_run_id: String,
        member: String,
    },
    ShutdownResponded {
        team_run_id: String,
        member: String,
        approved: bool,
    },
    ShutdownFailed {
        operation: ShutdownOperation,
        team_run_id: String,
        member: String,
        code: ShutdownFailedCode,
        reason: String,
    },
}

impl SendResultDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Steered { .. } => "steered",
            Self::Revived { .. } => "revived",
            Self::Queued { .. } => "queued",
            Self::NotContinuable { .. } => "not_continuable",
            Self::OneShotAgent { .. } => "one_shot_agent",
            Self::ScopeDenied { .. } => "scope_denied",
            Self::NotFound { .. } => "not_found",
            Self::InvalidArguments { .. } => "invalid_arguments",
            Self::TeamMessage { .. } => "team_message",
            Self::ShutdownRequested { .. } => "shutdown_requested",
            Self::ShutdownResponded { .. } => "shutdown_responded",
            Self::ShutdownFailed { .. } => "shutdown_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CancelResultDetails {
    Cancelled {
        task_id: String,
        #[serde(serialize_with = "serialize_task_status")]
        previous_status: TaskStatus,
        #[serde(serialize_with = "serialize_task_status")]
        status: TaskStatus,
    },
    Noop {
        task_id: String,
        #[serde(serialize_with = "serialize_task_status")]
        status: TaskStatus,
        reason: String,
    },
    NotFound {
        reason: String,
    },
    InvalidArguments {
        reason: String,
    },
}

impl CancelResultDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cancelled { .. } => "cancelled",
            Self::Noop { .. } => "noop",
            Self::NotFound { .. } => "not_found",
            Self::InvalidArguments { .. } => "invalid_arguments",
        }
    }
}

pub type SendToolResult = AgentToolResult<SendResultDetails>;
pub type CancelToolResult = AgentToolResult<CancelResultDetails>;
