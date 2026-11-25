//! Consumed sender target verification (`thread/tools/reply-target.ts`).

use serde_json::json;

use super::super::addressing::ThreadAddressEntry;
use super::super::errors::{ThreadToolFailure, thread_tool_failure};
use super::reply_permission::{ReplySender, consumed_senders_of};

pub fn other_device<'a>(device: Option<&'a str>, here: Option<&str>) -> Option<&'a str> {
    match (device, here) {
        (Some(d), Some(h)) if d == h => None,
        _ => device,
    }
}

pub fn reply_target_mismatch(
    reply_targets: &[String],
    reply_senders: &[ReplySender],
    address: &str,
    entry: &ThreadAddressEntry,
    here: Option<&str>,
) -> Option<ThreadToolFailure> {
    let senders = consumed_senders_of(reply_targets, reply_senders, address);
    if senders.is_empty() {
        return None;
    }
    let entry_device = other_device(entry.device.as_deref(), here);
    let matches = senders.iter().any(|sender| {
        sender.session_id == entry.summary.thread_id
            && other_device(sender.device.as_deref(), here) == entry_device
    });
    if matches {
        None
    } else {
        Some(reply_target_failure(address, entry))
    }
}

pub fn reply_target_failure(address: &str, entry: &ThreadAddressEntry) -> ThreadToolFailure {
    let where_str = entry
        .device
        .as_deref()
        .map(|d| format!(" on {d}"))
        .unwrap_or_default();
    let details = match entry.device.as_deref() {
        Some(d) => json!({ "thread": entry.summary.thread_id, "device": d }),
        None => json!({ "thread": entry.summary.thread_id }),
    };
    thread_tool_failure(
        "reply_target_mismatch",
        format!(
            "\"{address}\" was approved as a reply to a sender of this run's deliveries, but it resolves to {} ({}{where_str}), which is not that sender.",
            entry.summary.name, entry.summary.thread_id
        ),
        "Reply with the sender's durable id from the delivery header, or send to the other thread by its id as a normal send.",
        Some(details),
    )
}
