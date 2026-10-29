use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use crate::config::TeamModeConfig;
use crate::error::{Result, TeamCoreError};
use crate::team_registry::paths::{get_inbox_dir, resolve_base_dir};
use crate::team_state_store::locks::{LockOptions, atomic_write, with_lock};
use crate::team_state_store::store::load_runtime_state;
use crate::types::{Message, RuntimeStatus};

use super::ack::create_private_dir_all;

/// `SendContext`.
#[derive(Debug, Clone, Default)]
pub struct SendContext {
    pub is_lead: bool,
    pub active_members: Vec<String>,
    pub lead_recipient: Option<String>,
    pub reserved_recipients: Option<HashSet<String>>,
}

/// `sendMessage`'s `{ messageId, deliveredTo }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendResult {
    pub message_id: String,
    pub delivered_to: Vec<String>,
}

fn assert_team_accepts_messages(team_run_id: &str, config: &TeamModeConfig) -> Result<()> {
    match load_runtime_state(team_run_id, config) {
        Ok(state)
            if matches!(
                state.status,
                RuntimeStatus::Deleting | RuntimeStatus::Deleted
            ) =>
        {
            Err(TeamCoreError::TeamDeleting)
        }
        Ok(_) => Ok(()),
        Err(error) if error.is_not_found() => Ok(()),
        Err(error) => Err(error),
    }
}

fn resolve_recipients(message: &Message, context: &SendContext) -> Result<Vec<String>> {
    if message.to != "*" {
        let allowed = context.active_members.contains(&message.to)
            || context
                .reserved_recipients
                .as_ref()
                .is_some_and(|reserved| reserved.contains(&message.to))
            || context.lead_recipient.as_deref() == Some(message.to.as_str());
        if !allowed {
            return Err(TeamCoreError::InvalidRecipient(message.to.clone()));
        }
        return Ok(vec![message.to.clone()]);
    }
    let mut seen = HashSet::new();
    Ok(context
        .active_members
        .iter()
        .filter(|member| seen.insert((*member).clone()))
        .cloned()
        .collect())
}

fn get_unread_size_bytes(inbox_dir: &Path) -> Result<u64> {
    let entries = match fs::read_dir(inbox_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut total = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_file() || !name.ends_with(".json") {
            continue;
        }
        if name.starts_with(".delivering-") || !name.starts_with('.') {
            total += fs::metadata(inbox_dir.join(&name))?.len();
        }
    }
    Ok(total)
}

fn file_exists(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// `sendMessage`: deliver to one recipient or broadcast (`to: "*"`, lead only), enforcing the
/// payload cap, recipient backpressure and message-id uniqueness under the per-inbox lock.
pub fn send_message(
    message: &Message,
    team_run_id: &str,
    config: &TeamModeConfig,
    context: &SendContext,
) -> Result<SendResult> {
    let serialized = format!(
        "{}\n",
        serde_json::to_string_pretty(message)
            .map_err(|error| TeamCoreError::message(error.to_string()))?
    );
    let serialized_bytes = u64::try_from(serialized.len()).unwrap_or(u64::MAX);
    let payload_bytes = i64::try_from(message.body.len()).unwrap_or(i64::MAX);
    if payload_bytes > config.message_payload_max_bytes {
        return Err(TeamCoreError::PayloadTooLarge);
    }
    assert_team_accepts_messages(team_run_id, config)?;
    if message.to == "*" && !context.is_lead {
        return Err(TeamCoreError::BroadcastNotPermitted);
    }
    let base_dir = resolve_base_dir(config);
    let mut delivered_to = Vec::new();
    let max_unread = u64::try_from(config.recipient_unread_max_bytes).unwrap_or(0);

    for recipient in resolve_recipients(message, context)? {
        let inbox_dir = get_inbox_dir(&base_dir, team_run_id, &recipient)?;
        create_private_dir_all(&inbox_dir)?;
        let mut lock_path = inbox_dir.clone().into_os_string();
        lock_path.push(".lock");
        with_lock(
            Path::new(&lock_path),
            || {
                if get_unread_size_bytes(&inbox_dir)? + serialized_bytes > max_unread {
                    return Err(TeamCoreError::RecipientBackpressure);
                }
                let unreserved = inbox_dir.join(format!("{}.json", message.message_id));
                let reserved = inbox_dir.join(format!(".delivering-{}.json", message.message_id));
                if file_exists(&unreserved)? || file_exists(&reserved)? {
                    return Err(TeamCoreError::DuplicateMessageId);
                }
                let use_reserved = context.lead_recipient.as_deref() != Some(recipient.as_str())
                    && context
                        .reserved_recipients
                        .as_ref()
                        .is_some_and(|reserved| reserved.contains(&recipient));
                atomic_write(
                    if use_reserved { &reserved } else { &unreserved },
                    &serialized,
                )?;
                Ok(())
            },
            Some(&LockOptions::owner(format!("team-mailbox:{recipient}"))),
        )?;
        delivered_to.push(recipient);
    }
    Ok(SendResult {
        message_id: message.message_id.clone(),
        delivered_to,
    })
}
