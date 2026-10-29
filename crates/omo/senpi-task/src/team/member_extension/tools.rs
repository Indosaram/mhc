//! Port of `team/member-extension/tools.ts`.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use team_core::TeamCoreError;
use team_core::config::TeamModeConfig;
use team_core::team_mailbox::{SendContext, send_message};

use crate::team::member_extension::self_poller::TeamMessageEvent;
use crate::team::messaging::message::{BuildTeamMessageOptions, build_team_message};
use crate::team::messaging::types::SendTeamMessageInput;
use crate::team::normalize::TEAM_LEAD_SENTINEL;
use crate::tools::control::tool_result::{AgentToolResult, tool_result};

pub const MEMBER_TASK_SEND_TOOL_NAME: &str = "task_send";
pub const MEMBER_TASK_SEND_TOOL_LABEL: &str = "Task Send";
pub const MEMBER_TASK_SEND_TOOL_DESCRIPTION: &str =
    "Send a durable message to another team member or the team lead.";

/// JSON schema of `MemberTaskSendParams` (typebox `Type.Object`).
pub fn member_task_send_params() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": { "type": "string", "description": "Recipient member name or lead." },
            "message": { "type": "string", "description": "Message body." },
            "summary": { "type": "string", "description": "Optional short summary." },
        },
        "required": ["to", "message"],
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberTaskSendInput {
    pub to: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemberTaskSendDetails {
    pub kind: &'static str,
    pub message_id: String,
    pub to: String,
}

pub type NowFn = Box<dyn Fn() -> i64 + Send + Sync>;
pub type NewMessageIdFn = Box<dyn Fn() -> String + Send + Sync>;
pub type AppendTaskEventFn = Box<dyn Fn(&str, TeamMessageEvent) + Send + Sync>;

pub struct MemberTaskSendDeps {
    pub team_run_id: String,
    pub member_name: String,
    pub task_id: String,
    pub config: TeamModeConfig,
    pub members: Vec<String>,
    pub append_event: Option<AppendTaskEventFn>,
    pub now: Option<NowFn>,
    pub new_message_id: Option<NewMessageIdFn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMemberRecipientError {
    pub recipient: String,
    pub message: String,
}

impl UnknownMemberRecipientError {
    pub fn new(recipient: &str, members: &[String]) -> Self {
        let mut valid: Vec<&str> = members.iter().map(String::as_str).collect();
        valid.push(TEAM_LEAD_SENTINEL);
        valid.sort_unstable();
        Self {
            recipient: recipient.to_string(),
            message: format!(
                "Unknown team recipient: {recipient}. Valid recipients: {}.",
                valid.join(", ")
            ),
        }
    }

    pub fn name(&self) -> &'static str {
        "UnknownMemberRecipientError"
    }
}

impl fmt::Display for UnknownMemberRecipientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UnknownMemberRecipientError {}

#[derive(Debug, thiserror::Error)]
pub enum MemberTaskSendError {
    #[error(transparent)]
    UnknownRecipient(#[from] UnknownMemberRecipientError),
    #[error("{0}")]
    InvalidMessage(String),
    #[error(transparent)]
    TeamCore(#[from] TeamCoreError),
}

pub fn run_member_task_send(
    deps: &MemberTaskSendDeps,
    input: &MemberTaskSendInput,
) -> Result<AgentToolResult<MemberTaskSendDetails>, MemberTaskSendError> {
    let mut recipients: HashSet<&str> = deps.members.iter().map(String::as_str).collect();
    recipients.insert(TEAM_LEAD_SENTINEL);
    if !recipients.contains(input.to.as_str()) {
        return Err(UnknownMemberRecipientError::new(&input.to, &deps.members).into());
    }

    let send_input = SendTeamMessageInput {
        from: deps.member_name.clone(),
        to: input.to.clone(),
        body: input.message.clone(),
        summary: input.summary.clone(),
    };
    let options = BuildTeamMessageOptions {
        now: deps.now.as_ref().map(|now| now.as_ref() as &dyn Fn() -> i64),
        new_message_id: deps
            .new_message_id
            .as_ref()
            .map(|new_message_id| new_message_id.as_ref() as &dyn Fn() -> String),
    };
    let message = build_team_message(&send_input, &options).map_err(|issues| {
        MemberTaskSendError::InvalidMessage(
            issues
                .first()
                .map(|issue| issue.message.clone())
                .unwrap_or_else(|| "Invalid input".to_string()),
        )
    })?;

    let context = SendContext {
        is_lead: false,
        active_members: deps.members.clone(),
        lead_recipient: Some(TEAM_LEAD_SENTINEL.to_string()),
        reserved_recipients: Default::default(),
    };
    send_message(&message, &deps.team_run_id, &deps.config, &context)?;

    if let Some(append_event) = deps.append_event.as_ref() {
        append_event(
            &deps.task_id,
            TeamMessageEvent::for_message("team_message_sent", &message),
        );
    }
    Ok(tool_result(
        &format!("Message enqueued to {} (id: {}).", input.to, message.message_id),
        MemberTaskSendDetails {
            kind: "team_message",
            message_id: message.message_id.clone(),
            to: input.to.clone(),
        },
    ))
}

/// Minimal model of senpi's `ToolDefinition` for the member `task_send` tool.
pub struct MemberTaskSendTool {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    deps: MemberTaskSendDeps,
}

impl MemberTaskSendTool {
    pub fn execute(
        &self,
        _tool_call_id: &str,
        params: &MemberTaskSendInput,
    ) -> Result<AgentToolResult<MemberTaskSendDetails>, MemberTaskSendError> {
        run_member_task_send(&self.deps, params)
    }

    pub fn deps(&self) -> &MemberTaskSendDeps {
        &self.deps
    }
}

pub fn create_member_task_send_tool(deps: MemberTaskSendDeps) -> MemberTaskSendTool {
    MemberTaskSendTool {
        name: MEMBER_TASK_SEND_TOOL_NAME,
        label: MEMBER_TASK_SEND_TOOL_LABEL,
        description: MEMBER_TASK_SEND_TOOL_DESCRIPTION,
        parameters: member_task_send_params(),
        deps,
    }
}
