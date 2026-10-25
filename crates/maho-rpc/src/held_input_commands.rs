use maho_core::agent_session::AgentSession;
use serde_json::{Value, json};

pub fn run_hold_input(
    session: &AgentSession,
    command: &Value,
    interactive_user: bool,
    task_parent: bool,
) -> Result<Value, String> {
    if !interactive_user {
        if !task_parent {
            return Err("hold_input_requires_interactive_client".into());
        }
        if !session.holds().iter().any(|hold| {
            hold.kind == maho_core::engine_turn_hold::HoldKind::InheritedHold
                && hold
                    .data
                    .as_ref()
                    .is_some_and(|data| data["parentSessionId"] == command["parentSessionId"])
        }) {
            return Err(
                "wrong_parent: this child is not paused for that parent session's run".into(),
            );
        }
    }
    let mut input = json!({
        "inputId": command["inputId"],
        "revision": command["revision"],
        "text": command["text"],
    });
    if let Some(images) = command.get("images") {
        input["images"] = images.clone();
    }
    let result = session.hold_input(input)?;
    match result["kind"].as_str() {
        Some("held") => Ok(json!({"entryId":result["entryId"],"duplicate":result["duplicate"]})),
        Some("already_delivered") => Ok(json!({"alreadyDelivered":true})),
        _ => Err(format!(
            "input_id_conflict: input {} is held at revision {}",
            command["inputId"].as_str().unwrap_or_default(),
            result["heldRevision"]
        )),
    }
}
