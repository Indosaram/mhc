//! Typed senpi-task team spec errors.

/// Machine-readable code distinguishing the reserved-name rejection paths from schema and
/// member-vocabulary failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SenpiTeamSpecErrorCode {
    ReservedLeadField,
    ReservedLeadMember,
    ReservedCallerTeamLead,
    InvalidSpec,
    UnresolvableCategory,
    UnknownSubagentType,
}

impl SenpiTeamSpecErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReservedLeadField => "RESERVED_LEAD_FIELD",
            Self::ReservedLeadMember => "RESERVED_LEAD_MEMBER",
            Self::ReservedCallerTeamLead => "RESERVED_CALLER_TEAM_LEAD",
            Self::InvalidSpec => "INVALID_SPEC",
            Self::UnresolvableCategory => "UNRESOLVABLE_CATEGORY",
            Self::UnknownSubagentType => "UNKNOWN_SUBAGENT_TYPE",
        }
    }
}

impl std::fmt::Display for SenpiTeamSpecErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Raised when a senpi-task team spec cannot be normalized or validated. Carries a typed `code` so
/// callers can distinguish the three reserved-name rejection paths (raw `lead` field, member named
/// `lead`, or a `callerTeamLead` option) from schema and member-vocabulary failures. Every path that
/// returns this error spawns zero members.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SenpiTeamSpecError {
    pub message: String,
    pub code: SenpiTeamSpecErrorCode,
    pub team_name: String,
}

impl SenpiTeamSpecError {
    pub fn new(message: impl Into<String>, code: SenpiTeamSpecErrorCode, team_name: &str) -> Self {
        Self {
            message: message.into(),
            code,
            team_name: team_name.to_string(),
        }
    }

    pub fn name(&self) -> &'static str {
        "SenpiTeamSpecError"
    }
}
