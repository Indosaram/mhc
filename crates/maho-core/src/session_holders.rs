//! The runtime's hold on its active session file (senpi `core/session-holders.ts` as used by
//! `agent-session-runtime.ts`: `holdActiveSession`, `wasFlushed`, and the `holderEndpoint` threading).
//!
//! The holder records, probes and live-holder reads live in `schedule::runner_lease` and
//! `session_holder_records`; this module is the runtime-facing seam over them.
pub use crate::schedule::runner_lease::SessionHolderEndpoint;
use crate::schedule::runner_lease::{SessionHold, hold_session_file_with_endpoint};
use crate::session_manager::SessionManager;
use serde_json::Value;

/// A persisted session is written to disk with its first assistant message, so one that has an assistant
/// message and no file was moved away after it was read (senpi `wasFlushed`).
pub fn was_flushed(session_manager: &SessionManager) -> bool {
    session_manager.entries().iter().any(is_assistant_message)
}

fn is_assistant_message(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("message")
        && entry
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            == Some("assistant")
}

/// senpi `holdActiveSession`: `None` for an in-memory session or one without a file. A hold that cannot be
/// taken (the file is being moved, or was moved away) is reported to the caller, never swallowed.
pub(crate) fn hold_active_session(
    session_manager: &SessionManager,
    expect_existing: bool,
    holder_endpoint: Option<&SessionHolderEndpoint>,
) -> Result<Option<SessionHold>, String> {
    let Some(session_file) = session_manager.session_file() else {
        return Ok(None);
    };
    if !session_manager.is_persisted() {
        return Ok(None);
    }
    hold_session_file_with_endpoint(
        std::path::Path::new(session_file),
        session_manager.session_id(),
        session_manager.cwd(),
        expect_existing,
        holder_endpoint,
    )
    .map(Some)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_an_assistant_message_counts_as_flushed() {
        assert!(is_assistant_message(
            &json!({"type":"message","message":{"role":"assistant"}})
        ));
        assert!(!is_assistant_message(
            &json!({"type":"message","message":{"role":"user"}})
        ));
        assert!(!is_assistant_message(
            &json!({"type":"custom","customType":"x"})
        ));
    }

    #[test]
    fn an_in_memory_session_takes_no_hold() {
        let manager = SessionManager::in_memory("/tmp", None, None);
        assert!(!was_flushed(&manager));
        assert!(matches!(
            hold_active_session(&manager, false, None),
            Ok(None)
        ));
    }

    #[test]
    fn a_persisted_session_is_held_with_the_host_endpoint_on_its_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("s.jsonl");
        let manager = SessionManager::in_memory(&temp.path().to_string_lossy(), None, None);
        // `in_memory` has no file: the seam must not invent one.
        assert!(manager.session_file().is_none());
        let endpoint = SessionHolderEndpoint {
            host_instance: Some("host-1".into()),
            host_socket: Some("/tmp/h.sock".into()),
            host_daemon_dir: None,
        };
        let hold = hold_session_file_with_endpoint(
            &file,
            "sess-1",
            &temp.path().to_string_lossy(),
            false,
            Some(&endpoint),
        )
        .expect("hold");
        let holders = crate::schedule::runner_lease::session_holders_dir(&file, "sess-1")
            .expect("holders dir");
        let record = std::fs::read_to_string(holders.join(format!("{}.json", std::process::id())))
            .expect("record");
        let record: Value = serde_json::from_str(&record).expect("json");
        assert_eq!(record["host_instance"], json!("host-1"));
        assert_eq!(record["host_socket"], json!("/tmp/h.sock"));
        assert!(record.get("host_daemon_dir").is_none());
        drop(hold);
    }
}
