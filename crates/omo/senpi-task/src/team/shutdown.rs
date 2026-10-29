//! Lead-driven shutdown protocol over the team-core runtime state.

use serde_json::{Map, Value, json};
use team_core::TeamCoreError;
use team_core::team_state_store::{load_runtime_state, save_runtime_state};
use team_core::types::RuntimeState;

use crate::team::normalize::TEAM_LEAD_SENTINEL;
use crate::team::runtime_config::TeamCoreConfig;
use crate::team::shutdown_helpers::{find_latest_shutdown_request_index, find_runtime_member, is_unresolved_request};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SenpiShutdownErrorCode {
    UnknownMember,
    NoPendingRequest,
}

impl SenpiShutdownErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownMember => "unknown_member",
            Self::NoPendingRequest => "no_pending_request",
        }
    }
}

impl std::fmt::Display for SenpiShutdownErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Raised by the shutdown protocol for the two lead-driven failure modes: a request/approval/rejection
/// that names a member the team never spawned, and an approve/reject issued when the member has no
/// outstanding shutdown request. Carries the team run and member in play for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SenpiShutdownError {
    pub message: String,
    pub code: SenpiShutdownErrorCode,
    pub team_run_id: String,
    pub member_name: String,
}

impl SenpiShutdownError {
    pub fn new(message: impl Into<String>, code: SenpiShutdownErrorCode, team_run_id: &str, member_name: &str) -> Self {
        Self {
            message: message.into(),
            code,
            team_run_id: team_run_id.to_string(),
            member_name: member_name.to_string(),
        }
    }

    pub fn name(&self) -> &'static str {
        "SenpiShutdownError"
    }
}

/// Any failure surfaced by the shutdown protocol.
#[derive(Debug, thiserror::Error)]
pub enum ShutdownFailure {
    #[error(transparent)]
    Shutdown(#[from] SenpiShutdownError),
    #[error(transparent)]
    Core(#[from] TeamCoreError),
    #[error("{0}")]
    Transport(String),
    #[error("{0}")]
    State(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShutdownMessageKind {
    ShutdownRequest,
    ShutdownApproved,
    ShutdownRejected,
}

impl ShutdownMessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShutdownRequest => "shutdown_request",
            Self::ShutdownApproved => "shutdown_approved",
            Self::ShutdownRejected => "shutdown_rejected",
        }
    }
}

/// A single shutdown-protocol message the lead emits toward a member. The transport is injected so
/// the shutdown protocol stays decoupled from the messaging layer and testable in isolation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownOutboundMessage {
    pub to: String,
    pub kind: ShutdownMessageKind,
    pub body: String,
}

pub type ShutdownMessenger<'a> = &'a dyn Fn(&ShutdownOutboundMessage) -> Result<(), String>;
pub type CancelMemberTask<'a> = &'a dyn Fn(&str) -> Result<(), String>;

pub struct RequestShutdownDeps<'a> {
    pub config: &'a TeamCoreConfig,
    pub send_message: ShutdownMessenger<'a>,
    pub now: Option<&'a dyn Fn() -> i64>,
}

pub struct ApproveShutdownDeps<'a> {
    pub config: &'a TeamCoreConfig,
    pub send_message: ShutdownMessenger<'a>,
    pub now: Option<&'a dyn Fn() -> i64>,
    pub cancel_member_task: CancelMemberTask<'a>,
}

pub type RejectShutdownDeps<'a> = RequestShutdownDeps<'a>;

fn current_time(now: Option<&dyn Fn() -> i64>) -> i64 {
    match now {
        Some(now) => now(),
        None => chrono::Utc::now().timestamp_millis(),
    }
}

fn require_member(state: &RuntimeState, team_run_id: &str, member_name: &str) -> Result<(), SenpiShutdownError> {
    if find_runtime_member(state, member_name).is_none() {
        return Err(SenpiShutdownError::new(
            format!("unknown team member '{member_name}'"),
            SenpiShutdownErrorCode::UnknownMember,
            team_run_id,
            member_name,
        ));
    }
    Ok(())
}

fn require_pending_request_index(
    state: &RuntimeState,
    team_run_id: &str,
    member_name: &str,
) -> Result<usize, SenpiShutdownError> {
    match find_latest_shutdown_request_index(state, member_name) {
        Some(index) if is_unresolved_request(state.shutdown_requests.get(index)) => Ok(index),
        _ => Err(SenpiShutdownError::new(
            format!("no pending shutdown request for '{member_name}'"),
            SenpiShutdownErrorCode::NoPendingRequest,
            team_run_id,
            member_name,
        )),
    }
}

fn send(deps_send: ShutdownMessenger<'_>, message: ShutdownOutboundMessage) -> Result<(), ShutdownFailure> {
    deps_send(&message).map_err(ShutdownFailure::Transport)
}

/// Loads the current runtime state, lets `mutate` edit its JSON form (returning whether anything
/// changed), then re-parses and persists it. An unchanged state is returned as-is without writing.
fn transition_state(
    team_run_id: &str,
    config: &TeamCoreConfig,
    mutate: impl FnOnce(&RuntimeState, &mut Map<String, Value>) -> bool,
) -> Result<RuntimeState, ShutdownFailure> {
    let current = load_runtime_state(team_run_id, config)?;
    let mut value = serde_json::to_value(&current).map_err(|error| ShutdownFailure::State(error.to_string()))?;
    let Some(record) = value.as_object_mut() else {
        return Err(ShutdownFailure::State("runtime state is not an object".to_string()));
    };
    if !mutate(&current, record) {
        return Ok(current);
    }
    let next = RuntimeState::safe_parse(&value).map_err(|issues| {
        ShutdownFailure::State(
            issues
                .first()
                .map(|issue| issue.message.clone())
                .unwrap_or_else(|| "Invalid runtime state".to_string()),
        )
    })?;
    save_runtime_state(&next, config)?;
    Ok(next)
}

