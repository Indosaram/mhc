//! Port of `tools/team/messaging.ts`.

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use crate::team::messaging::types::{SendTeamMessageInput, SendTeamMessageResult};
use crate::tools::control::tool_result::{AgentToolResult, tool_result};
use crate::tools::team::classify_error::{MailboxErrorKind, classify_mailbox_error_name};
use crate::tools::team::types::{TeamToolServiceError, TeamToolsService};

/// Delivery outcome recorded for a single member recipient (always `enqueued`).
pub const MEMBER_DELIVERY_OUTCOME_ENQUEUED: &str = "enqueued";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamSendMemberView {
    pub member: String,
    pub outcome: &'static str,
}

/// Structured result details of a team send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamSendDetails {
    ToLead {
        message_id: String,
    },
    ToMembers {
        message_id: String,
        recipients: Vec<String>,
    },
    Mailbox {
        kind: MailboxErrorKind,
        to: String,
        reason: String,
    },
}

impl TeamSendDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ToLead { .. } => "to_lead",
            Self::ToMembers { .. } => "to_members",
            Self::Mailbox { kind, .. } => kind.as_str(),
        }
    }
}

impl Serialize for TeamSendDetails {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::ToLead { message_id } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("kind", "to_lead")?;
                map.serialize_entry("message_id", message_id)?;
                map.end()
            }
            Self::ToMembers { message_id, recipients } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("kind", "to_members")?;
                map.serialize_entry("message_id", message_id)?;
                map.serialize_entry("recipients", recipients)?;
                map.end()
            }
            Self::Mailbox { kind, to, reason } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("kind", kind.as_str())?;
                map.serialize_entry("to", to)?;
                map.serialize_entry("reason", reason)?;
                map.end()
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamSendInput {
    pub to: String,
    pub body: String,
    pub summary: Option<String>,
}

/// Sends a team message through the service, mapping classified mailbox failures to structured
/// results. Unclassified failures propagate as `Err`.
pub fn run_team_send(
    service: &dyn TeamToolsService,
    team_run_id: &str,
    from: &str,
    input: &TeamSendInput,
) -> Result<AgentToolResult<TeamSendDetails>, TeamToolServiceError> {
    let outcome = service.send_message(
        team_run_id,
        &SendTeamMessageInput {
            from: from.to_string(),
            to: input.to.clone(),
            body: input.body.clone(),
            summary: input.summary.clone(),
        },
    );
    match outcome {
        Ok(result) => {
            let message_id = result.message_id().to_string();
            if let SendTeamMessageResult::ToMembers { recipients, .. } = &result {
                return Ok(tool_result(
                    &format!(
                        "Message enqueued to {} recipient(s): {} (id: {message_id}).",
                        recipients.len(),
                        recipients.join(", ")
                    ),
                    TeamSendDetails::ToMembers {
                        message_id,
                        recipients: recipients.clone(),
                    },
                ));
            }
            Ok(tool_result(
                &format!("Message enqueued to lead (id: {message_id})."),
                TeamSendDetails::ToLead { message_id },
            ))
        }
        Err(error) => match classify_mailbox_error_name(&error.name) {
            Some(kind) => {
                let reason = error.message.clone();
                Ok(tool_result(
                    &reason,
                    TeamSendDetails::Mailbox {
                        kind,
                        to: input.to.clone(),
                        reason: reason.clone(),
                    },
                ))
            }
            None => Err(error),
        },
    }
}
