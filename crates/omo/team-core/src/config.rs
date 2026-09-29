//! Team-mode configuration (`TeamModeConfigSchema`).

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamModeConfig {
    pub enabled: bool,
    pub tmux_visualization: bool,
    pub max_parallel_members: i64,
    pub max_members: i64,
    pub max_messages_per_run: i64,
    pub max_wall_clock_minutes: i64,
    pub max_member_turns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
    pub message_payload_max_bytes: i64,
    pub recipient_unread_max_bytes: i64,
    pub mailbox_poll_interval_ms: i64,
}

impl Default for TeamModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tmux_visualization: false,
            max_parallel_members: 4,
            max_members: 8,
            max_messages_per_run: 10_000,
            max_wall_clock_minutes: 120,
            max_member_turns: 500,
            base_dir: None,
            message_payload_max_bytes: 32_768,
            recipient_unread_max_bytes: 262_144,
            mailbox_poll_interval_ms: 3_000,
        }
    }
}

fn read_int(
    object: &serde_json::Map<String, Value>,
    key: &str,
    min: i64,
    max: Option<i64>,
    default: i64,
) -> Result<i64, String> {
    let Some(value) = object.get(key) else {
        return Ok(default);
    };
    let number =
        crate::types::as_integer(value).ok_or_else(|| format!("{key}: expected an integer"))?;
    if number < min || max.is_some_and(|max| number > max) {
        return Err(format!("{key}: out of range"));
    }
    Ok(number)
}

fn read_bool(object: &serde_json::Map<String, Value>, key: &str) -> Result<bool, String> {
    match object.get(key) {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(format!("{key}: expected a boolean")),
    }
}

impl TeamModeConfig {
    /// `TeamModeConfigSchema.parse`: apply defaults and range checks to raw config JSON.
    pub fn parse(raw: &Value) -> Result<Self, String> {
        let object = raw
            .as_object()
            .ok_or_else(|| "team mode config must be an object".to_owned())?;
        let base_dir = match object.get("base_dir") {
            None => None,
            Some(Value::String(dir)) => Some(dir.clone()),
            Some(_) => return Err("base_dir: expected a string".to_owned()),
        };
        Ok(Self {
            enabled: read_bool(object, "enabled")?,
            tmux_visualization: read_bool(object, "tmux_visualization")?,
            max_parallel_members: read_int(object, "max_parallel_members", 1, Some(8), 4)?,
            max_members: read_int(object, "max_members", 1, Some(8), 8)?,
            max_messages_per_run: read_int(object, "max_messages_per_run", 1, None, 10_000)?,
            max_wall_clock_minutes: read_int(object, "max_wall_clock_minutes", 1, None, 120)?,
            max_member_turns: read_int(object, "max_member_turns", 1, None, 500)?,
            base_dir,
            message_payload_max_bytes: read_int(
                object,
                "message_payload_max_bytes",
                1024,
                None,
                32_768,
            )?,
            recipient_unread_max_bytes: read_int(
                object,
                "recipient_unread_max_bytes",
                1024,
                None,
                262_144,
            )?,
            mailbox_poll_interval_ms: read_int(
                object,
                "mailbox_poll_interval_ms",
                500,
                None,
                3_000,
            )?,
        })
    }

    /// Default config rooted at `base_dir`.
    #[must_use]
    pub fn with_base_dir(base_dir: impl Into<String>) -> Self {
        Self {
            base_dir: Some(base_dir.into()),
            ..Self::default()
        }
    }
}
