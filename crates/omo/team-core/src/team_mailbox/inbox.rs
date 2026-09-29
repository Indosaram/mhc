use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::config::TeamModeConfig;
use crate::error::Result;
use crate::logger::log;
use crate::team_registry::paths::{get_inbox_dir, resolve_base_dir};
use crate::types::Message;

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
