//! Shared messaging-engine types.

use crate::store::StateDirConfig;
use crate::team::messaging::lead_poller_types::AppendEventFn;
use crate::team::runtime_config::TeamCoreConfig;

pub use crate::team::member_map::MemberTaskMap;
pub use team_core::types::Message;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendTeamMessageInput {
    pub from: String,
    /// A member name, the reserved lead sentinel "lead", or "*" for a lead broadcast to every member.
    pub to: String,
    pub body: String,
    pub summary: Option<String>,
}

pub struct MessagingEngineDeps {
    pub team_run_id: String,
    pub state_dir: StateDirConfig,
    pub config: TeamCoreConfig,
    pub active_members: Vec<String>,
    pub append_event: Option<AppendEventFn>,
    pub now: Option<Box<dyn Fn() -> i64 + Send + Sync>>,
    pub new_message_id: Option<Box<dyn Fn() -> String + Send + Sync>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendTeamMessageResult {
    ToLead { message_id: String },
    ToMembers { message_id: String, recipients: Vec<String> },
}

impl SendTeamMessageResult {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ToLead { .. } => "to_lead",
            Self::ToMembers { .. } => "to_members",
        }
    }

    pub fn message_id(&self) -> &str {
        match self {
            Self::ToLead { message_id } | Self::ToMembers { message_id, .. } => message_id,
        }
    }
}