fn shutdown_requests_mut(record: &mut Map<String, Value>) -> Option<&mut Vec<Value>> {
    record
        .entry("shutdownRequests")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
}

/// Lead requests a member's shutdown: sends a `shutdown_request` message to the member, then records
/// an unresolved shutdown request keyed to the lead sentinel. Idempotent while a request is pending,
/// and a resolved (approved/rejected) prior request never blocks a fresh one.
pub fn request_shutdown(
    team_run_id: &str,
    member_name: &str,
    deps: &RequestShutdownDeps<'_>,
) -> Result<RuntimeState, ShutdownFailure> {
    let state = load_runtime_state(team_run_id, deps.config)?;
    require_member(&state, team_run_id, member_name)?;

    let existing = find_latest_shutdown_request_index(&state, member_name);
    if is_unresolved_request(existing.and_then(|index| state.shutdown_requests.get(index))) {
        return Ok(state);
    }

    send(
        deps.send_message,
        ShutdownOutboundMessage {
            to: member_name.to_string(),
            kind: ShutdownMessageKind::ShutdownRequest,
            body: String::new(),
        },
    )?;

    let requested_at = current_time(deps.now);
    transition_state(team_run_id, deps.config, |current, record| {
        let current_index = find_latest_shutdown_request_index(current, member_name);
        if is_unresolved_request(current_index.and_then(|index| current.shutdown_requests.get(index))) {
            return false;
        }
        let Some(requests) = shutdown_requests_mut(record) else {
            return false;
        };
        requests.push(json!({
            "memberId": member_name,
            "requesterName": TEAM_LEAD_SENTINEL,
            "requestedAt": requested_at,
        }));
        true
    })
}

/// Lead approves a pending shutdown: marks the member `shutdown_approved` (unless it already finished
/// or errored, whose terminal status is preserved), stamps the request approved, cancels the member's
/// background task, then notifies the member. Idempotent once the request is already approved.
pub fn approve_shutdown(
    team_run_id: &str,
    member_name: &str,
    deps: &ApproveShutdownDeps<'_>,
) -> Result<RuntimeState, ShutdownFailure> {
    let state = load_runtime_state(team_run_id, deps.config)?;
    require_member(&state, team_run_id, member_name)?;
    let request_index = require_pending_request_index(&state, team_run_id, member_name)?;
    if state
        .shutdown_requests
        .get(request_index)
        .is_some_and(|request| request.approved_at.is_some())
    {
        return Ok(state);
    }

    let approved_at = current_time(deps.now);
    let updated = transition_state(team_run_id, deps.config, |current, record| {
        let current_index = find_latest_shutdown_request_index(current, member_name);
        if let Some(members) = record.get_mut("members").and_then(Value::as_array_mut) {
            for member in members.iter_mut() {
                let Some(member) = member.as_object_mut() else {
                    continue;
                };
                if member.get("name").and_then(Value::as_str) != Some(member_name) {
                    continue;
                }
                let status = member.get("status").and_then(Value::as_str);
                if status == Some("completed") || status == Some("errored") {
                    continue;
                }
                member.insert("status".to_string(), Value::String("shutdown_approved".to_string()));
            }
        }
        if let (Some(index), Some(requests)) = (current_index, shutdown_requests_mut(record))
            && let Some(request) = requests.get_mut(index).and_then(Value::as_object_mut)
        {
            request.insert("approvedAt".to_string(), json!(approved_at));
        }
        true
    })?;

    (deps.cancel_member_task)(member_name).map_err(ShutdownFailure::Transport)?;
    send(
        deps.send_message,
        ShutdownOutboundMessage {
            to: member_name.to_string(),
            kind: ShutdownMessageKind::ShutdownApproved,
            body: member_name.to_string(),
        },
    )?;
    Ok(updated)
}

/// Lead rejects a pending shutdown: notifies the member with the reason (keep working), then records
/// the rejection on the request while leaving the member's live status untouched. Idempotent when the
/// latest request already carries the same rejection reason.
pub fn reject_shutdown(
    team_run_id: &str,
    member_name: &str,
    reason: &str,
    deps: &RejectShutdownDeps<'_>,
) -> Result<RuntimeState, ShutdownFailure> {
    let state = load_runtime_state(team_run_id, deps.config)?;
    require_member(&state, team_run_id, member_name)?;
    let request_index = require_pending_request_index(&state, team_run_id, member_name)?;

    if let Some(existing) = state.shutdown_requests.get(request_index) {
        let existing_reason = serde_json::to_value(existing)
            .ok()
            .and_then(|value| value.get("rejectedReason").and_then(Value::as_str).map(str::to_string));
        if existing.rejected_at.is_some() && existing_reason.as_deref() == Some(reason) {
            return Ok(state);
        }
    }

    send(
        deps.send_message,
        ShutdownOutboundMessage {
            to: member_name.to_string(),
            kind: ShutdownMessageKind::ShutdownRejected,
            body: reason.to_string(),
        },
    )?;

    let rejected_at = current_time(deps.now);
    transition_state(team_run_id, deps.config, |current, record| {
        let current_index = find_latest_shutdown_request_index(current, member_name);
        if let (Some(index), Some(requests)) = (current_index, shutdown_requests_mut(record))
            && let Some(request) = requests.get_mut(index).and_then(Value::as_object_mut)
        {
            request.insert("rejectedAt".to_string(), json!(rejected_at));
            request.insert("rejectedReason".to_string(), Value::String(reason.to_string()));
        }
        true
    })
}
