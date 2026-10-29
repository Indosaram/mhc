use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::config::TeamModeConfig;
use crate::error::Result;
use crate::logger::log;
use crate::team_registry::paths::{get_inbox_dir, resolve_base_dir};
use crate::types::Message;

use super::reservation::DeliveryReservation;

fn reservation_paths(inbox_dir: &Path, message_id: &str) -> DeliveryReservation {
    let processed_dir = inbox_dir.join("processed");
    DeliveryReservation {
        reserved_path: inbox_dir.join(format!(".delivering-{message_id}.json")),
        inbox_path: inbox_dir.join(format!("{message_id}.json")),
        processed_path: processed_dir.join(format!("{message_id}.json")),
        processed_dir,
    }
}

/// Read one unread message by ID, whether it is still in the inbox or reserved for delivery.
pub fn read_unread_message_by_id(
    team_run_id: &str,
    member_name: &str,
    message_id: &str,
    config: &TeamModeConfig,
) -> Result<Option<Message>> {
    let inbox_dir = get_inbox_dir(&resolve_base_dir(config), team_run_id, member_name)?;
    let reservation = reservation_paths(&inbox_dir, message_id);
    let mut contents = None;
    let mut file_name = format!("{message_id}.json");
    for candidate in [&reservation.inbox_path, &reservation.reserved_path] {
        match fs::read_to_string(candidate) {
            Ok(content) => {
                contents = Some(content);
                file_name = candidate
                    .file_name()
                    .map_or_else(|| file_name.clone(), |name| name.to_string_lossy().into_owned());
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                log(
                    "team mailbox exact message read failed",
                    Some(json!({
                        "event": "team-mailbox-exact-message-read-failed",
                        "memberName": member_name,
                        "teamRunId": team_run_id,
                        "fileName": candidate.file_name().map(|name| name.to_string_lossy()),
                        "error": error.to_string(),
                    })),
                );
                return Err(error.into());
            }
        }
    }
    let Some(content) = contents else {
        return Ok(None);
    };
    let parsed = serde_json::from_str::<Value>(&content)
        .map_err(|error| error.to_string())
        .and_then(|raw| Message::safe_parse(&raw).map_err(|issues| issues.to_string()));
    match parsed {
        Ok(message) => Ok(Some(message)),
        Err(error) => {
            log(
                "team mailbox skipped malformed exact message",
                Some(json!({
                    "event": "team-mailbox-malformed-exact-message",
                    "memberName": member_name,
                    "teamRunId": team_run_id,
                    "fileName": file_name,
                    "error": error,
                })),
            );
            Ok(None)
        }
    }
}

fn read_inbox_message(
    inbox_dir: &Path,
    file_name: &str,
    member_name: &str,
    team_run_id: &str,
) -> Option<Message> {
    let parsed = fs::read_to_string(inbox_dir.join(file_name))
        .map_err(|error| error.to_string())
        .and_then(|content| {
            serde_json::from_str::<Value>(&content).map_err(|error| error.to_string())
        });
    match parsed {
        Ok(raw) => match Message::safe_parse(&raw) {
            Ok(message) => Some(message),
            Err(issues) => {
                let issues: Vec<Value> = issues
                    .0
                    .iter()
                    .map(|issue| json!({ "path": issue.path, "message": issue.message }))
                    .collect();
                log(
                    "team mailbox skipped malformed message",
                    Some(json!({
                        "event": "team-mailbox-malformed-message",
                        "memberName": member_name,
                        "teamRunId": team_run_id,
                        "fileName": file_name,
                        "issues": issues,
                    })),
                );
                None
            }
        },
        Err(error) => {
            log(
                "team mailbox skipped unreadable message",
                Some(json!({
                    "event": "team-mailbox-unreadable-message",
                    "memberName": member_name,
                    "teamRunId": team_run_id,
                    "fileName": file_name,
                    "error": error,
                })),
            );
            None
        }
    }
}

/// `listUnreadMessages`: valid unread messages ordered by timestamp; malformed files are logged.
pub fn list_unread_messages(
    team_run_id: &str,
    member_name: &str,
    config: &TeamModeConfig,
) -> Result<Vec<Message>> {
    let inbox_dir = get_inbox_dir(&resolve_base_dir(config), team_run_id, member_name)?;
    let entries = match fs::read_dir(&inbox_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut file_names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_file() && name.ends_with(".json") && !name.starts_with('.') {
            file_names.push(name);
        }
    }
    file_names.sort();
    let mut messages: Vec<Message> = file_names
        .iter()
        .filter_map(|name| read_inbox_message(&inbox_dir, name, member_name, team_run_id))
        .collect();
    messages.sort_by_key(|message| message.timestamp);
    Ok(messages)
}
