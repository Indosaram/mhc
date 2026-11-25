//! Shared field constants & schema builders (`thread/contracts/fields.ts`).

use serde_json::{Value, json};

pub const THREAD_MESSAGE_MAX_BYTES: usize = 32_768;
pub const THREAD_SUMMARY_MAX_LENGTH: usize = 200;
pub const THREAD_READ_DEFAULT_BYTES: usize = 131_072;
pub const THREAD_READ_MAX_BYTES: usize = 1_048_576;
pub const DEFAULT_THREAD_DELIVERY: &str = "auto";
pub const REMOTE_GRACEFUL_MAX_WAIT_MS: u64 = 35_000;

pub const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn thinking_level_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "enum": THINKING_LEVELS,
        "description": description,
    })
}

pub fn thread_delivery_mode_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["auto", "steer", "follow_up"],
        "default": DEFAULT_THREAD_DELIVERY,
        "description": "Omitted: auto. auto steers the running turn when one is active and starts a new turn otherwise; steer requires expected_turn_id and becomes turn_conflict when the active turn changed; follow_up queues the message behind the running turn (pass it only when the message must wait for that turn to end).",
    })
}
