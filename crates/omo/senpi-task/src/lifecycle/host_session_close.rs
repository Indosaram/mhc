//! Port of `lifecycle/host-session-close.ts`: end a daemon session this process holds no handle for.
//!
//! Only `closed` is confirmation: a refused attach or close_session is not, and the caller must keep
//! whatever points at the session (a record, a cleanup obligation) and start nothing in its place.

use std::sync::Arc;

use super::context::LifecycleContext;
use crate::state::HostSessionIdentity;
use crate::store::PersistedTaskEvent;

/// How a close this process asked for ended. The TS `pending` state (the daemon had not answered
/// within `hostCloseTimeoutMs`) collapses natively because the transport close is synchronous: it
/// either confirms or refuses, so there is no in-flight close to model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSessionCloseOutcome {
    Closed,
    Refused,
}

impl HostSessionCloseOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Refused => "refused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("host session close was not confirmed: {message}")]
pub struct HostSessionCloseError {
    pub message: String,
}

/// The close request handed to the host-owned transport (TS HostSessionCloseRequest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSessionCloseRequest {
    pub host_session: HostSessionIdentity,
    pub cwd: Option<String>,
}

/// THE single close writer's seam (`runners/rpc-host/close`): abort + close_session, no signal.
pub type HostSessionCloser =
    Arc<dyn Fn(&HostSessionCloseRequest) -> Result<(), HostSessionCloseError> + Send + Sync>;

/// End a daemon session. Only a confirmed close records the `host_session_closed` event; a failure to
/// record it must not turn a confirmed close into a refusal.
pub fn close_host_session(
    context: &LifecycleContext,
    task_id: &str,
    host_session: &HostSessionIdentity,
    cwd: Option<&str>,
) -> HostSessionCloseOutcome {
    let Some(close) = context.host_session_close.as_ref() else {
        return HostSessionCloseOutcome::Refused;
    };
    let request = HostSessionCloseRequest {
        host_session: host_session.clone(),
        cwd: cwd.map(str::to_string),
    };
    match close(&request) {
        Ok(()) => {
            let event = PersistedTaskEvent {
                event_type: "host_session_closed".to_string(),
                payload: serde_json::json!({
                    "session_path": host_session.session_path,
                    "socket": host_session.socket,
                }),
            };
            if let Err(error) = context.store.append_event(task_id, &event) {
                utils::logger::log(
                    "senpi-task could not record a confirmed host session close",
                    Some(&serde_json::json!({ "taskId": task_id, "error": error.to_string() })),
                );
            }
            HostSessionCloseOutcome::Closed
        }
        Err(error) => {
            utils::logger::log(
                "senpi-task host session close not confirmed",
                Some(&serde_json::json!({ "taskId": task_id, "error": error.to_string() })),
            );
            HostSessionCloseOutcome::Refused
        }
    }
}

pub fn close_host_session_confirmed(
    context: &LifecycleContext,
    task_id: &str,
    host_session: &HostSessionIdentity,
    cwd: Option<&str>,
) -> bool {
    close_host_session(context, task_id, host_session, cwd) == HostSessionCloseOutcome::Closed
}
