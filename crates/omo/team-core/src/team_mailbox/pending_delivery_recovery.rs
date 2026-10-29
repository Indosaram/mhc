use std::collections::HashSet;

use serde_json::json;

use crate::config::TeamModeConfig;
use crate::error::Result;
use crate::logger::log;
use crate::session_client::{TeamSessionClient, get_messages_data, value_contains_message_id};

use super::reservation::{release_delivery_reservation, reserve_message_for_delivery};

/// `findDeliveredMessageIds`: ids whose envelope appears in the recipient's session history.
/// Any failure yields an empty set (the loss-safe answer: callers requeue rather than ack).
#[must_use]
pub fn find_delivered_message_ids(
    client: &dyn TeamSessionClient,
    session_id: &str,
    message_ids: &[String],
) -> HashSet<String> {
    let mut delivered = HashSet::new();
    if message_ids.is_empty() {
        return delivered;
    }
    match client.messages(session_id) {
        None => {}
        Some(Ok(response)) => {
            let messages = get_messages_data(&response);
            for message_id in message_ids {
                if messages
                    .iter()
                    .any(|message| value_contains_message_id(message, message_id))
                {
                    delivered.insert(message_id.clone());
                }
            }
        }
        Some(Err(error)) => log(
            "[team-mailbox] failed to read session history for pending-delivery verification",
            Some(json!({ "sessionID": session_id, "error": error.message.unwrap_or_default() })),
        ),
    }
    delivered
}

/// `requeuePendingLiveDeliveries`: return reserved pending messages to the unread inbox.
pub fn requeue_pending_live_deliveries(
    team_run_id: &str,
    member_name: &str,
    message_ids: &[String],
    config: &TeamModeConfig,
) -> Result<()> {
    for message_id in message_ids {
        if let Some(reservation) =
            reserve_message_for_delivery(team_run_id, member_name, message_id, config)?
        {
            release_delivery_reservation(&reservation)?;
        }
    }
    Ok(())
}
