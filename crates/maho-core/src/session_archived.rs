//! Port of senpi packages/coding-agent/src/core/session-archived.ts.

use serde_json::{Value, json};

use crate::session_manager::SessionManager;

pub const SESSION_ARCHIVED_CUSTOM_TYPE: &str = "session_archived";

/// The data of `entry` when it is a `customType` marker written for session `session_id`, else None.
pub fn session_marker_data<'a>(
    entry: &'a Value,
    custom_type: &str,
    session_id: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    if entry.get("type").and_then(Value::as_str) != Some("custom")
        || entry.get("customType").and_then(Value::as_str) != Some(custom_type)
    {
        return None;
    }
    let data = entry.get("data")?.as_object()?;
    if data.get("sessionId").and_then(Value::as_str) == Some(session_id) {
        Some(data)
    } else {
        None
    }
}

/// The flag `entry` sets for session `session_id`, or None when it sets none there.
pub fn archived_flag_of(entry: &Value, session_id: &str) -> Option<bool> {
    let data = session_marker_data(entry, SESSION_ARCHIVED_CUSTOM_TYPE, session_id)?;
    Some(data.get("archived").and_then(Value::as_bool) == Some(true))
}

pub fn is_session_archived(manager: &SessionManager) -> bool {
    manager.is_archived()
}

pub fn set_session_archived(manager: &mut SessionManager, archived: bool) {
    let session_id = manager.session_id().to_owned();
    manager.append_custom(
        SESSION_ARCHIVED_CUSTOM_TYPE,
        Some(json!({ "archived": archived, "sessionId": session_id })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_only_applies_to_matching_session_id() {
        let entry = json!({
            "type": "custom",
            "customType": "session_archived",
            "data": { "archived": true, "sessionId": "s1" }
        });
        assert_eq!(archived_flag_of(&entry, "s1"), Some(true));
        assert_eq!(archived_flag_of(&entry, "s2"), None);
    }
}
