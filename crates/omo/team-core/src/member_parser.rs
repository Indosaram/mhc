//! `createParseMember`: member parsing with human-oriented validation errors.

use serde_json::Value;

use crate::error::TeamCoreError;
use crate::types::{Member, agent_eligibility};

/// Failure of [`crate::types::parse_member`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MemberParseError {
    /// `MemberValidationError`.
    #[error("{message}")]
    Validation {
        message: String,
        member_name: Option<String>,
        issue: Option<String>,
    },
    /// A plain `Error` carrying an agent's hard-reject message.
    #[error("{0}")]
    Rejected(String),
}

impl From<MemberParseError> for TeamCoreError {
    fn from(error: MemberParseError) -> Self {
        match error {
            MemberParseError::Validation {
                message,
                member_name,
                issue,
            } => Self::MemberValidation {
                message,
                member_name,
                issue,
            },
            MemberParseError::Rejected(message) => Self::Message(message),
        }
    }
}

fn validation(message: String, name: &str, issue: &str) -> MemberParseError {
    MemberParseError::Validation {
        message,
        member_name: Some(name.to_owned()),
        issue: Some(issue.to_owned()),
    }
}

fn present(value: Option<&Value>) -> bool {
    value.is_some_and(|value| !value.is_null())
}

fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn translate_member_error(input: &serde_json::Map<String, Value>) -> MemberParseError {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unnamed>");
    let has_category = present(input.get("category"));
    let has_subagent_type = present(input.get("subagent_type"));
    let kind = input.get("kind").and_then(Value::as_str);
    let has_kind = matches!(kind, Some("category" | "subagent_type"));

    if has_category && has_subagent_type {
        return validation(
            format!(
                "Member '{name}' specifies both 'category' and 'subagent_type'. Must specify exactly one via 'kind' discriminator."
            ),
            name,
            "both-kinds",
        );
    }
    if !has_kind && !has_category && !has_subagent_type {
        return validation(
            format!(
                "Member '{name}' missing 'kind' discriminator. Specify either {{kind:'category', category, prompt}} or {{kind:'subagent_type', subagent_type}}."
            ),
            name,
            "missing-kind",
        );
    }
    if kind == Some("category") || (!has_kind && has_category) {
        let category = input
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        return validation(
            format!(
                "Member '{name}' uses category '{category}' but is missing required 'prompt' field. Category members must supply a task prompt."
            ),
            name,
            "category-missing-prompt",
        );
    }
    if kind == Some("subagent_type") || (!has_kind && has_subagent_type) {
        let raw = input.get("subagent_type");
        let known = raw
            .and_then(Value::as_str)
            .is_some_and(|agent| agent_eligibility(agent).is_some());
        if !known {
            return validation(
                format!(
                    "Unknown subagent_type '{}'. Available ELIGIBLE agents: sisyphus, atlas, sisyphus-junior, hephaestus (if D-36 applied). Use delegate-task for read-only agents like oracle, librarian, explore, metis, momus, multimodal-looker.",
                    js_string(raw)
                ),
                name,
                "unknown-subagent",
            );
        }
    }
    validation(
        format!("Member '{name}' validation failed."),
        name,
        "zod-residual",
    )
}

pub(crate) fn parse_member_base(input: &Value) -> Result<Member, MemberParseError> {
    let Some(raw) = input.as_object() else {
        return Err(MemberParseError::Validation {
            message: "Member must be an object".to_owned(),
            member_name: None,
            issue: None,
        });
    };
    let mut candidate = raw.clone();
    if !raw.contains_key("kind")
        && (raw.contains_key("category") || raw.contains_key("subagent_type"))
    {
        let kind = if raw.contains_key("category") {
            "category"
        } else {
            "subagent_type"
        };
        candidate.insert("kind".to_owned(), Value::String(kind.to_owned()));
    }
    Member::safe_parse(&Value::Object(candidate)).map_err(|_| translate_member_error(raw))
}
