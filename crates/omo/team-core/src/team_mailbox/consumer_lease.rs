use crate::config::TeamModeConfig;
use crate::error::Result;
use crate::team_registry::paths::{get_inbox_dir, resolve_base_dir};
use crate::team_state_store::locks::{LockOptions, with_lock};

use super::ack::create_private_dir_all;

#[derive(Debug, Clone, Copy)]
pub struct InboxConsumerLeaseOptions {
    pub stale_after_ms: i64,
}

/// `withInboxConsumerLease`: serialize consumers of one inbox via `.consumer.lock`.
pub fn with_inbox_consumer_lease<T>(
    team_run_id: &str,
    recipient: &str,
    config: &TeamModeConfig,
    run: impl FnOnce() -> Result<T>,
    options: InboxConsumerLeaseOptions,
) -> Result<T> {
    let inbox_dir = get_inbox_dir(&resolve_base_dir(config), team_run_id, recipient)?;
    create_private_dir_all(&inbox_dir)?;
    with_lock(
        &inbox_dir.join(".consumer.lock"),
        run,
        Some(&LockOptions {
            owner_tag: Some(format!("team-mailbox-consumer:{recipient}")),
            stale_after_ms: Some(options.stale_after_ms),
        }),
    )
}
