use std::fs;
use std::io;

use crate::config::TeamModeConfig;
use crate::error::Result;
use crate::team_registry::paths::{get_inbox_dir, resolve_base_dir};

/// `isMessageConsumed`: the message has been moved to `processed/`.
pub fn is_message_consumed(
    team_run_id: &str,
    recipient: &str,
    message_id: &str,
    config: &TeamModeConfig,
) -> Result<bool> {
    let inbox_dir = get_inbox_dir(&resolve_base_dir(config), team_run_id, recipient)?;
    match fs::metadata(
        inbox_dir
            .join("processed")
            .join(format!("{message_id}.json")),
    ) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
