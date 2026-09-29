use std::fs;
use std::io;

use crate::config::TeamModeConfig;
use crate::error::Result;
use crate::team_registry::paths::{get_inbox_dir, resolve_base_dir};

pub(crate) fn create_private_dir_all(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// `ackMessages`: move each message (unread or reserved) into `processed/`.
pub fn ack_messages(
    team_run_id: &str,
    member_name: &str,
    message_ids: &[String],
    config: &TeamModeConfig,
) -> Result<()> {
    let inbox_dir = get_inbox_dir(&resolve_base_dir(config), team_run_id, member_name)?;
    let processed_dir = inbox_dir.join("processed");
    create_private_dir_all(&processed_dir)?;
    for message_id in message_ids {
        let file_name = format!("{message_id}.json");
        let sources = [
            inbox_dir.join(&file_name),
            inbox_dir.join(format!(".delivering-{file_name}")),
        ];
        let target = processed_dir.join(&file_name);
        for source in sources {
            match fs::rename(&source, &target) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}
