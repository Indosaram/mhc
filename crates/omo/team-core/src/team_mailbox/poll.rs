use std::cell::RefCell;

use crate::config::TeamModeConfig;
use crate::error::{Result, TeamCoreError};
use crate::team_state_store::store::transition_runtime_state;
use crate::types::{Message, RuntimeState};

use super::inbox::list_unread_messages;

/// `InjectionResult`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InjectionResult {
    pub injected: bool,
    pub content: Option<String>,
    pub message_ids: Vec<String>,
    pub reason: Option<String>,
}

impl InjectionResult {
    fn skipped(reason: &str) -> Self {
        Self {
            injected: false,
            content: None,
            message_ids: Vec::new(),
            reason: Some(reason.to_owned()),
        }
    }
}

fn escape_attribute_value(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
}

/// `buildEnvelope`: the `<peer_message ...>` wrapper injected into a member's turn.
#[must_use]
pub fn build_envelope(message: &Message) -> String {
    let mut attributes = vec![
        format!("from=\"{}\"", escape_attribute_value(&message.from)),
        format!(
            "timestamp=\"{}\"",
            escape_attribute_value(&message.timestamp.to_string())
        ),
        format!(
            "messageId=\"{}\"",
            escape_attribute_value(&message.message_id)
        ),
        format!("kind=\"{}\"", escape_attribute_value(message.kind.as_str())),
        format!(
            "correlationId=\"{}\"",
            escape_attribute_value(message.correlation_id.as_deref().unwrap_or(""))
        ),
    ];
    if let Some(summary) = &message.summary {
        attributes.push(format!("summary=\"{}\"", escape_attribute_value(summary)));
    }
    if let Some(references) = &message.references {
        let encoded = serde_json::to_string(references).unwrap_or_default();
        attributes.push(format!(
            "references=\"{}\"",
            escape_attribute_value(&encoded)
        ));
    }
    format!(
        "<peer_message {}>\n{}\n</peer_message>",
        attributes.join(" "),
        message.body
    )
}

/// `pollAndBuildInjection`: claim unread, not-yet-pending messages for this turn.
pub fn poll_and_build_injection(
    session_id: &str,
    member_name: &str,
    team_run_id: &str,
    config: &TeamModeConfig,
    turn_marker: &str,
) -> Result<InjectionResult> {
    let unread_messages = list_unread_messages(team_run_id, member_name, config)?;
    let result: RefCell<Option<InjectionResult>> = RefCell::new(None);
    let missing_member: RefCell<bool> = RefCell::new(false);

    let transition = |mut state: RuntimeState| -> RuntimeState {
        let Some(member) = state
            .members
            .iter_mut()
            .find(|member| member.name == member_name)
        else {
            *missing_member.borrow_mut() = true;
            return state;
        };
        if member.last_injected_turn_marker.as_deref() == Some(turn_marker) {
            *result.borrow_mut() = Some(InjectionResult::skipped("already injected this turn"));
            return state;
        }
        let injectable: Vec<&Message> = unread_messages
            .iter()
            .filter(|message| {
                !member
                    .pending_injected_message_ids
                    .contains(&message.message_id)
            })
            .collect();
        if injectable.is_empty() {
            let reason = if member.pending_injected_message_ids.is_empty() {
                "no unread"
            } else {
                "pending ack"
            };
            *result.borrow_mut() = Some(InjectionResult::skipped(reason));
            return state;
        }
        let message_ids: Vec<String> = injectable
            .iter()
            .map(|message| message.message_id.clone())
            .collect();
        let envelopes: Vec<String> = injectable
            .iter()
            .map(|message| build_envelope(message))
            .collect();
        member.last_injected_turn_marker = Some(turn_marker.to_owned());
        for message_id in &message_ids {
            if !member.pending_injected_message_ids.contains(message_id) {
                member.pending_injected_message_ids.push(message_id.clone());
            }
        }
        *result.borrow_mut() = Some(InjectionResult {
            injected: true,
            content: Some(envelopes.join("\n")),
            message_ids,
            reason: None,
        });
        state
    };

    // A missing member throws inside the TS transition, aborting the save.
    let outcome = transition_runtime_state(team_run_id, transition, config);
    if *missing_member.borrow() {
        return Err(TeamCoreError::message(format!(
            "runtime member not found for session {session_id}: {member_name}"
        )));
    }
    outcome?;
    result.into_inner().ok_or_else(|| {
        TeamCoreError::message(format!(
            "mailbox injection claim failed for session {session_id}: {member_name}"
        ))
    })
}
