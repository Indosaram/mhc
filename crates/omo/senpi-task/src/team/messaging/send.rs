//! Durable team message send: writes to recipient inbox(es) and returns.

use serde_json::json;
use team_core::TeamCoreError;
use team_core::team_mailbox::{SendContext, send_message};
use team_core::types::SchemaIssues;

use crate::team::member_map::read_member_task_map;
use crate::team::messaging::lead_poller_types::TeamTaskEvent;
use crate::team::messaging::message::{BuildTeamMessageOptions, build_team_message};
use crate::team::messaging::types::{MessagingEngineDeps, SendTeamMessageInput, SendTeamMessageResult};
use crate::team::normalize::TEAM_LEAD_SENTINEL;
use crate::team::storage::resolve_team_runtime_dirs;

#[derive(Debug, thiserror::Error)]
pub enum SendTeamMessageError {
    /// Keeps the stable `InvalidRecipientError` name so the tool layer maps it to invalid_recipient.
    #[error("{message}")]
    InvalidRecipient {
        message: String,
        #[source]
        cause: TeamCoreError,
    },
    #[error("{0}")]
    InvalidMessage(String),
    #[error(transparent)]
    Core(#[from] TeamCoreError),
}

impl SendTeamMessageError {
    pub fn name(&self) -> &'static str {
        match self {
            Self::InvalidRecipient { .. } => "InvalidRecipientError",
            Self::InvalidMessage(_) => "ZodError",
            Self::Core(error) => error.name(),
        }
    }
}

fn describe_issues(issues: &SchemaIssues) -> String {
    match issues.first() {
        Some(issue) => {
            let path = issue.path.join(".");
            if path.is_empty() {
                issue.message.clone()
            } else {
                format!("{path}: {}", issue.message)
            }
        }
        None => "Invalid input".to_string(),
    }
}

fn as_now(now: &(dyn Fn() -> i64 + Send + Sync)) -> &dyn Fn() -> i64 {
    now
}

fn as_new_message_id(new_message_id: &(dyn Fn() -> String + Send + Sync)) -> &dyn Fn() -> String {
    new_message_id
}

/// Writes a message to the durable recipient inbox(es) and returns. Recipient-owned pollers perform
/// delivery later, so the send path never reserves, reads, steers, revives, or notifies. Broadcast
/// ("*") remains lead-only, and the lead sentinel is a real inbox recipient.
pub fn send_team_message(
    input: &SendTeamMessageInput,
    deps: &MessagingEngineDeps,
) -> Result<SendTeamMessageResult, SendTeamMessageError> {
    let message_options = BuildTeamMessageOptions {
        now: deps.now.as_deref().map(as_now),
        new_message_id: deps.new_message_id.as_deref().map(as_new_message_id),
    };
    let message = build_team_message(input, &message_options)
        .map_err(|issues| SendTeamMessageError::InvalidMessage(describe_issues(&issues)))?;
    let runtime_dir = resolve_team_runtime_dirs(&deps.state_dir, &deps.team_run_id)?.runtime_dir;
    let member_task_map = read_member_task_map(&runtime_dir);
    let is_lead = input.from == TEAM_LEAD_SENTINEL;
    let to_lead = input.to == TEAM_LEAD_SENTINEL;

    let context = SendContext {
        is_lead,
        active_members: deps.active_members.clone(),
        lead_recipient: to_lead.then(|| TEAM_LEAD_SENTINEL.to_string()),
        reserved_recipients: Default::default(),
    };
    let sent = send_message(&message, &deps.team_run_id, &deps.config, &context).map_err(|error| {
        // team-core does not re-export InvalidRecipientError; key on its stable name.
        if error.name() == "InvalidRecipientError" {
            let mut members = deps.active_members.clone();
            members.sort();
            SendTeamMessageError::InvalidRecipient {
                message: format!(
                    "unknown or inactive team recipient: {}. Members: {}. Use a member name, '{TEAM_LEAD_SENTINEL}', or '*' (lead-only broadcast).",
                    input.to,
                    members.join(", ")
                ),
                cause: error,
            }
        } else {
            SendTeamMessageError::Core(error)
        }
    })?;

    let event = TeamTaskEvent {
        event_type: "team_message_sent".to_string(),
        payload: json!({
            "message_id": message.message_id,
            "from": message.from,
            "to": message.to,
            "kind": message.kind.as_str(),
        }),
    };
    if let Some(append_event) = deps.append_event.as_ref() {
        if is_lead {
            for recipient in &sent.delivered_to {
                if let Some(task_id) = member_task_map.get(recipient) {
                    append_event(task_id, event.clone());
                }
            }
        } else if let Some(task_id) = member_task_map.get(&input.from) {
            append_event(task_id, event);
        }
    }

    Ok(if to_lead {
        SendTeamMessageResult::ToLead {
            message_id: sent.message_id,
        }
    } else {
        SendTeamMessageResult::ToMembers {
            message_id: sent.message_id,
            recipients: sent.delivered_to,
        }
    })
}
